#!/usr/bin/env python3
"""Software bill of materials for the Linux x86_64 build (P3.1, ADR 0025).

Writes an SPDX 2.3 JSON document for the shipped binaries `broxser-desktop`
and `broxser`: every crate they link on x86_64-unknown-linux-gnu (normal
dependencies only; build tools are not shipped), with its version, declared
license, crates.io download location and the SHA-256 that Cargo.lock pins,
the vendored GPUI, and Helium as a runtime dependency that is fetched and
verified separately and not contained in the package. Output is deterministic
for a commit: packages are sorted and the creation time is the commit's, in
UTC regardless of the machine's timezone. Requires Python 3.11+ (tomllib).

`--licenses` writes the license and notice files of every shipped
third-party component, grouped by identical text; a crate that ships none gets
the standard text of the license it declares (scripts/licenses). Each crate is
used under a permissive license it offers, or MPL-2.0 unmodified from crates.io;
`--check` fails for a crate that offers neither (ADR 0025, decision 1).

    python3 scripts/sbom.py --output sbom.spdx.json [--summary licenses.md]
        [--licenses THIRD-PARTY-LICENSES.txt]
    python3 scripts/sbom.py --check   # validate without writing
"""

import argparse
import collections
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

if sys.version_info < (3, 11):
    sys.exit(f"scripts/sbom.py needs Python 3.11 or newer (tomllib); found {sys.version.split()[0]}")
import tomllib  # noqa: E402  (import guarded by the version check above)

TARGET = "x86_64-unknown-linux-gnu"
ROOTS = ("broxser-desktop", "broxser-cli")
ROOT = Path(__file__).resolve().parent.parent
STANDARD_TEXTS = ROOT / "scripts" / "licenses"

# Licenses Broxser may use a crate under: permissive ones, and MPL-2.0 for a
# crate used unmodified from crates.io (its source form is named in the notes).
PERMISSIVE = {
    "0BSD", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "BSL-1.0", "CC0-1.0", "ISC",
    "MIT", "MIT-0", "Unicode-3.0", "Unicode-DFS-2016", "Unlicense", "Zlib",
}
FILE_COPYLEFT = {"MPL-2.0"}
EXCEPTIONS = {"LLVM-exception"}
# Among alternatives, the first of these that a crate offers is taken.
PREFERENCE = ("Apache-2.0", "MIT", "BSD-3-Clause", "BSD-2-Clause", "ISC", "Zlib")
LICENSE_FILE = re.compile(r"^(?:licen[cs]e|copying|unlicense|notice|copyright)", re.IGNORECASE)
SOURCE_SUFFIXES = {"c", "cc", "cpp", "h", "html", "js", "json", "py", "rs", "sh", "toml", "ts", "yaml", "yml"}
SKIPPED_DIRECTORIES = {".github", "benches", "doc", "docs", "examples", "fuzz", "target", "test",
                       "testdata", "tests"}


def run(*command):
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(
            f"`{' '.join(command)}` failed with {result.returncode}:\n{result.stderr.strip()}"
        )
    return result.stdout


def spdx_id(kind, name, version=""):
    raw = f"{kind}-{name}-{version}" if version else f"{kind}-{name}"
    return "SPDXRef-" + re.sub(r"[^A-Za-z0-9.-]", "-", raw)


def license_expression(declared):
    """A crate's declared license as an SPDX expression: the legacy `/`
    separator becomes OR; nothing declared becomes NOASSERTION."""
    if not declared:
        return "NOASSERTION"
    expression = re.sub(r"\s*/\s*", " OR ", declared.strip())
    return re.sub(r"\s+", " ", expression)


LICENSE_TOKEN = re.compile(r"^(?:[A-Za-z0-9.+-]+|AND|OR|WITH|\(|\))$")


def valid_expression(expression):
    tokens = expression.replace("(", " ( ").replace(")", " ) ").split()
    return expression == "NOASSERTION" or (tokens and all(LICENSE_TOKEN.match(t) for t in tokens))


def parse_expression(expression):
    """An SPDX license expression as a tree: a license identifier,
    ("WITH", license, exception), or ("AND" | "OR", [operands]). WITH binds
    tighter than AND, and AND tighter than OR."""
    tokens = expression.replace("(", " ( ").replace(")", " ) ").split()
    position = 0

    def take():
        nonlocal position
        if position == len(tokens):
            raise ValueError(f"license expression {expression!r} ends early")
        position += 1
        return tokens[position - 1]

    def operand():
        token = take()
        if token == "(":
            node = alternatives()
            if take() != ")":
                raise ValueError(f"license expression {expression!r} has unbalanced parentheses")
            return node
        if token in ("AND", "OR", "WITH", ")"):
            raise ValueError(f"license expression {expression!r} has {token} out of place")
        if position < len(tokens) and tokens[position] == "WITH":
            take()
            return ("WITH", token, take())
        return token

    def joined(operator, part):
        nodes = [part()]
        while position < len(tokens) and tokens[position] == operator:
            take()
            nodes.append(part())
        return nodes[0] if len(nodes) == 1 else (operator, nodes)

    def alternatives():
        return joined("OR", lambda: joined("AND", operand))

    tree = alternatives()
    if position != len(tokens):
        raise ValueError(f"license expression {expression!r} has {tokens[position]} out of place")
    return tree


def election_rank(licenses):
    """Orders the license sets a crate offers: permissive before MPL-2.0, then
    by PREFERENCE, a plain license before one WITH an exception, fewer first."""
    bases = [license_.split(" WITH ")[0] for license_ in licenses]
    return (any(base in FILE_COPYLEFT for base in bases),
            min((PREFERENCE.index(base) for base in bases if base in PREFERENCE), default=len(PREFERENCE)),
            any(" WITH " in license_ for license_ in licenses),
            len(licenses))


def elect(node):
    """The licenses Broxser uses a crate under, or None when its expression
    offers no acceptable choice."""
    if isinstance(node, str):
        return [node] if node in PERMISSIVE | FILE_COPYLEFT else None
    if node[0] == "WITH":
        _, license_, exception = node
        return [f"{license_} WITH {exception}"] if license_ in PERMISSIVE and exception in EXCEPTIONS else None
    choices = [elect(operand) for operand in node[1]]
    if node[0] == "AND":
        return None if None in choices else [license_ for choice in choices for license_ in choice]
    viable = [choice for choice in choices if choice is not None]
    return min(viable, key=election_rank) if viable else None


def license_files(directory):
    """(path, text) of the license and notice files in a crate's source, those
    of the code it bundles included; tests, examples and documentation are
    skipped. Deterministic order."""
    found = []
    for path, directories, files in os.walk(directory):
        directories[:] = sorted(name for name in directories if name not in SKIPPED_DIRECTORIES)
        for name in sorted(files):
            suffix = name.rsplit(".", 1)[1].lower() if "." in name else ""
            if LICENSE_FILE.match(name) and suffix not in SOURCE_SUFFIXES:
                relative = Path(path, name).relative_to(directory).as_posix()
                try:
                    found.append((relative, Path(path, name).read_text(encoding="utf-8")))
                except UnicodeDecodeError:
                    raise ValueError(f"{relative} is not UTF-8 text") from None
    return found


def standard_text(license_, name, authors):
    """The standard text of `license_` for a crate that ships none; the MIT
    copyright line names the crate's authors."""
    path = STANDARD_TEXTS / f"{license_}.txt"
    if " WITH " in license_ or not path.is_file():
        return None
    text = path.read_text(encoding="utf-8")
    if license_ == "MIT":
        text = text.replace("<year> <copyright holders>", ", ".join(authors) or f"the {name} authors")
    return text


def license_bundle(document, packages):
    """THIRD-PARTY-LICENSES.txt: every third-party component's license and
    notice files, grouped by identical text, with the notes the licenses call
    for. Returns the text, its number of distinct texts and any problems."""
    groups, notes, standard, problems = collections.defaultdict(list), [], [], []
    for entry in document["packages"]:
        vendored = entry["SPDXID"].startswith("SPDXRef-vendored-")
        if not (vendored or entry["SPDXID"].startswith("SPDXRef-crate-")):
            continue
        package, label, declared = packages[entry["SPDXID"]], f"{entry['name']} {entry['versionInfo']}", entry["licenseDeclared"]
        try:
            elected = elect(parse_expression(declared))
        except ValueError as error:
            problems.append(f"{label}: {error}")
            continue
        if elected is None:
            problems.append(f"{label}: {declared} offers no license Broxser uses "
                            "(a permissive one, or MPL-2.0 unmodified from crates.io)")
            continue
        chosen = " AND ".join(elected)
        if any(license_ in FILE_COPYLEFT for license_ in elected):
            if vendored:
                problems.append(f"{label}: {chosen} code must be used unmodified from crates.io")
                continue
            notes.append(f"{label} is used under {chosen} and unmodified; its source form is "
                         f"{entry['downloadLocation']} (SHA-256 {entry['checksums'][0]['checksumValue']}).")
        offered = set(re.findall(r"[A-Za-z0-9.+-]+", declared)) - {"AND", "OR", "WITH"} - EXCEPTIONS
        taken = {license_.split(" WITH ")[0] for license_ in elected}
        if any(license_ not in PERMISSIVE | FILE_COPYLEFT for license_ in offered - taken):
            notes.append(f"{label} declares {declared}; Broxser uses it under {chosen}.")
        if vendored:
            notes.append(f"{label} is vendored with changes by Broxser: each changed file says so, "
                         "and BROXSER-PATCH.md in its source describes them.")
        try:
            files = license_files(Path(package["manifest_path"]).parent)
        except ValueError as error:
            problems.append(f"{label}: {error}")
            continue
        if not files:
            texts = [(license_, standard_text(license_, entry["name"], package.get("authors") or []))
                     for license_ in elected]
            missing = [license_ for license_, text in texts if text is None]
            if missing:
                problems.append(f"{label} ships no license file and scripts/licenses has no text for "
                                f"{', '.join(missing)}")
                continue
            standard.append(f"{label}: declares {declared}; the standard {chosen} text")
            files = [(f"standard {license_} text", text) for license_, text in texts]
        for path, text in files:
            groups[text].append(f"{label}: {path}")

    lines = [
        f"Third-party licenses of {document['name']}",
        "",
        "The license and notice files of every third-party component linked into",
        "bin/broxser-desktop and bin/broxser (sbom.spdx.json lists them), grouped by",
        "identical text. A file here does not mean that all the code it covers is built",
        "into the binaries. Broxser's own code is not licensed (NOTICE.md); Helium is",
        "downloaded separately and not part of this archive.",
        "",
    ]
    if notes:
        lines += ["Notes:", ""] + [f"- {note}" for note in sorted(notes)] + [""]
    if standard:
        lines += ["Crates that ship no license file, with the standard text of the license",
                  "Broxser uses them under:", ""] + [f"- {line}" for line in sorted(standard)] + [""]
    ordered = sorted(groups.items(), key=lambda item: (sorted(item[1])[0], item[0]))
    for index, (text, users) in enumerate(ordered, 1):
        lines += ["=" * 78, f"[{index}/{len(ordered)}] used by:"] + [f"  {user}" for user in sorted(users)]
        lines += ["-" * 78, text.rstrip("\n"), ""]
    return "\n".join(lines) + "\n", len(ordered), problems


def build_document():
    metadata = json.loads(
        run("cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET)
    )
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    checksums = {
        (package["name"], package["version"]): package.get("checksum")
        for package in lock["package"]
    }
    by_name_version = {(package["name"], package["version"]): package for package in metadata["packages"]}

    # The crates the binaries link, per `cargo tree`: it resolves features for
    # these packages alone, while `cargo metadata` unifies them across the
    # workspace and its tests. Depth prefixes give the edges; repeated
    # subtrees are listed once, their edges still appear.
    tree = run(
        "cargo", "tree", "--locked", "--edges", "normal", "--target", TARGET,
        "--prefix", "depth", "--format", "{p}",
        *[argument for root in ROOTS for argument in ("--package", root)],
    )
    shipped, edges, stack = {}, set(), []
    for line in tree.splitlines():
        match = re.match(r"^(\d+)(\S+) v(\S+)", line)
        if not match:
            continue
        digits, name, version = match.group(1), match.group(2), match.group(3)
        # The depth prefix is concatenated to the package without a separator,
        # so a name that starts with a digit makes the greedy digit match
        # ambiguous. Try the longest depth first and give digits back to the
        # name until the package is one cargo metadata knows at a depth the
        # tree allows (a child is at most one deeper than the current stack).
        package, depth = None, 0
        for split in range(len(digits), 0, -1):
            candidate_depth = int(digits[:split])
            if candidate_depth > len(stack):
                continue
            if (digits[split:] + name, version) in by_name_version:
                package = by_name_version[(digits[split:] + name, version)]
                depth = candidate_depth
                break
        if package is None:
            raise SystemExit(
                f"cargo tree lists `{line}`; no split of it is a package cargo metadata knows"
            )
        shipped[package["id"]] = package
        del stack[depth:]
        if stack:
            edges.add((stack[-1], package["id"]))
        stack.append(package["id"])
    packages = shipped
    roots = [package["id"] for package in shipped.values()
             if package["name"] in ROOTS and package["source"] is None]
    if len(roots) != len(ROOTS):
        raise SystemExit(f"expected the workspace crates {ROOTS}, found {len(roots)}")

    commit = run("git", "rev-parse", "HEAD").strip()
    # SPDX 2.3 requires `created` in UTC. Git's format-local renders in the
    # machine's timezone, so derive the timestamp from the commit epoch
    # instead; this also keeps the SBOM byte-identical across timezones.
    epoch = int(run("git", "log", "-1", "--format=%ct").strip())
    created = datetime.fromtimestamp(epoch, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    workspace_version = packages[roots[0]]["version"]

    ids, spdx_packages = {}, []
    for package_id in sorted(packages, key=lambda pid: (packages[pid]["name"], packages[pid]["version"])):
        package = packages[package_id]
        name, crate_version, source = package["name"], package["version"], package["source"]
        entry = {
            "name": name,
            "versionInfo": crate_version,
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": license_expression(package.get("license")),
            "copyrightText": "NOASSERTION",
            "supplier": "NOASSERTION",
        }
        if source and source.startswith("registry+"):
            entry["SPDXID"] = spdx_id("crate", name, crate_version)
            entry["downloadLocation"] = f"https://crates.io/api/v1/crates/{name}/{crate_version}/download"
            checksum = checksums.get((name, crate_version))
            if not checksum:
                raise SystemExit(f"Cargo.lock pins no checksum for {name} {crate_version}")
            entry["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
            entry["externalRefs"] = [{
                "referenceCategory": "PACKAGE-MANAGER",
                "referenceType": "purl",
                "referenceLocator": f"pkg:cargo/{name}@{crate_version}",
            }]
        elif source is None and Path(package["manifest_path"]).is_relative_to(ROOT / "vendor"):
            entry["SPDXID"] = spdx_id("vendored", name, crate_version)
            entry["downloadLocation"] = "NOASSERTION"
            entry["comment"] = (
                f"Vendored in {Path(package['manifest_path']).parent.relative_to(ROOT)} "
                "with the patches it describes in BROXSER-PATCH.md."
            )
        elif source is None:
            entry["SPDXID"] = spdx_id("broxser", name, crate_version)
            entry["downloadLocation"] = "NOASSERTION"
            entry["comment"] = "Broxser source in this repository; not licensed, all rights reserved (NOTICE.md)."
        else:
            raise SystemExit(f"unexpected source for {name}: {source}")
        ids[package_id] = entry["SPDXID"]
        spdx_packages.append(entry)

    manifest = json.loads((ROOT / "runtime" / "helium-linux-x86_64.json").read_text())
    helium_id = spdx_id("runtime", "helium", manifest["version"])
    spdx_packages.append({
        "SPDXID": helium_id,
        "name": "helium",
        "versionInfo": manifest["version"],
        "downloadLocation": manifest["url"],
        "checksums": [{"algorithm": "SHA256", "checksumValue": manifest["sha256"]}],
        "filesAnalyzed": False,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": "NOASSERTION",
        "copyrightText": "NOASSERTION",
        "supplier": "NOASSERTION",
        "comment": (
            f"Chromium {manifest['chromium_version']}. Not contained in the package: "
            "scripts/fetch-helium.sh downloads it and verifies this checksum. Helium's own "
            "code is GPL-3.0; Chromium's components keep their own licenses (NOTICE.md)."
        ),
    })

    relationships = [
        {"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES", "relatedSpdxElement": ids[root]}
        for root in sorted(roots, key=lambda pid: packages[pid]["name"])
    ]
    relationships += [
        {"spdxElementId": ids[a], "relationshipType": "DEPENDS_ON", "relatedSpdxElement": ids[b]}
        for a, b in sorted(edges, key=lambda edge: (ids[edge[0]], ids[edge[1]]))
    ]
    engine = next(pid for pid in packages if packages[pid]["name"] == "broxser-engine")
    relationships.append({
        "spdxElementId": ids[engine],
        "relationshipType": "DEPENDS_ON",
        "relatedSpdxElement": helium_id,
        "comment": "Runtime: the engine launches this browser; it is not linked or bundled.",
    })
    document = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"broxser-{workspace_version}-linux-x86_64",
        "documentNamespace": f"https://broxser.invalid/spdx/{workspace_version}/{commit}",
        "creationInfo": {
            "created": created,
            "creators": ["Tool: broxser-scripts-sbom.py"],
            "comment": f"Commit {commit}; target {TARGET}; normal dependencies of {', '.join(ROOTS)}.",
        },
        "packages": spdx_packages,
        "relationships": relationships,
    }
    return document, {ids[package_id]: package for package_id, package in packages.items()}


def check(document):
    """Structural checks that a consumer relies on."""
    problems = []
    ids = [package["SPDXID"] for package in document["packages"]]
    if len(ids) != len(set(ids)):
        problems.append("duplicate SPDXIDs")
    known = set(ids) | {"SPDXRef-DOCUMENT"}
    for relation in document["relationships"]:
        for key in ("spdxElementId", "relatedSpdxElement"):
            if relation[key] not in known:
                problems.append(f"relationship names unknown {relation[key]}")
    for package in document["packages"]:
        if not re.fullmatch(r"SPDXRef-[A-Za-z0-9.-]+", package["SPDXID"]):
            problems.append(f"invalid SPDXID {package['SPDXID']}")
        if not valid_expression(package["licenseDeclared"]):
            problems.append(
                f"{package['name']}: license {package['licenseDeclared']!r} is no SPDX expression; "
                "map it to SPDX identifiers in license_expression() in scripts/sbom.py"
            )
        if package["downloadLocation"].startswith("https://crates.io/") and not package.get("checksums"):
            problems.append(f"{package['name']}: no checksum")
    return problems


def summary(document):
    by_license = collections.defaultdict(list)
    for package in document["packages"]:
        by_license[package["licenseDeclared"]].append(f"{package['name']} {package['versionInfo']}")
    lines = [
        f"# Third-party components of {document['name']}",
        "",
        "Generated from the SBOM (`sbom.spdx.json`). Declared licenses as the crates state",
        "them; `THIRD-PARTY-LICENSES.txt` carries their license and notice texts. An",
        "inventory for the license owner's review, not a legal conclusion.",
        "",
        f"{len(document['packages'])} components, by declared license:",
        "",
    ]
    for license_, names in sorted(by_license.items(), key=lambda item: (-len(item[1]), item[0])):
        lines.append(f"- {license_} ({len(names)}): {', '.join(sorted(names))}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--output", type=Path)
    parser.add_argument("--summary", type=Path)
    parser.add_argument("--licenses", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    document, packages = build_document()
    problems = check(document)
    bundle, texts, license_problems = license_bundle(document, packages)
    problems += license_problems
    if problems:
        print("\n".join(problems), file=sys.stderr)
        raise SystemExit(1)
    if args.output:
        args.output.write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n")
    if args.summary:
        args.summary.write_text(summary(document))
    if args.licenses:
        args.licenses.write_text(bundle, encoding="utf-8")
    if args.check or not (args.output or args.summary or args.licenses):
        crates = sum(1 for p in document["packages"] if p["downloadLocation"].startswith("https://crates.io/"))
        print(f"SBOM ok: {len(document['packages'])} components ({crates} crates.io crates), "
              f"{len(document['relationships'])} relationships; {texts} distinct license texts")


if __name__ == "__main__":
    main()
