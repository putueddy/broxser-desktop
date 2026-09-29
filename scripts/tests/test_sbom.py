"""Tests for the license texts of scripts/sbom.py (ADR 0025, decision 1). No
cargo run: crates are small directories made here.

    python3 -m unittest discover -s scripts/tests
"""

import importlib.util
import shutil
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "sbom.py"
spec = importlib.util.spec_from_file_location("sbom", SCRIPT)
sbom = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sbom)


def elected(expression):
    return sbom.elect(sbom.parse_expression(expression))


class ElectionTests(unittest.TestCase):
    def test_a_permissive_choice_is_taken_by_preference(self):
        self.assertEqual(elected("MIT OR Apache-2.0"), ["Apache-2.0"])
        self.assertEqual(elected("Apache-2.0 OR GPL-2.0-only"), ["Apache-2.0"])
        self.assertEqual(elected("Unlicense OR MIT"), ["MIT"])
        self.assertEqual(elected("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"), ["Apache-2.0"])
        self.assertEqual(elected("(MIT OR Apache-2.0) AND Unicode-3.0"), ["Apache-2.0", "Unicode-3.0"])
        self.assertEqual(elected("Apache-2.0 AND ISC"), ["Apache-2.0", "ISC"])

    def test_mpl_is_accepted_only_without_a_permissive_alternative(self):
        self.assertEqual(elected("MPL-2.0"), ["MPL-2.0"])
        self.assertEqual(elected("MPL-2.0 OR MIT"), ["MIT"])

    def test_copyleft_only_expressions_are_refused(self):
        for expression in ("GPL-3.0-only", "LGPL-2.1-or-later", "MIT AND LGPL-2.1-only",
                           "GPL-2.0-only WITH Classpath-exception-2.0", "NOASSERTION"):
            with self.subTest(expression=expression):
                self.assertIsNone(elected(expression))

    def test_malformed_expressions_are_errors(self):
        for expression in ("MIT OR", "(MIT", "MIT Apache-2.0", "AND MIT", "MIT WITH"):
            with self.subTest(expression=expression), self.assertRaises(ValueError):
                sbom.parse_expression(expression)


class LicenseTextTests(unittest.TestCase):
    def setUp(self):
        self.scratch = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.scratch)

    def crate(self, name, files):
        directory = self.scratch / name
        directory.mkdir()
        for path, text in files.items():
            (directory / path).parent.mkdir(parents=True, exist_ok=True)
            if isinstance(text, bytes):
                (directory / path).write_bytes(text)
            else:
                (directory / path).write_text(text)
        (directory / "Cargo.toml").write_text("[package]\n")
        return directory

    def test_license_files_include_bundled_code_but_not_tests_or_sources(self):
        directory = self.crate("a", {
            "LICENSE-MIT": "mit", "COPYING.LIB": "lgpl", "src/tables/LICENSE-UNICODE": "unicode",
            "src/license.rs": "fn main() {}", "tests/data/LICENSE": "test data", "examples/NOTICE": "example",
            "README.md": "readme",
        })
        self.assertEqual(sbom.license_files(directory),
                         [("COPYING.LIB", "lgpl"), ("LICENSE-MIT", "mit"),
                          ("src/tables/LICENSE-UNICODE", "unicode")])

    def test_the_mit_standard_text_names_the_authors(self):
        text = sbom.standard_text("MIT", "crate", ["A <a@example.invalid>", "B"])
        self.assertIn("Copyright (c) A <a@example.invalid>, B\n", text)
        self.assertIn("Copyright (c) the crate authors\n", sbom.standard_text("MIT", "crate", []))
        self.assertIsNone(sbom.standard_text("Apache-2.0 WITH LLVM-exception", "crate", []))
        self.assertIsNone(sbom.standard_text("Zlib", "crate", []))

    def bundle(self, crates):
        packages, entries = {}, []
        for name, declared, files, vendored in crates:
            spdx = f"SPDXRef-{'vendored' if vendored else 'crate'}-{name}-1.0.0"
            packages[spdx] = {"manifest_path": str(self.crate(name, files) / "Cargo.toml"),
                              "authors": [f"{name} author"]}
            entries.append({"SPDXID": spdx, "name": name, "versionInfo": "1.0.0", "licenseDeclared": declared,
                            "downloadLocation": f"https://crates.io/api/v1/crates/{name}/1.0.0/download",
                            "checksums": [{"algorithm": "SHA256", "checksumValue": "0" * 64}]})
        entries.append({"SPDXID": "SPDXRef-broxser-own-0.1.0", "name": "own", "versionInfo": "0.1.0",
                        "licenseDeclared": "NOASSERTION", "downloadLocation": "NOASSERTION"})
        return sbom.license_bundle({"name": "broxser-test", "packages": entries}, packages)

    def test_identical_texts_are_grouped_and_missing_ones_get_the_standard_text(self):
        text, texts, problems = self.bundle([
            ("one", "MIT OR Apache-2.0", {"LICENSE-MIT": "same text\n"}, False),
            ("two", "MIT", {"LICENSE": "same text\n"}, False),
            ("bare", "MIT", {}, False),
            ("dual", "Apache-2.0 OR GPL-2.0-only", {"LICENSE-APACHE": "apache\n"}, False),
            ("mpl", "MPL-2.0", {"LICENSE": "mpl\n"}, False),
            ("patched", "Apache-2.0", {"LICENSE-APACHE": "apache\n"}, True),
        ])
        self.assertEqual(problems, [])
        self.assertEqual(texts, 4)
        self.assertIn("  one 1.0.0: LICENSE-MIT\n  two 1.0.0: LICENSE\n", text)
        self.assertIn("  dual 1.0.0: LICENSE-APACHE\n  patched 1.0.0: LICENSE-APACHE\n", text)
        self.assertIn("- bare 1.0.0: declares MIT; the standard MIT text", text)
        self.assertIn("Copyright (c) bare author\n", text)
        self.assertIn("- dual 1.0.0 declares Apache-2.0 OR GPL-2.0-only; Broxser uses it under Apache-2.0.", text)
        self.assertIn("- mpl 1.0.0 is used under MPL-2.0 and unmodified; its source form is "
                      "https://crates.io/api/v1/crates/mpl/1.0.0/download", text)
        self.assertIn("- patched 1.0.0 is vendored with changes by Broxser", text)
        self.assertNotIn("own 0.1.0", text)

    def test_copyleft_modified_or_textless_components_are_problems(self):
        _, _, problems = self.bundle([
            ("gpl", "GPL-3.0-only", {"COPYING": "gpl\n"}, False),
            ("mplpatch", "MPL-2.0", {"LICENSE": "mpl\n"}, True),
            ("zlib", "Zlib", {}, False),
            ("broken", "MIT OR", {"LICENSE": "mit\n"}, False),
            ("latin", "MIT", {"LICENSE": b"Copyright \xa9 2020\n"}, False),
        ])
        self.assertEqual(len(problems), 5, problems)
        self.assertIn("gpl 1.0.0: GPL-3.0-only offers no license Broxser uses", problems[0])
        self.assertIn("mplpatch 1.0.0: MPL-2.0 code must be used unmodified", problems[1])
        self.assertIn("zlib 1.0.0 ships no license file and scripts/licenses has no text for Zlib", problems[2])
        self.assertIn("broken 1.0.0: license expression 'MIT OR' ends early", problems[3])
        self.assertEqual("latin 1.0.0: LICENSE is not UTF-8 text", problems[4])


class ShippedGraphTests(unittest.TestCase):
    def test_every_standard_text_file_is_described(self):
        readme = (sbom.STANDARD_TEXTS / "README.md").read_text()
        for path in sbom.STANDARD_TEXTS.glob("*.txt"):
            self.assertIn(f"`{path.name}`", readme)


if __name__ == "__main__":
    unittest.main()
