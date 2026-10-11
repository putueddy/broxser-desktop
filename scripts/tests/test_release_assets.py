"""Tests for scripts/release-assets.sh (ADR 0025, decision 3) that build
nothing: a tag that does not name the workspace version is refused before the
script builds or writes anything.

    python3 -m unittest discover -s scripts/tests
"""

import subprocess
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "release-assets.sh"
VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]


class TagTests(unittest.TestCase):
    def test_a_tag_that_does_not_name_the_workspace_version_is_refused(self):
        for tag in ("", VERSION, f"V{VERSION}", f"v{VERSION}.1", f"v{VERSION}-rc.1", "v0.0.0"):
            with self.subTest(tag=tag):
                result = subprocess.run(["bash", str(SCRIPT), tag], cwd=ROOT, capture_output=True, text=True)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"does not name the workspace version {VERSION}", result.stderr)
                self.assertNotIn("Compiling", result.stderr)
                if tag:
                    self.assertFalse((ROOT / "artifacts" / "release" / tag).exists())


if __name__ == "__main__":
    unittest.main()
