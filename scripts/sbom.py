#!/usr/bin/env python3
"""Software bill of materials for the Linux x86_64 build (P3.1, ADR 0025).

Writes an SPDX 2.3 JSON document for the shipped binaries `broxser-desktop`
and `broxser`: every crate they link on x86_64-unknown-linux-gnu (normal
dependencies only; build tools are not shipped), with its version, declared
license, crates.io download location and the SHA-256 that Cargo.lock pins,
the vendored GPUI, and Helium as a runtime dependency that is fetched and
verified separately and not contained in the package. Output is deterministic
for a commit: packages are sorted and the creation time is the commit's.

    python3 scripts/sbom.py --output sbom.spdx.json [--summary licenses.md]
    python3 scripts/sbom.py --check   # validate without writing
"""

import argparse
import collections
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

TARGET = "x86_64-unknown-linux-gnu"
ROOTS = ("broxser-desktop", "broxser-cli")
ROOT = Path(__file__).resolve().parent.parent


def run(*command):
    return subprocess.run(command, cwd=ROOT, check=True, capture_output=True, text=True).stdout


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
        depth, key = int(match.group(1)), (match.group(2), match.group(3))
        if key not in by_name_version:
            raise SystemExit(f"cargo tree lists {key}, which cargo metadata does not know")
        package = by_name_version[key]
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
    created = run("git", "log", "-1", "--format=%cd", "--date=format-local:%Y-%m-%dT%H:%M:%SZ").strip()
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
            entry["comment"] = "Broxser source in this repository; no distribution license chosen yet (NOTICE.md)."
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
    return {
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
            problems.append(f"{package['name']}: license {package['licenseDeclared']!r} is no SPDX expression")
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
        "them; this is an inventory for the license owner's review, not a legal conclusion.",
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
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    document = build_document()
    problems = check(document)
    if problems:
        print("\n".join(problems), file=sys.stderr)
        raise SystemExit(1)
    if args.output:
        args.output.write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n")
    if args.summary:
        args.summary.write_text(summary(document))
    if args.check or not (args.output or args.summary):
        crates = sum(1 for p in document["packages"] if p["downloadLocation"].startswith("https://crates.io/"))
        print(f"SBOM ok: {len(document['packages'])} components ({crates} crates.io crates), "
              f"{len(document['relationships'])} relationships")


if __name__ == "__main__":
    main()
