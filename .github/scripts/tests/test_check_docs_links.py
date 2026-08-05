#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Focused unit tests for the repo docs link checker.

Scope of this file (PR-0 fix): prove that Markdown links hidden inside HTML
comments are masked before link extraction and before the PR-0 structural
invariant checks, so a commented-out link can never satisfy the required
README -> plans/08 / CONTRIBUTING -> plans/11 links.

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


class StructuralInvariantTests(unittest.TestCase):
    """Requirements (b) and (c) exercised via _links_to in isolated temp repos.

    _links_to is the predicate behind the README -> plans/08 and
    CONTRIBUTING -> plans/11 invariants in check_structure. It shares the single
    extract_links choke point, so masking there covers both code paths.
    """

    def _make_repo(self, readme: str, contributing: str) -> Path:
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        root = Path(td.name)
        _write(root / "README.md", readme)
        _write(root / "CONTRIBUTING.md", contributing)
        _write(root / "plans" / "08-security-model.md", "# Threat model\n")
        _write(root / "plans" / "11-pr-roadmap.md", "# PR roadmap\n")
        return root

    # --- (b) a commented-only required link must NOT satisfy the invariant ---

    def test_commented_only_threat_link_fails_invariant(self):
        root = self._make_repo(
            readme="# pkg\n<!-- [threat](plans/08-security-model.md) -->\n",
            contributing="# Contrib\n",
        )
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertFalse(
                cdl._links_to(root / "README.md", "plans/08-security-model.md")
            )

    def test_commented_only_reviewer_link_fails_invariant(self):
        root = self._make_repo(
            readme="# pkg\n",
            contributing="# Contrib\n<!-- [roadmap](plans/11-pr-roadmap.md) -->\n",
        )
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertFalse(
                cdl._links_to(root / "CONTRIBUTING.md", "plans/11-pr-roadmap.md")
            )

    def test_multiline_commented_only_threat_link_fails_invariant(self):
        readme = (
            "# pkg\n"
            "<!-- a multi-line note\n"
            "[threat](plans/08-security-model.md)\n"
            "end note -->\n"
        )
        root = self._make_repo(readme=readme, contributing="# Contrib\n")
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertFalse(
                cdl._links_to(root / "README.md", "plans/08-security-model.md")
            )

    # --- (c) the existing visible links still satisfy the invariant --------

    def test_visible_threat_link_satisfies_invariant(self):
        root = self._make_repo(
            readme="# pkg\n[threat](plans/08-security-model.md)\n",
            contributing="# Contrib\n",
        )
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertTrue(
                cdl._links_to(root / "README.md", "plans/08-security-model.md")
            )

    def test_visible_reviewer_link_satisfies_invariant(self):
        root = self._make_repo(
            readme="# pkg\n",
            contributing="# Contrib\n[roadmap](plans/11-pr-roadmap.md)\n",
        )
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertTrue(
                cdl._links_to(root / "CONTRIBUTING.md", "plans/11-pr-roadmap.md")
            )

    def test_visible_link_beats_commented_broken_link(self):
        # A real visible invariant link plus a commented broken link: the
        # invariant is satisfied AND the commented broken link is not extracted.
        readme = (
            "# pkg\n"
            "<!-- [broken](missing.md) -->\n"
            "[threat](plans/08-security-model.md)\n"
        )
        root = self._make_repo(readme=readme, contributing="# Contrib\n")
        with patch.object(cdl, "REPO_ROOT", root):
            self.assertTrue(
                cdl._links_to(root / "README.md", "plans/08-security-model.md")
            )
        targets = [lnk.target for lnk in cdl.extract_links(root / "README.md", readme)]
        self.assertIn("plans/08-security-model.md", targets)
        self.assertNotIn("missing.md", targets)


if __name__ == "__main__":
    unittest.main(verbosity=2)
