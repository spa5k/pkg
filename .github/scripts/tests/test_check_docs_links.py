#!/usr/bin/env python3
"""Focused unit tests for the repo docs link checker.

Scope of this file: prove that Markdown links hidden inside HTML comments are
masked before link extraction and before structural invariant checks, so a
commented-out link cannot satisfy a required active-plan link.

Standard library only. Every test runs in a private tempdir and cleans up after
itself — no residue is left in the repository working tree.

Run:    python3 -m unittest .github/scripts/tests/test_check_docs_links.py
        python3 -m unittest discover -s .github/scripts/tests -v
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

# Make the sibling check_docs_links module importable regardless of CWD.
_SCRIPTS_DIR = Path(__file__).resolve().parents[1]
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

import check_docs_links as cdl  # noqa: E402  (import after sys.path tweak)


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


class MaskHtmlCommentsUnitTests(unittest.TestCase):
    """Direct unit tests for the masking primitive itself."""

    def test_single_line_comment_is_blanked_length_preserved(self):
        text = "a [real](b.md) c <!-- [bad](missing.md) --> d"
        masked = cdl.mask_html_comments(text)
        # Length is identical (no characters added or removed).
        self.assertEqual(len(masked), len(text))
        # The visible link text survives; the commented one does not.
        self.assertIn("[real](b.md)", masked)
        self.assertNotIn("missing.md", masked)
        self.assertNotIn("<!--", masked)

    def test_multiline_comment_preserves_line_count_and_offsets(self):
        text = "line1\n<!-- open\n[bad](missing.md)\nclose -->\nline5\n"
        masked = cdl.mask_html_comments(text)
        # Same total length and identical newline positions => line numbers intact.
        self.assertEqual(len(masked), len(text))
        self.assertEqual(
            [i for i, ch in enumerate(masked) if ch == "\n"],
            [i for i, ch in enumerate(text) if ch == "\n"],
        )
        # Same number of lines, blanked interior, anchors preserved.
        self.assertEqual(masked.count("\n"), text.count("\n"))
        self.assertTrue(masked.splitlines()[0].startswith("line1"))
        self.assertTrue(masked.splitlines()[4].startswith("line5"))
        self.assertNotIn("missing.md", masked)

    def test_two_adjacent_comments_both_masked(self):
        text = "<!-- a --> [ok](b.md) <!-- [c](d.md) -->"
        masked = cdl.mask_html_comments(text)
        self.assertIn("[ok](b.md)", masked)
        self.assertNotIn("d.md", masked)


class CommentedLinksAreIgnoredTests(unittest.TestCase):
    """Requirement (a): a broken link inside an HTML comment is ignored."""

    def test_broken_link_in_single_line_comment_not_extracted(self):
        with tempfile.TemporaryDirectory() as d:
            src = Path(d) / "doc.md"
            _write(Path(d) / "target.md", "# Target\n")
            text = "<!-- [broken](does-not-exist.md) -->\n[ok](./target.md)\n"
            targets = [lnk.target for lnk in cdl.extract_links(src, text)]
        self.assertIn("./target.md", targets)
        self.assertNotIn("does-not-exist.md", targets)

    def test_broken_link_in_multiline_comment_not_extracted(self):
        with tempfile.TemporaryDirectory() as d:
            src = Path(d) / "doc.md"
            _write(Path(d) / "target.md", "# Target\n")
            text = (
                "<!-- start of comment\n"
                "[broken](missing.md)\n"
                "still commented -->\n"
                "[ok](./target.md)\n"
            )
            targets = [lnk.target for lnk in cdl.extract_links(src, text)]
        self.assertIn("./target.md", targets)
        self.assertNotIn("missing.md", targets)

    def test_commented_reference_definition_not_extracted(self):
        with tempfile.TemporaryDirectory() as d:
            src = Path(d) / "doc.md"
            _write(Path(d) / "target.md", "# Target\n")
            text = "<!-- [ref]: missing.md -->\n[ok](./target.md)\n"
            targets = [lnk.target for lnk in cdl.extract_links(src, text)]
        self.assertIn("./target.md", targets)
        self.assertNotIn("missing.md", targets)

    def test_comment_opener_in_fence_does_not_hide_visible_broken_link(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            src = root / "doc.md"
            text = (
                "```text\n"
                "<!-- literal code\n"
                "```\n"
                "[visible broken](missing.md)\n"
                "-->\n"
            )
            _write(src, text)

            targets = [lnk.target for lnk in cdl.extract_links(src, text)]
            self.assertIn("missing.md", targets)

            with patch.object(cdl, "REPO_ROOT", root):
                report = cdl.Report()
                cdl.check_links(report)

        self.assertFalse(report.ok)
        self.assertTrue(
            any(
                "missing.md" in error and "target not found" in error
                for error in report.errors
            )
        )


class StructuralInvariantTests(unittest.TestCase):
    """Requirements (b) and (c) exercised via _links_to in isolated temp repos.

    _links_to is the predicate behind the README and CONTRIBUTING active-plan
    invariants in check_structure. It shares the single
    extract_links choke point, so masking there covers both code paths.
    """

    def _make_repo(self, readme: str, contributing: str) -> Path:
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        root = Path(td.name)
        _write(root / "README.md", readme)
        _write(root / "CONTRIBUTING.md", contributing)
        _write(root / "plans" / "determinate-nix-stacked-prs.md", "# Active plan\n")
        return root

    # --- (b) a commented-only required link must NOT satisfy the invariant ---

    def test_commented_only_readme_plan_link_fails_invariant(self):
        root = self._make_repo(
            readme="# pkg\n<!-- [plan](plans/determinate-nix-stacked-prs.md) -->\n",
            contributing="# Contrib\n",
        )
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertFalse(
                cdl._links_to(root / "README.md", "plans/determinate-nix-stacked-prs.md")
            )

    def test_commented_only_contributing_plan_link_fails_invariant(self):
        root = self._make_repo(
            readme="# pkg\n",
            contributing="# Contrib\n<!-- [plan](plans/determinate-nix-stacked-prs.md) -->\n",
        )
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertFalse(
                cdl._links_to(root / "CONTRIBUTING.md", "plans/determinate-nix-stacked-prs.md")
            )

    def test_multiline_commented_only_plan_link_fails_invariant(self):
        readme = (
            "# pkg\n"
            "<!-- a multi-line note\n"
            "[plan](plans/determinate-nix-stacked-prs.md)\n"
            "end note -->\n"
        )
        root = self._make_repo(readme=readme, contributing="# Contrib\n")
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertFalse(
                cdl._links_to(root / "README.md", "plans/determinate-nix-stacked-prs.md")
            )

    # --- (c) the existing visible links still satisfy the invariant --------

    def test_visible_readme_plan_link_satisfies_invariant(self):
        root = self._make_repo(
            readme="# pkg\n[plan](plans/determinate-nix-stacked-prs.md)\n",
            contributing="# Contrib\n",
        )
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertTrue(
                cdl._links_to(root / "README.md", "plans/determinate-nix-stacked-prs.md")
            )

    def test_visible_contributing_plan_link_satisfies_invariant(self):
        root = self._make_repo(
            readme="# pkg\n",
            contributing="# Contrib\n[plan](plans/determinate-nix-stacked-prs.md)\n",
        )
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertTrue(
                cdl._links_to(root / "CONTRIBUTING.md", "plans/determinate-nix-stacked-prs.md")
            )

    def test_visible_link_beats_commented_broken_link(self):
        # A real visible invariant link plus a commented broken link: the
        # invariant is satisfied AND the commented broken link is not extracted.
        readme = (
            "# pkg\n"
            "<!-- [broken](missing.md) -->\n"
            "[plan](plans/determinate-nix-stacked-prs.md)\n"
        )
        root = self._make_repo(readme=readme, contributing="# Contrib\n")
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertTrue(
                cdl._links_to(root / "README.md", "plans/determinate-nix-stacked-prs.md")
            )
        targets = [lnk.target for lnk in cdl.extract_links(root / "README.md", readme)]
        self.assertIn("plans/determinate-nix-stacked-prs.md", targets)
        self.assertNotIn("missing.md", targets)


class IterMarkdownFilesDiscoveryTests(unittest.TestCase):
    """Regression: Markdown discovery must skip `.git` and Rust `target`.

    After `cargo doc` runs, `target/doc/static.files/*.md` contains generated
    license Markdown. Previously `iter_markdown_files` only excluded `.git`, so
    the checker scanned generated build output and over-counted the repo's
    Markdown set (19 instead of 18). Discovery must now skip both trees while
    still surfacing normal root- and `plans/`-level Markdown.
    """

    def test_skips_git_and_target_keeps_repo_markdown(self):
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        root = Path(td.name)

        # Author-owned Markdown that must be discovered.
        _write(root / "README.md", "# pkg\n")
        _write(root / "plans" / "determinate-nix-stacked-prs.md", "# Active plan\n")
        # Generated / ignored trees that must NOT be discovered.
        _write(
            root
            / "target"
            / "doc"
            / "static.files"
            / "SourceSerif4-LICENSE-a2cfd9d5.md",
            "# License\n",
        )
        _write(root / ".git" / "notes" / "commentary.md", "# git notes\n")

        with patch.object(cdl, "REPO_ROOT", root):
            found = {
                p.relative_to(root).as_posix()
                for p in cdl.iter_markdown_files()
            }

        # Generated/VCS Markdown is excluded; repo Markdown survives.
        self.assertIn("README.md", found)
        self.assertIn("plans/determinate-nix-stacked-prs.md", found)
        self.assertNotIn(
            "target/doc/static.files/SourceSerif4-LICENSE-a2cfd9d5.md", found
        )
        self.assertNotIn(".git/notes/commentary.md", found)
        # Exactly the two author-owned files and nothing else.
        self.assertEqual(
            found, {"README.md", "plans/determinate-nix-stacked-prs.md"}
        )

    def test_ignored_match_is_exact_component_not_substring(self):
        # A path component that merely *contains* the ignored name (e.g.
        # `my-target`, `target-report`) is still author-owned and stays in
        # scope; only a literal `target` or `.git` directory is skipped.
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        root = Path(td.name)
        _write(root / "my-target" / "notes.md", "# notes\n")
        _write(root / "target-report" / "q2.md", "# q2\n")

        with patch.object(cdl, "REPO_ROOT", root):
            found = {
                p.relative_to(root).as_posix()
                for p in cdl.iter_markdown_files()
            }

        self.assertEqual(found, {"my-target/notes.md", "target-report/q2.md"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
