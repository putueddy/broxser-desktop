"""Tests for scripts/qualify-helium.py (ADR 0026). No network and no browser:
the signature tests sign with throwaway keys in a private GnuPG home.

    python3 -m unittest discover -s scripts/tests
"""

import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "qualify-helium.py"
spec = importlib.util.spec_from_file_location("qualify_helium", SCRIPT)
qualify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualify)

HAS_GPG = shutil.which("gpg") is not None and shutil.which("gpgv") is not None


class ManifestTests(unittest.TestCase):
    def test_the_pinned_manifest_can_be_qualified(self):
        self.assertEqual(qualify.check_manifest(json.loads(qualify.PINNED.read_text())), [])

    def test_manifests_that_would_fetch_something_else_are_refused(self):
        pinned = json.loads(qualify.PINNED.read_text())
        changes = {
            "platform": {"platform": "linux-arm64"},
            "version": {"version": "latest"},
            "url must be": {"url": pinned["url"].replace("imputnet", "someone")},
            "release_url": {"release_url": "https://example.invalid/"},
            "chromium_version": {"chromium_version": None},
            "sha256": {"sha256": pinned["sha256"].upper()},
        }
        for expected, change in changes.items():
            with self.subTest(change=change):
                problems = qualify.check_manifest({**pinned, **change})
                self.assertTrue(any(expected in problem for problem in problems), problems)
        self.assertEqual(qualify.check_manifest([pinned]), ["a manifest is a JSON object"])
        other = {**pinned, "version": "0.17.2.1"}
        self.assertTrue(any("url must be" in problem for problem in qualify.check_manifest(other)),
                        "a version must come with its own tarball")

    def test_reported_versions_and_test_counts_are_read(self):
        self.assertEqual(qualify.reported_version("Helium 0.18.1.1 (Chromium 154.0.8037.57)\n"),
                         ("0.18.1.1", "154.0.8037.57"))
        with self.assertRaises(qualify.Refused):
            qualify.reported_version("Chromium 154.0.8037.57\n")
        output = (
            "test live::tests::a ... ok\n"
            "test live::tests::b ... FAILED\n"
            "test result: ok. 44 passed; 0 failed; 0 ignored; 0 measured; 104 filtered out\n"
            "test result: FAILED. 3 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out\n"
        )
        self.assertEqual(qualify.test_counts(output),
                         {"passed": 47, "failed": 2, "ignored": 1, "failures": ["live::tests::b"]})

    def test_only_complete_runs_qualify(self):
        passed = {"result": "passed"}
        self.assertEqual(qualify.verdict([passed, passed]), "qualified")
        self.assertEqual(qualify.verdict([passed, {"result": "skipped"}]), "incomplete")
        self.assertEqual(qualify.verdict([{"result": "failed"}, {"result": "skipped"}]), "not qualified")


@unittest.skipUnless(HAS_GPG, "gpg and gpgv are required")
class SignatureTests(unittest.TestCase):
    def setUp(self):
        self.scratch = Path(tempfile.mkdtemp())
        self.home = self.scratch / "gnupg"
        self.home.mkdir(mode=0o700)
        self.env = {**os.environ, "GNUPGHOME": str(self.home)}
        self.keys = {name: self.generate(name) for name in ("release", "other")}
        self.data = self.scratch / "helium.tar.xz"
        self.data.write_bytes(b"not really a browser\n")

    def tearDown(self):
        subprocess.run(["gpgconf", "--kill", "all"], env=self.env, capture_output=True)
        shutil.rmtree(self.scratch)

    def gpg(self, *args, **kwargs):
        return subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "",
                               *args], env=self.env, capture_output=True, check=True, **kwargs)

    def generate(self, name):
        self.gpg("--quick-gen-key", f"{name} <{name}@example.invalid>", "ed25519", "sign", "never")
        listing = self.gpg("--with-colons", "--list-keys", f"{name}@example.invalid", text=True).stdout
        fingerprint = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        key = self.scratch / f"{name}.asc"
        key.write_bytes(self.gpg("--armor", "--export", fingerprint).stdout)
        return fingerprint, key

    def sign(self, name, data):
        signature = self.scratch / f"{name}.sig"
        self.gpg("--yes", "--armor", "--local-user", self.keys[name][0],
                 "--output", str(signature), "--detach-sign", str(data))
        return signature

    def ring(self, name, fingerprint=None):
        fingerprint = fingerprint or self.keys[name][0]
        scratch = tempfile.mkdtemp(dir=self.scratch)
        return qualify.keyring(self.keys[name][1], fingerprint, scratch)

    def test_a_signature_by_the_pinned_key_is_accepted(self):
        signed_at = qualify.verify_signature(self.data, self.sign("release", self.data),
                                             self.ring("release"), self.keys["release"][0])
        self.assertRegex(signed_at, r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")

    def test_changed_data_and_other_signers_are_refused(self):
        signature = self.sign("release", self.data)
        self.data.write_bytes(b"a browser someone changed\n")
        with self.assertRaisesRegex(qualify.Refused, "not accepted"):
            qualify.verify_signature(self.data, signature, self.ring("release"), self.keys["release"][0])
        other = self.sign("other", self.data)
        with self.assertRaisesRegex(qualify.Refused, "not accepted"):
            qualify.verify_signature(self.data, other, self.ring("release"), self.keys["release"][0])
        with self.assertRaisesRegex(qualify.Refused, "not by"):
            qualify.verify_signature(self.data, other, self.ring("other"), self.keys["release"][0])

    def test_a_key_file_must_hold_exactly_the_pinned_key(self):
        with self.assertRaisesRegex(qualify.Refused, "not the one key"):
            self.ring("other", self.keys["release"][0])
        both = self.scratch / "both.asc"
        both.write_bytes(self.keys["release"][1].read_bytes() + self.keys["other"][1].read_bytes())
        with self.assertRaisesRegex(qualify.Refused, "not the one key"):
            qualify.keyring(both, self.keys["release"][0], tempfile.mkdtemp(dir=self.scratch))

    def test_the_committed_key_is_heliums(self):
        qualify.keyring(qualify.SIGNING_KEY, qualify.SIGNING_FINGERPRINT, tempfile.mkdtemp(dir=self.scratch))


if __name__ == "__main__":
    unittest.main()
