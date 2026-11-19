"""Tests of tools/closed_deps_check.py: python -m unittest discover -s tools -p "test_*.py"."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import closed_deps_check as c  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def write(root, rel, text):
    p = os.path.join(root, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "w", encoding="utf-8") as f:
        f.write(text)


class ClosedDeps(unittest.TestCase):
    def test_repo_is_clean(self):
        self.assertEqual(c.scan(REPO), [])

    def test_clean_manifest_passes(self):
        with tempfile.TemporaryDirectory() as t:
            write(t, "a/Cargo.toml", '[dependencies]\nserde = "1"\nb = { path = "../b" }\n')
            self.assertEqual(c.scan(t), [])

    def test_violations_are_reported(self):
        with tempfile.TemporaryDirectory() as t:
            write(t, "repo/a/Cargo.toml", "\n".join([
                "[dependencies]",
                'ddc-rvt = "1"',
                'x = { git = "https://example.invalid/x" }',
                'y = { path = "../../outside" }',
                'z = { version = "1", registry = "corp" }',
                '[target.\'cfg(windows)\'.dependencies]',
                'w = { package = "closed-reader", version = "1" }',
                "[features]",
                "closed-rvt = []",
            ]))
            hits = "\n".join(c.scan(os.path.join(t, "repo")))
            for want in ("ddc-rvt", "git dependency", "path outside repo",
                         "non-default registry", "w: closed-looking", "feature closed-rvt"):
                self.assertIn(want, hits)


if __name__ == "__main__":
    unittest.main()
