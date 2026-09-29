#!/usr/bin/env python3
"""Deterministic data-only fixture writer for public-tap import checks.

Writes one small synthetic tap tree per independent matrix case, the
intentional two-source collision pair, and a hash manifest from
matrix.json. Output is data only: no Ruby executes here, nothing fetches,
and nothing validates against a real Homebrew loader. Sandbox denial
proofs are not generated here: the parent runs controlled native Nix
probes and the backend runs the real reader-gate probe. Bounds come from
matrix.json policy; re-check commands are one-offs in README.md. The
output directory must be absolute and fresh or empty. Canonical parent
aliases (for example /tmp or /var on macOS) are resolved first; a leaf
symlink or a nonempty directory is refused.

Usage: make_fixtures.py --out DIR
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MATRIX = HERE / "matrix.json"
GENERATOR_VERSION = 4


def synth(label: str) -> str:
    """Deterministic synthetic sha256-style hex; never a real payload hash."""
    return hashlib.sha256(f"public-tap-check:{label}".encode()).hexdigest()


def cask(
    token: str, version: str, ext: str, name: str, desc: str, tail_text: str
) -> str:
    """Standard cask: banner, meta stanzas, then caller-supplied tail."""
    banner = (
        "# Synthetic public-tap-check fixture. Data only; never executed here.\n"
        f'# Expected result: see tools/public-tap-check/matrix.json "{token}".\n'
        f'cask "{token}" do\n'
        f'  version "{version}"\n'
        f'  sha256 "{synth(token + ":" + ext)}"\n'
        f'  url "https://example.com/pubtap/{token}/{token}-{version}.{ext}"\n'
        f'  name "{name}"\n'
        f'  desc "{desc} (synthetic fixture)"\n'
        f'  homepage "https://example.com/pubtap/{token}"\n'
    )
    return banner + tail_text + "end\n"


def tail(*lines: str) -> str:
    return "\n  ".join(("", *lines)) + "\n"


def helper_rb(version: str, comment: str) -> str:
    """Local tap helper; the version value is the whole-tree identity lever."""
    return (
        f"# {comment}\n"
        "module PubtapHelper\n"
        f'  VERSION = "{version}".freeze\n\n'
        "  def self.version\n    VERSION\n  end\nend\n"
    )


# ---- one small tree per independent case ----------------------------------
# A source-wide failure (duplicate token) aborts its whole tree. To keep
# that abort from hiding other cases, every independent case lives in its
# own tree. The only shared-token cases are the intentional collision pair
# (COLLISION_A/COLLISION_B) and the duplicate pair inside DUP.

RELEASE_ARCHIVE = {
    "Casks/r/pubtap-release-archive.rb": cask(
        "pubtap-release-archive",
        "1.2.3",
        "zip",
        "PubTap Release Archive",
        "Release archive with per-OS artifacts",
        tail(
            "on_macos do",
            '  app "PubTapReleaseArchive.app"',
            "end",
            "on_linux do",
            '  binary "bin/pubtap-release-archive"',
            "end",
            'zap trash: "~/.pubtap/pubtap-release-archive"',
        ),
    ),
}

RAW_BINARY = {
    "Casks/b/pubtap-raw-binary.rb": cask(
        "pubtap-raw-binary",
        "0.9.0",
        "bin",
        "PubTap Raw Binary",
        "Raw binary downloaded under its own basename, renamed on install",
        tail(
            "# the binary source is the download basename; the target is a deliberate rename",
            "on_macos do",
            '  binary "pubtap-raw-binary-0.9.0.bin", target: "pubtap-tool"',
            "end",
            "on_linux do",
            '  binary "pubtap-raw-binary-0.9.0.bin", target: "pubtap-tool"',
            "end",
        ),
    ),
}

APP_BUNDLE = {
    "Casks/a/pubtap-app-bundle.rb": cask(
        "pubtap-app-bundle",
        "3.1.4",
        "dmg",
        "PubTap App Bundle",
        "macOS app bundle in a dmg",
        tail("depends_on macos: :sonoma", 'app "PubTapBundle.app"'),
    ),
}


def control_cask(token: str, title: str) -> str:
    """Simple valid positive-control cask for a tree that also holds one
    intentionally failing record. The actual reader fails a whole export
    when every record fails, so each such tree needs one eligible record.
    Reserved host, synthetic checksum, plain binary artifacts per OS;
    metadata only, never executed here.
    """
    return cask(
        token,
        "1.0.0",
        "zip",
        title,
        "Valid positive control for its isolated tree",
        tail(
            "on_macos do",
            f'  binary "bin/{token}"',
            "end",
            "on_linux do",
            f'  binary "bin/{token}"',
            "end",
        ),
    )


def branches_rb() -> str:
    """OS blocks, arch interpolation, language blocks, and a local helper."""
    head = (
        "# OS/arch/language branches plus a local helper; the helper is part\n"
        "# of whole-tree identity (see helper-only-invalidation).\n"
        'require_relative "../../lib/pubtap_helper"\n'
        'cask "pubtap-branches" do\n'
        "  version PubtapHelper::VERSION\n"
        '  arch arm: "aarch64", intel: "x86_64"\n'
        '  sha256 arm: "'
        + synth("pubtap-branches:zip:arm")
        + '", intel: "'
        + synth("pubtap-branches:zip:intel")
        + '"\n'
        '  name "Branch Tool"\n'
        '  desc "OS, arch, and language branches (synthetic fixture)"\n'
        '  homepage "https://example.com/pubtap/pubtap-branches"\n'
    )
    languages = ""
    for code, default in (("en", ", default: true"), ("de", "")):
        languages += (
            f'  language "{code}"{default} do\n'
            f'    url "https://example.com/pubtap/pubtap-branches/#{{version}}'
            f'/branches-#{{arch}}-{code}.zip"\n'
            f'    "{code}"\n'
            "  end\n"
        )
    systems = (
        "  on_macos do\n"
        "    depends_on macos: :ventura\n"
        '    app "BranchTool.app"\n'
        "  end\n"
        "  on_linux do\n"
        '    binary "branchtool"\n'
        "  end\n"
    )
    return head + languages + systems + "end\n"


BRANCHES = {
    "Casks/t/pubtap-branches.rb": branches_rb(),
    "lib/pubtap_helper.rb": helper_rb("1.2.3", "Branch helper; part of tree identity."),
}

STAGING = {
    "Casks/s/pubtap-staging.rb": cask(
        "pubtap-staging",
        "1.0.0",
        "zip",
        "PubTap Staging",
        "Payload-dependent artifact list",
        tail('app "StagingTool.app"', 'binary "#{staged_path}/bin/staging-tool"'),
    ),
    # Positive control: without it this tree holds only a failing record
    # and the reader would fail the whole export.
    "Casks/c/pubtap-staging-control.rb": control_cask(
        "pubtap-staging-control", "PubTap Staging Control"
    ),
}

STAGING_RESCUED = {
    "Casks/s/pubtap-staging-rescued.rb": cask(
        "pubtap-staging-rescued",
        "1.0.0",
        "zip",
        "PubTap Staging Rescued",
        "Staging access rescued to a static fallback",
        tail(
            'app "StagingTool.app"',
            "begin",
            '  binary "#{staged_path}/bin/staging-tool"',
            "rescue StandardError",
            '  binary "staging-tool"',
            "end",
        ),
    ),
    # Positive control: without it this tree holds only a failing record
    # and the reader would fail the whole export.
    "Casks/c/pubtap-staging-rescued-control.rb": control_cask(
        "pubtap-staging-rescued-control", "PubTap Staging Rescued Control"
    ),
}

FLIGHTS = {
    # Legacy preflight/postflight callbacks are deprecated upstream and
    # raise a fatal reader rejection (for example MethodDeprecatedError)
    # under the pinned reader with HOMEBREW_DEVELOPER=1, so this record
    # is expected to fail record load, not to classify as an active step.
    # Keep the deprecated shape: it is the case under test. The structured
    # steps case separately tests active-operation classification.
    "Casks/p/pubtap-flights.rb": cask(
        "pubtap-flights",
        "2.0.0",
        "zip",
        "PubTap Flights",
        "Legacy preflight and postflight callbacks",
        tail(
            'app "FlightsTool.app"',
            "preflight do",
            '  FileUtils.mkdir_p("/opt/pubtap-flights")',
            "end",
            "postflight do",
            '  puts "pubtap-flights postflight"',
            "end",
        ),
    ),
    # Positive control: without it this tree holds only a failing record
    # and the reader would fail the whole export.
    "Casks/c/pubtap-flights-control.rb": control_cask(
        "pubtap-flights-control", "PubTap Flights Control"
    ),
}

STEPS = {
    "Casks/p/pubtap-steps.rb": cask(
        "pubtap-steps",
        "1.1.0",
        "zip",
        "PubTap Steps",
        "Structured postflight_steps with a run stanza",
        tail(
            'app "StepsTool.app"',
            "postflight_steps do",
            '  run "/usr/bin/true", args: []',
            "end",
        ),
    ),
}

INSTALLER = {
    "Casks/i/pubtap-installer.rb": cask(
        "pubtap-installer",
        "5.0.0",
        "dmg",
        "PubTap Installer",
        "Installer script stanza",
        tail(
            "installer script: {",
            '  executable: "install.sh",',
            '  args: ["--fixture"],',
            "}",
        ),
    ),
}

# Same basename in two directories, both declaring the same token: a real
# duplicate token, not a token/filename mismatch.
DUP = {
    "Casks/a/pubtap-dup.rb": cask(
        "pubtap-dup",
        "1.0.0",
        "zip",
        "PubTap Duplicate One",
        "Duplicate token, file one",
        tail('app "DupOne.app"'),
    ),
    "Casks/b/pubtap-dup.rb": cask(
        "pubtap-dup",
        "1.0.0",
        "zip",
        "PubTap Duplicate Two",
        "Duplicate token, file two",
        tail('app "DupTwo.app"'),
    ),
}

BROKEN = (
    "# Synthetic fixture: intentionally invalid Ruby.\n"
    "# Expected: one excluded record with code record-load-error.\n"
    'cask "pubtap-broken" do\n  version "1.0.0"\n  this is not valid ruby {{{\nend\n'
)

MALFORMED = {
    "Casks/m/pubtap-broken.rb": BROKEN,
    # Positive control: without it this tree holds only a failing record
    # and the reader would fail the whole export.
    "Casks/c/pubtap-broken-control.rb": control_cask(
        "pubtap-broken-control", "PubTap Broken Control"
    ),
}


def flood_rb(budget: int) -> str:
    """Bounded output flood: emits exactly the fixed budget, then stops.

    The budget is baked in from matrix policy. There is no environment
    override, so the fixture itself can never become unbounded; the runner
    must still cap captured logs while the command runs.
    """
    return (
        f"# Bounded output flood: emits exactly {budget} bytes of stdout,\n"
        "# then stops. Fixed budget; no environment override exists. The\n"
        "# runner must still cap captured output while the command runs.\n"
        'chunk = "PUBTAP-FLOOD " + ("x" * 99) + "\\n"\n'
        f"budget = {budget}\n"
        "emitted = 0\n"
        "while emitted + chunk.bytesize <= budget\n"
        "  puts chunk\n"
        "  emitted += chunk.bytesize\n"
        "end\n"
        'puts "PUBTAP-FLOOD-DONE emitted=#{emitted}"\n'
    )


def local_url_cask(token: str, title: str, url: str) -> str:
    """Cask whose URL is a literal local or private address; no DNS name.

    Redirect and DNS policy are backend-test scope. These literals only
    exercise the destination check without any name resolution.
    """
    return (
        "# Literal local/private destination; no DNS name, no redirect.\n"
        "# Expected: excluded on both targets; the destination policy must\n"
        "# refuse it without a lookup.\n"
        f'cask "{token}" do\n'
        '  version "1.0.0"\n'
        f'  sha256 "{synth(token + ":zip")}"\n'
        f'  url "{url}"\n'
        f'  name "{title}"\n'
        '  desc "Literal local destination (synthetic fixture)"\n'
        f'  homepage "https://example.com/pubtap/{token}"\n'
        '  on_macos do\n    binary "pubtap-tool"\n  end\n'
        '  on_linux do\n    binary "pubtap-tool"\n  end\n'
        "end\n"
    )


def url_literal_tree(token: str, title: str, url: str) -> dict[str, str]:
    return {f"Casks/u/{token}.rb": local_url_cask(token, title, url)}


URL_LITERALS: dict[str, dict[str, str]] = {
    "url-localhost": url_literal_tree(
        "pubtap-url-localhost",
        "PubTap URL Localhost",
        "http://localhost:9/pubtap/url-localhost.zip",
    ),
    "url-private": url_literal_tree(
        "pubtap-url-private",
        "PubTap URL Private",
        "http://192.168.1.9/pubtap/url-private.zip",
    ),
    "url-linklocal": url_literal_tree(
        "pubtap-url-linklocal",
        "PubTap URL Link-Local",
        "http://169.254.169.254/pubtap/url-linklocal.zip",
    ),
    "url-ipv6": url_literal_tree(
        "pubtap-url-ipv6",
        "PubTap URL IPv6 Loopback",
        "http://[::1]:9/pubtap/url-ipv6.zip",
    ),
}

# Intentional two-source collision pair: editor eligible in both, viewer
# eligible only on the beta side (the alpha viewer uses an installer script).
COLLISION_A = {
    "Casks/e/editor.rb": cask(
        "editor",
        "7.2.0",
        "zip",
        "Alpha Editor",
        "Cross-tap collision token, alpha side",
        tail(
            "on_macos do",
            '  app "AlphaEditor.app"',
            "end",
            "on_linux do",
            '  binary "alpha-editor"',
            "end",
        ),
    ),
    "Casks/v/viewer.rb": cask(
        "viewer",
        "2.1.0",
        "dmg",
        "Alpha Viewer",
        "Excluded collision token, alpha side",
        tail(
            "installer script: {",
            '  executable: "#{staged_path}/Viewer Installer.app/Contents/MacOS/install",',
            "}",
        ),
    ),
}

COLLISION_B = {
    "Casks/e/editor.rb": cask(
        "editor",
        "4.5.0",
        "zip",
        "Beta Editor",
        "Cross-tap collision token, beta side",
        tail(
            "on_macos do",
            '  binary "beta-editor"',
            "end",
            "on_linux do",
            '  binary "beta-editor"',
            "end",
        ),
    ),
    "Casks/v/viewer.rb": cask(
        "viewer",
        "9.0.1",
        "zip",
        "Beta Viewer",
        "Eligible collision token, beta side",
        tail(
            "on_macos do",
            '  app "BetaViewer.app"',
            "end",
            "on_linux do",
            '  binary "beta-viewer"',
            "end",
        ),
    ),
}


def invalidation_tree(version: str) -> dict[str, str]:
    """One tree variant; only the helper VERSION value changes between v1/v2."""
    token = "pubtap-invalidation"
    cask_rb = (
        "# Cask file is byte-identical in v1 and v2; only the helper differs.\n"
        'require_relative "../../lib/pubtap_helper"\n'
        'cask "' + token + '" do\n'
        "  version PubtapHelper.version\n"
        '  sha256 "' + synth(token + ":zip") + '"\n'
        '  url "https://example.com/pubtap/'
        + token
        + "/"
        + token
        + '-#{PubtapHelper.version}.zip"\n'
        '  name "PubTap Invalidation"\n'
        '  desc "Helper-only invalidation fixture (synthetic)"\n'
        '  homepage "https://example.com/pubtap/' + token + '"\n'
        '  app "InvalidationTool.app"\n'
        "end\n"
    )
    return {
        f"Casks/t/{token}.rb": cask_rb,
        "lib/pubtap_helper.rb": helper_rb(
            version, "Invalidation helper; only VERSION differs between variants."
        ),
    }


def build_files(matrix: dict) -> dict[str, str]:
    bounds = matrix["policy"]["bounds"]
    trees: dict[str, dict[str, str]] = {
        "release-archive": RELEASE_ARCHIVE,
        "raw-binary": RAW_BINARY,
        "app-bundle": APP_BUNDLE,
        "branches": BRANCHES,
        "staging": STAGING,
        "staging-rescued": STAGING_RESCUED,
        "flights": FLIGHTS,
        "steps": STEPS,
        "installer": INSTALLER,
        "dup": DUP,
        "broken": MALFORMED,
        "flood": {
            "Casks/f/pubtap-flood.rb": flood_rb(bounds["output_exhaustion_emit_bytes"])
            + cask(
                "pubtap-flood",
                "1.0.0",
                "zip",
                "PubTap Flood",
                "Output exhaustion probe",
                tail('app "FloodTool.app"'),
            ),
        },
        "collision-a": COLLISION_A,
        "collision-b": COLLISION_B,
    }
    trees.update(URL_LITERALS)
    files: dict[str, str] = {}
    for name, tree in trees.items():
        for rel, content in tree.items():
            files[f"taps/{name}/{rel}"] = content
    for v, version in ((1, "1.4.0"), (2, "1.4.1")):
        for rel, content in invalidation_tree(version).items():
            files[f"taps/invalidation/alpha-v{v}/{rel}"] = content
    files["harness/policy.json"] = (
        json.dumps(
            {
                "generator_version": GENERATOR_VERSION,
                "matrix_version": matrix["matrix_version"],
                "bounds": bounds,
                "note": "Data-only fixtures. Sandbox denial proofs are not "
                "generated here; the parent runs controlled native Nix probes "
                "and the backend runs the real reader-gate probe.",
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    return files


def prepare_out_dir(out_dir: Path) -> Path:
    """Resolve parent aliases, then accept only a fresh or empty directory.

    Canonical parent aliases such as /tmp or /var on macOS are resolved
    first, so default mktemp targets work. The resolved leaf must not be a
    symlink, and an existing leaf must be an empty directory.
    """
    if not out_dir.is_absolute():
        raise SystemExit(f"output directory must be absolute: {out_dir}")
    if out_dir.is_symlink():
        raise SystemExit(f"refusing symlinked output directory: {out_dir}")
    resolved = out_dir.resolve()
    if resolved.is_symlink():
        raise SystemExit(f"refusing symlinked output directory: {resolved}")
    if resolved.exists():
        if not resolved.is_dir():
            raise SystemExit(f"output path exists and is not a directory: {resolved}")
        if any(resolved.iterdir()):
            raise SystemExit(f"output directory exists and is not empty: {resolved}")
    resolved.mkdir(parents=True, exist_ok=True)
    return resolved


def generate(out_dir: Path) -> int:
    matrix = json.loads(MATRIX.read_text(encoding="utf-8"))
    bounds = matrix["policy"]["bounds"]
    files = build_files(matrix)
    if len(files) > bounds["max_generated_files"]:
        raise SystemExit(f"generated file count exceeds policy bound: {len(files)}")
    manifest = {
        "generator_version": GENERATOR_VERSION,
        "matrix_version": matrix["matrix_version"],
        "source_id": matrix["source_id"],
        "cases": {c["id"]: sorted(c["files"]) for c in matrix["cases"]},
        "files": {},
    }
    out_dir = prepare_out_dir(out_dir)
    for rel in sorted(files):
        path = out_dir / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.is_symlink():
            raise SystemExit(f"refusing to overwrite symlink: {path}")
        data = files[rel].encode("utf-8")
        if len(data) > bounds["max_fixture_file_bytes"]:
            raise SystemExit(f"fixture exceeds size bound: {rel}")
        path.write_bytes(data)
        manifest["files"][rel] = hashlib.sha256(data).hexdigest()
    (out_dir / "matrix-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return len(files)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Write deterministic data-only fixtures"
    )
    parser.add_argument(
        "--out",
        type=Path,
        required=True,
        help="absolute disposable output directory; fresh or empty",
    )
    args = parser.parse_args()
    count = generate(args.out)
    print(f"wrote {count} fixture files to {args.out}; no Ruby executed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
