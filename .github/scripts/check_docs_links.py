#!/usr/bin/env python3
"""Repository-local docs link checker for `pkg` documentation.

Pure Python 3 standard library — no third-party dependencies, no network.

What it validates
-----------------
1. Every **local** Markdown link (inline `[text](target)`, images `![alt](target)`,
   angle-bracket `[text](<target>)`, and reference definitions `[id]: target`)
   in every `.md` file under the repo root:
     * the target file exists (resolved relative to the linking file;
       a leading `/` is repo-root-relative, matching GitHub);
     * the fragment (`#slug`) resolves to a heading in the target Markdown file
       using GitHub-compatible heading slugs (including duplicate-heading
       `-N` suffixes);
     * the target does **not** escape the repository root.
2. External schemes (`http:`, `https:`, `ftp:`, `mailto:`, ...) and autolinks
   are ignored — they are out of scope for a repo-local checker.
3. Structural plan invariants:
     * `plans/README.md` and the active stacked-PR plan exist;
     * `README.md` and `CONTRIBUTING.md` link the active plan.

Exit code is 0 only when every check passes; otherwise errors are aggregated and
printed with file + (approximate) reason, and the process exits 1.

Run locally:  python3 .github/scripts/check_docs_links.py
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from typing import Dict, List, Set, Tuple

# Repo root = the directory three levels up from this script
# (.github/scripts/check_docs_links.py -> repo root).
REPO_ROOT: Path = Path(__file__).resolve().parents[2]

# Path components that mark trees the checker must never scan. Files living
# under any directory named here are excluded from Markdown discovery: `.git`
# is VCS metadata, and `target` is Rust build output (notably the generated
# `target/doc/static.files/*.md` license files emitted by `cargo doc`, which
# otherwise inflate the author-owned Markdown count). Matched as exact path
# components, never substrings, so e.g. `my-target/` stays in scope.
IGNORED_DIR_COMPONENTS: Set[str] = {".git", "target"}

# The plan index names one active implementation plan.
REQUIRED_PLANS: List[str] = [
    "plans/README.md",
    "plans/determinate-nix-stacked-prs.md",
]

# ----- Markdown parsing helpers ------------------------------------------------

# Matches a fenced code block opening/closing fence (``` or ~~~, 3+ chars).
FENCE_OPEN_RE = re.compile(r"^[ \t]{0,3}(`{3,}|~{3,})")

# ATX heading: 1-6 '#' then text; trailing '#' decoration is stripped.
ATX_HEADING_RE = re.compile(r"^[ \t]{0,3}(#{1,6})[ \t]+(.*?)[ \t]*#*[ \t]*$")

# Inline code span (single backtick pair) — blanked out before link scanning
# so link-shaped text inside `code` is ignored.
INLINE_CODE_RE = re.compile(r"`[^`]*`")

# Inline link / image, plain form:   [label](target)   or   ![alt](target)
# Optional ' "title"' suffix is allowed.
LINK_PLAIN_RE = re.compile(
    r"!?\[([^\]]*)\]\(\s*([^)\s>]+)(?:\s+\"[^\"]*\")?\s*\)"
)
# Inline link / image, angle-bracket form:   [label](<target>)
LINK_ANGLE_RE = re.compile(r"!?\[([^\]]*)\]\(\s*<([^>]*)>\s*\)")

# Reference-style link definition:  [id]: <target>  | [id]: target  "title"
# Footnote definitions ([^id]: ...) are deliberately excluded.
REF_DEF_RE = re.compile(
    r"^[ \t]{0,3}\[([^\]^][^\]]*)\]:[ \t]+(?:<([^>]*)>|(\S+))(?:[ \t]+\"[^\"]*\")?[ \t]*$"
)

# A target with an RFC-3986-style scheme such as https:, mailto:, ftp:.
SCHEME_RE = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.\-]*:")

# GitHub-slugger punctuation set: these characters are stripped before slug
# formation (letters, digits, underscore, space, and hyphen are kept).
SLUG_STRIP_RE = re.compile(
    r"[\u2000-\u206F\u2E00-\u2E7F\\'!\"#$%&()*+,./:;<=>?@\[\]^`{|}~]"
)


def slugify(text: str) -> str:
    """Return the GitHub-compatible anchor id for a heading."""
    s = text.lower()
    s = SLUG_STRIP_RE.sub("", s)
    s = re.sub(r"\s", "-", s)
    return s


def collect_heading_slugs(text: str) -> Set[str]:
    """Return the set of valid heading anchors for a Markdown document.

    Fenced code blocks are skipped so commented lines like `# foo` inside a
    code fence are not mistaken for headings. Duplicate headings get the
    github-slugger `-N` suffix treatment.
    """
    slugs: Set[str] = set()
    seen: Dict[str, int] = {}
    in_fence = False
    fence_char = ""
    for line in text.splitlines():
        m = FENCE_OPEN_RE.match(line)
        if m:
            tok = m.group(1)[0]
            if not in_fence:
                in_fence = True
                fence_char = tok
            elif tok == fence_char:
                in_fence = False
                fence_char = ""
            continue
        if in_fence:
            continue
        hm = ATX_HEADING_RE.match(line)
        if not hm:
            continue
        slug = slugify(hm.group(2))
        if not slug:
            continue
        if slug in seen:
            seen[slug] += 1
            slugs.add(f"{slug}-{seen[slug]}")
        else:
            seen[slug] = 0
            slugs.add(slug)
    return slugs


def iter_markdown_files() -> List[Path]:
    """All author-owned `.md` files under the repo root.

    The repo root is rglob-scanned for Markdown, but anything inside a
    directory listed in :data:`IGNORED_DIR_COMPONENTS` is skipped. That keeps
    the scanned set equal to the author-owned Markdown rather than including
    VCS metadata (`.git`) or Rust build output (`target`, e.g. the
    `target/doc/static.files/*.md` license files produced by `cargo doc`).
    """
    out: List[Path] = []
    for p in sorted(REPO_ROOT.rglob("*.md")):
        if IGNORED_DIR_COMPONENTS.intersection(p.parts):
            continue
        out.append(p)
    return out


def mask_html_comments(text: str) -> str:
    """Blank comments outside fenced code while preserving offsets.

    Every character of an `<!-- ... -->` comment (including any Markdown links
    inside it) is replaced with a space; newlines are preserved so that line
    numbers in downstream error messages stay accurate. Comment markers inside
    fenced code are literal code and do not start or end a comment.
    """
    out: List[str] = []
    in_comment = False
    in_fence = False
    fence_char = ""

    for raw in text.splitlines(keepends=True):
        if not in_comment:
            fence = FENCE_OPEN_RE.match(raw)
            if fence:
                token = fence.group(1)[0]
                if not in_fence:
                    in_fence = True
                    fence_char = token
                elif token == fence_char:
                    in_fence = False
                    fence_char = ""
                out.append(raw)
                continue
        if in_fence:
            out.append(raw)
            continue

        masked = list(raw)
        cursor = 0
        while cursor < len(raw):
            if in_comment:
                end = raw.find("-->", cursor)
                stop = len(raw) if end < 0 else end + 3
                for index in range(cursor, stop):
                    if masked[index] not in "\r\n":
                        masked[index] = " "
                cursor = stop
                if end < 0:
                    break
                in_comment = False
            else:
                start = raw.find("<!--", cursor)
                if start < 0:
                    break
                cursor = start
                in_comment = True
        out.append("".join(masked))

    return "".join(out)


def strip_inline_code(line: str) -> str:
    """Blank out `inline code` so link regexes do not match inside it."""
    return INLINE_CODE_RE.sub(lambda m: " " * len(m.group(0)), line)


# ----- Link model --------------------------------------------------------------


class Link:
    __slots__ = ("source", "target", "kind")

    def __init__(self, source: Path, target: str, kind: str):
        self.source = source  # absolute path of the file containing the link
        self.target = target  # raw target as written in markdown
        self.kind = kind  # "inline" | "ref-def"


def extract_links(path: Path, text: str) -> List[Link]:
    """Extract all local link targets from a Markdown file.

    HTML comments are masked first (see :func:`mask_html_comments`) so that
    Markdown links hidden inside a comment are neither extracted here nor able
    to satisfy the structural invariants checked via :func:`_links_to`. Because
    masking preserves newlines, line numbers in error messages remain accurate.
    """
    text = mask_html_comments(text)
    links: List[Link] = []
    in_fence = False
    fence_char = ""
    for raw in text.splitlines():
        # Track fenced code blocks; never scan their contents.
        m = FENCE_OPEN_RE.match(raw)
        if m:
            tok = m.group(1)[0]
            if not in_fence:
                in_fence = True
                fence_char = tok
            elif tok == fence_char:
                in_fence = False
                fence_char = ""
            continue
        if in_fence:
            continue
        line = strip_inline_code(raw)

        # Reference definitions (validate the target, like an inline link).
        rdm = REF_DEF_RE.match(line)
        if rdm:
            target = rdm.group(2) if rdm.group(2) is not None else rdm.group(3)
            if target:
                links.append(Link(path, target, "ref-def"))

        for tm in LINK_ANGLE_RE.finditer(line):
            links.append(Link(path, tm.group(2), "inline"))
        for tm in LINK_PLAIN_RE.finditer(line):
            links.append(Link(path, tm.group(2), "inline"))
    return links


def is_external(target: str) -> bool:
    """True for schemes this checker intentionally ignores (http, mailto, ...)."""
    return bool(SCHEME_RE.match(target))


def split_target(target: str) -> Tuple[str, str]:
    """Split a target into (path_part, fragment). fragment is '' if absent."""
    path_part, sep, fragment = target.partition("#")
    return path_part, (fragment if sep else "")


def within_repo(resolved: Path) -> bool:
    """True if `resolved` is inside the repository root."""
    try:
        resolved.resolve().relative_to(REPO_ROOT.resolve())
        return True
    except ValueError:
        return False


# ----- Cache of heading slugs per markdown file --------------------------------

_slug_cache: Dict[Path, Set[str]] = {}


def heading_slugs_for(path: Path) -> Set[str]:
    if path not in _slug_cache:
        try:
            _slug_cache[path] = collect_heading_slugs(path.read_text(encoding="utf-8"))
        except OSError:
            _slug_cache[path] = set()
    return _slug_cache[path]


# ----- Reporting ---------------------------------------------------------------


class Report:
    def __init__(self) -> None:
        self.errors: List[str] = []
        self.links_checked = 0
        self.files_scanned = 0

    def error(self, file: Path, target: str, message: str) -> None:
        rel = file.relative_to(REPO_ROOT) if within_repo(file) else file
        self.errors.append(f"- {rel}: link `{target}` -> {message}")

    def fail(self, message: str) -> None:
        self.errors.append(f"- {message}")

    @property
    def ok(self) -> bool:
        return not self.errors


# ----- Core checks -------------------------------------------------------------


def check_link(report: Report, link: Link) -> None:
    target = link.target.strip()
    if not target:
        report.error(link.source, target, "empty link target")
        return

    # Ignore external schemes and mailto/autolinks entirely.
    if is_external(target):
        return

    # Home-relative or UNC paths are not allowed in this repo's docs.
    if target.startswith("~") or target.startswith("\\\\"):
        report.error(link.source, target, "repository-escaping path (home/UNC)")
        return

    path_part, fragment = split_target(target)

    # Pure-fragment link resolves against the current file's own headings.
    if path_part == "":
        if fragment == "":
            report.error(link.source, target, "empty fragment")
            return
        if link.source.suffix == ".md" and fragment not in heading_slugs_for(link.source):
            report.error(link.source, target, f"fragment `#{fragment}` not found in this file")
        report.links_checked += 1
        return

    # Resolve the file path. Leading '/' is repo-root-relative (GitHub rule).
    if path_part.startswith("/"):
        base = REPO_ROOT
        rel = path_part.lstrip("/")
    else:
        base = link.source.parent
        rel = path_part

    # Strip any query string a stray author might have appended.
    rel = rel.split("?", 1)[0]
    resolved = (base / rel)
    normalized = os.path.normpath(resolved)

    # Reject anything that escapes the repository root.
    norm_path = Path(normalized)
    if not within_repo(norm_path):
        report.error(link.source, target, "repository-escaping path")
        return

    if not norm_path.exists():
        report.error(link.source, target, f"target not found (`{normalized}`)")
        return

    report.links_checked += 1

    # Fragment validation only for Markdown targets we can read headings from.
    if fragment and norm_path.suffix == ".md":
        if fragment not in heading_slugs_for(norm_path):
            report.error(
                link.source,
                target,
                f"fragment `#{fragment}` not found in {norm_path.relative_to(REPO_ROOT)}",
            )


def check_links(report: Report) -> None:
    for path in iter_markdown_files():
        report.files_scanned += 1
        try:
            text = path.read_text(encoding="utf-8")
        except OSError as exc:
            report.error(path, "", f"could not read file: {exc}")
            continue
        for link in extract_links(path, text):
            check_link(report, link)


def check_structure(report: Report) -> None:
    for rel in REQUIRED_PLANS:
        if not (REPO_ROOT / rel).is_file():
            report.fail(f"required plan missing: {rel}")

    readme = REPO_ROOT / "README.md"
    if not readme.is_file():
        report.fail("required file missing: README.md")
    elif not _links_to(readme, "plans/determinate-nix-stacked-prs.md"):
        report.fail(
            "README.md must link the active plan "
            "(plans/determinate-nix-stacked-prs.md)"
        )

    contributing = REPO_ROOT / "CONTRIBUTING.md"
    if not contributing.is_file():
        report.fail("required file missing: CONTRIBUTING.md")
    elif not _links_to(contributing, "plans/determinate-nix-stacked-prs.md"):
        report.fail(
            "CONTRIBUTING.md must link the active plan "
            "(plans/determinate-nix-stacked-prs.md)"
        )


def _links_to(path: Path, expected_repo_relative: str) -> bool:
    """True if `path` contains a local markdown link to `expected_repo_relative`."""
    expected = (REPO_ROOT / expected_repo_relative).resolve()
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return False
    for link in extract_links(path, text):
        if is_external(link.target):
            continue
        path_part, _ = split_target(link.target)
        if path_part == "":
            continue
        if path_part.startswith("/"):
            base = REPO_ROOT
            rel = path_part.lstrip("/")
        else:
            base = path.parent
            rel = path_part
        rel = rel.split("?", 1)[0]
        candidate = Path(os.path.normpath(base / rel))
        try:
            if candidate.resolve() == expected:
                return True
        except OSError:
            continue
    return False


# ----- Entry point -------------------------------------------------------------


def main() -> int:
    if not REPO_ROOT.is_dir():
        print(f"error: could not locate repo root from {__file__}", file=sys.stderr)
        return 2

    os.chdir(REPO_ROOT)
    report = Report()
    check_links(report)
    check_structure(report)

    print(
        f"Scanned {report.files_scanned} Markdown file(s); "
        f"validated {report.links_checked} local link(s)."
    )
    if report.ok:
        print("OK: all doc links and active-plan structural checks passed.")
        return 0

    print(f"\nFAILED: {len(report.errors)} problem(s):", file=sys.stderr)
    for e in report.errors:
        print(e, file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
