#!/usr/bin/env python3
"""Qualifies a Helium release before it is pinned (P3.1, ADR 0026).

    python3 scripts/qualify-helium.py propose VERSION [--output FILE]
    python3 scripts/qualify-helium.py verify MANIFEST
    python3 scripts/qualify-helium.py run MANIFEST

`propose` downloads the Linux x86_64 tarball of a Helium release and its
detached signature, accepts it only when Helium's signing key signed it (the
key in runtime/helium-signing-key.asc, pinned by fingerprint below), and writes
a manifest with its SHA-256 and the versions the browser reports. It unpacks
the browser into .local/helium-candidates/VERSION, beside the pinned one.

`verify` checks that a manifest's tarball still carries that signature and its
SHA-256, without unpacking it.

`run` prepares the manifest's browser in the same place (signature and SHA-256
checked again), checks the versions it reports, runs the live Helium suite and
the X11 desktop smoke against it and writes a record, with the logs, to
artifacts/qualification. Nothing here changes runtime/helium-linux-x86_64.json:
promoting a release, or rolling back to an earlier one, is a reviewed commit of
that file.

`run` needs an unprivileged user: the browser sandbox stays enabled.
BROXSER_ENGINE_TESTS and BROXSER_DESKTOP_BIN may name prebuilt binaries (the
engine's test binary; a release archive's desktop), otherwise cargo builds
them. The smoke needs DISPLAY (an X11 session or Xvfb); without it the record
says "incomplete", never "qualified".
"""

import argparse
import datetime
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PINNED = ROOT / "runtime" / "helium-linux-x86_64.json"
SIGNING_KEY = ROOT / "runtime" / "helium-signing-key.asc"
# Helium's release signing key as imputnet/helium-linux publishes it (pubkey.asc
# and the README's "Signature" section). Changing it is a reviewed decision.
SIGNING_FINGERPRINT = "BE677C1989D35EAB2C5F26C9351601AD01D6378E"
RELEASES = "https://github.com/imputnet/helium-linux/releases"
CANDIDATES = ROOT / ".local" / "helium-candidates"
RECORDS = ROOT / "artifacts" / "qualification"
MARKER = ".broxser-runtime.json"
VERIFIED = ".broxser-verified.json"
RELEASE_VERSION = re.compile(r"\d+(?:\.\d+){3}")
REPORTED_VERSION = re.compile(r"Helium (\S+) \(Chromium (\S+)\)")
TEST_RESULT = re.compile(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored")
FAILED_TEST = re.compile(r"^test (\S+) \.\.\. FAILED$", re.MULTILINE)


class Refused(Exception):
    """A release, manifest or run that must not be qualified."""


def release_urls(version):
    return (
        f"{RELEASES}/tag/{version}",
        f"{RELEASES}/download/{version}/helium-{version}-x86_64_linux.tar.xz",
    )


def check_manifest(manifest):
    """What is wrong with a runtime manifest; empty when it can be qualified."""
    if not isinstance(manifest, dict):
        return ["a manifest is a JSON object"]
    problems = []
    if manifest.get("platform") != "linux-x86_64":
        problems.append("platform must be linux-x86_64")
    version = manifest.get("version")
    if not isinstance(version, str) or not RELEASE_VERSION.fullmatch(version):
        problems.append(f"version {version!r} is not a Helium release version")
    else:
        release, url = release_urls(version)
        if manifest.get("url") != url:
            problems.append(f"url must be {url}")
        if manifest.get("release_url") != release:
            problems.append(f"release_url must be {release}")
    chromium = manifest.get("chromium_version")
    if not isinstance(chromium, str) or not RELEASE_VERSION.fullmatch(chromium):
        problems.append(f"chromium_version {chromium!r} is not a Chromium version")
    digest = manifest.get("sha256")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        problems.append("sha256 must be 64 lowercase hexadecimal digits")
    return problems


def reported_version(output):
    """(Helium version, Chromium version) from `helium --version`."""
    match = REPORTED_VERSION.search(output)
    if not match:
        raise Refused(f"unexpected --version output {output.strip()!r}")
    return match.group(1), match.group(2)


def test_counts(output):
    """Passed, failed and ignored tests summed over cargo's result lines."""
    counts = [0, 0, 0]
    for match in TEST_RESULT.finditer(output):
        counts = [total + int(value) for total, value in zip(counts, match.groups())]
    return {**dict(zip(("passed", "failed", "ignored"), counts)),
            "failures": FAILED_TEST.findall(output)}


def verdict(steps):
    results = [step["result"] for step in steps]
    if any(result != "passed" for result in results if result != "skipped"):
        return "not qualified"
    return "incomplete" if "skipped" in results else "qualified"


def utc(timestamp):
    return datetime.datetime.fromtimestamp(timestamp, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def keyring(key, fingerprint, scratch):
    """A gpgv keyring holding only `key`, after checking that its one primary
    key has `fingerprint`."""
    home = Path(scratch) / "gnupg"
    home.mkdir(mode=0o700)
    env = {**os.environ, "GNUPGHOME": str(home)}
    records = [line.split(":") for line in subprocess.run(
        ["gpg", "--batch", "--with-colons", "--show-keys", str(key)],
        env=env, capture_output=True, text=True, check=True,
    ).stdout.splitlines()]
    primaries = [i for i, fields in enumerate(records) if fields[0] == "pub"]
    # The first fingerprint after the one primary key is that key's own.
    found = next((fields[9] for fields in records[primaries[0] + 1:] if fields[0] == "fpr"),
                 None) if len(primaries) == 1 else None
    if found != fingerprint:
        raise Refused(f"{key} is not the one key with fingerprint {fingerprint}")
    ring = Path(scratch) / "signing-key.gpg"
    ring.write_bytes(subprocess.run(
        ["gpg", "--batch", "--dearmor"], input=Path(key).read_bytes(),
        env=env, capture_output=True, check=True,
    ).stdout)
    return ring


def verify_signature(data, signature, ring, fingerprint):
    """Accepts one good signature over `data` by the primary key `fingerprint`
    in `ring` and returns when it was made."""
    result = subprocess.run(
        ["gpgv", "--status-fd", "1", "--keyring", str(ring), str(signature), str(data)],
        capture_output=True, text=True,
    )
    status = [fields for fields in (line.split()[1:] for line in result.stdout.splitlines()
                                    if line.startswith("[GNUPG:] ")) if fields]
    refusals = [fields[0] for fields in status
                if fields[0] in ("BADSIG", "ERRSIG", "EXPSIG", "EXPKEYSIG", "REVKEYSIG")]
    valid = [fields for fields in status if fields[0] == "VALIDSIG"]
    if result.returncode != 0 or refusals or len(valid) != 1:
        reason = " ".join(refusals) or result.stderr.strip() or "no valid signature"
        raise Refused(f"signature of {Path(data).name} not accepted: {reason}")
    # VALIDSIG <fpr> <date> <timestamp> <expire> <version> <reserved> <pk-algo>
    # <hash-algo> <class> [<primary-fpr>]
    fields = valid[0]
    primary = fields[10] if len(fields) > 10 else fields[1]
    if primary != fingerprint:
        raise Refused(f"{Path(data).name} is signed by {primary}, not by {fingerprint}")
    return utc(int(fields[3]))


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(url, path):
    subprocess.run(
        ["curl", "--fail", "--silent", "--show-error", "--location", "--proto", "=https",
         "--tlsv1.2", "--output", str(path), url],
        check=True,
    )


def browser_version(executable):
    output = subprocess.run(
        [str(executable), "--version"], capture_output=True, text=True, timeout=30,
    ).stdout
    return reported_version(output)


def fetch_verified(url, expected, scratch):
    """Downloads a tarball and its signature into `scratch` and checks both.
    Returns the tarball and what was verified."""
    ring = keyring(SIGNING_KEY, SIGNING_FINGERPRINT, scratch)
    tarball = Path(scratch) / "helium.tar.xz"
    signature = Path(scratch) / "helium.tar.xz.asc"
    download(url, tarball)
    download(url + ".asc", signature)
    signed_at = verify_signature(tarball, signature, ring, SIGNING_FINGERPRINT)
    digest = sha256(tarball)
    if expected is not None and digest != expected:
        raise Refused(f"SHA-256 {digest} differs from the manifest's {expected}")
    return tarball, {"sha256": digest, "signed_by": SIGNING_FINGERPRINT, "signed_at": signed_at}


def prepare(version, url, expected, directory, manifest_text=None):
    """Unpacks a verified release into `directory`, beside the pinned engine.
    `manifest_text` becomes its marker; without it the caller writes one."""
    CANDIDATES.mkdir(parents=True, exist_ok=True)
    if directory.exists():
        raise Refused(f"{directory} already holds a browser; move it aside first")
    with tempfile.TemporaryDirectory(dir=CANDIDATES) as scratch:
        tarball, verified = fetch_verified(url, expected, scratch)
        unpacked = Path(scratch) / "runtime"
        unpacked.mkdir()
        subprocess.run(["tar", "-xJf", str(tarball), "--strip-components=1", "-C", str(unpacked)], check=True)
        if not os.access(unpacked / "helium", os.X_OK):
            raise Refused("the tarball holds no executable helium")
        reported = browser_version(unpacked / "helium")
        if reported[0] != version:
            raise Refused(f"the browser reports Helium {reported[0]}, not {version}")
        (unpacked / VERIFIED).write_text(json.dumps(verified, indent=2) + "\n")
        if manifest_text is not None:
            (unpacked / MARKER).write_text(manifest_text)
        unpacked.rename(directory)
    return verified, reported


def propose(version, output):
    if not RELEASE_VERSION.fullmatch(version):
        raise Refused(f"{version!r} is not a Helium release version")
    release, url = release_urls(version)
    directory = CANDIDATES / version
    verified, (_, chromium) = prepare(version, url, None, directory)
    manifest = {
        "platform": "linux-x86_64",
        "version": version,
        "chromium_version": chromium,
        "release_url": release,
        "url": url,
        "sha256": verified["sha256"],
        "reviewed_on": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d"),
        "purpose": json.loads(PINNED.read_text())["purpose"],
    }
    text = json.dumps(manifest, indent=2) + "\n"
    output = output or ROOT / "artifacts" / "helium" / f"helium-linux-x86_64-{version}.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(text)
    (directory / MARKER).write_text(text)
    print(f"Helium {version} (Chromium {chromium}) signed by {verified['signed_by']} "
          f"at {verified['signed_at']}; SHA-256 {verified['sha256']}.")
    print(f"Manifest: {output}\nBrowser: {directory / 'helium'}")
    print(f"Next: python3 scripts/qualify-helium.py run {output}")


def load_manifest(path):
    text = Path(path).read_text()
    manifest = json.loads(text)
    problems = check_manifest(manifest)
    if problems:
        raise Refused(f"{path}: " + "; ".join(problems))
    return text, manifest


def verify(path):
    _, manifest = load_manifest(path)
    with tempfile.TemporaryDirectory() as scratch:
        _, verified = fetch_verified(manifest["url"], manifest["sha256"], scratch)
    print(f"Helium {manifest['version']}: signed by {verified['signed_by']} at "
          f"{verified['signed_at']}; SHA-256 matches {path}.")


def logged(command, log, env, cwd=ROOT):
    started = time.monotonic()
    with open(log, "w") as out:
        code = subprocess.run(command, stdout=out, stderr=subprocess.STDOUT,
                              env={**os.environ, **env}, cwd=cwd).returncode
    return code, round(time.monotonic() - started, 1), Path(log).read_text(errors="replace")


def os_name():
    try:
        lines = Path("/etc/os-release").read_text().splitlines()
    except OSError:
        return "unknown"
    return next((line.split("=", 1)[1].strip('"') for line in lines
                 if line.startswith("PRETTY_NAME=")), "unknown")


def step(name, result, detail, seconds=None):
    entry = {"name": name, "result": result, "detail": detail}
    if seconds is not None:
        entry["seconds"] = seconds
    return entry


def run(path):
    if os.geteuid() == 0:
        raise Refused("run as an unprivileged user: Chromium refuses root with its sandbox enabled")
    text, manifest = load_manifest(path)
    version = manifest["version"]
    directory = CANDIDATES / version
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    RECORDS.mkdir(parents=True, exist_ok=True)
    base = RECORDS / f"helium-{version}-{stamp}"
    steps = []

    marker = directory / MARKER
    if marker.is_file() and marker.read_text() == text and (directory / VERIFIED).is_file():
        verified = json.loads((directory / VERIFIED).read_text())
        detail = "prepared earlier from this manifest"
    else:
        verified, _ = prepare(version, manifest["url"], manifest["sha256"], directory, text)
        detail = "downloaded"
    steps.append(step("prepare", "passed",
                      f"{detail}; signed by {verified['signed_by']} at {verified['signed_at']}; "
                      f"SHA-256 {verified['sha256']}"))
    helium = directory / "helium"
    reported = browser_version(helium)
    matches = reported == (version, manifest["chromium_version"])
    steps.append(step("version", "passed" if matches else "failed",
                      f"Helium {reported[0]} (Chromium {reported[1]})"))

    logs, desktop = [], None
    if matches:
        engine_tests = os.environ.get("BROXSER_ENGINE_TESTS")
        live = ([engine_tests, "--ignored", "--test-threads=4"] if engine_tests else
                ["cargo", "test", "--locked", "-p", "broxser-engine", "--", "--ignored", "--test-threads=4"])
        log = f"{base}-live.log"
        code, seconds, output = logged(live, log, {"BROXSER_TEST_BROWSER": str(helium)})
        counts = test_counts(output)
        passed = code == 0 and counts["failed"] == 0 and counts["passed"] > 0
        failures = "".join(f"; failed {name}" for name in counts["failures"])
        steps.append(step("live Helium suite", "passed" if passed else "failed",
                          f"{counts['passed']} passed, {counts['failed']} failed{failures}", seconds))
        logs.append(log)

        if not os.environ.get("DISPLAY"):
            steps.append(step("desktop smoke", "skipped", "DISPLAY is not set"))
        else:
            desktop = os.environ.get("BROXSER_DESKTOP_BIN")
            if not desktop:
                subprocess.run(["cargo", "build", "--locked", "-p", "broxser-desktop"], cwd=ROOT, check=True)
                desktop = str(ROOT / "target" / "debug" / "broxser-desktop")
            log = f"{base}-smoke.log"
            code, seconds, output = logged(
                ["bash", "scripts/desktop-smoke.sh"], log,
                {"BROXSER_HELIUM_BIN": str(helium), "BROXSER_DESKTOP_BIN": desktop},
            )
            passed = code == 0 and "desktop smoke passed" in output
            steps.append(step("desktop smoke", "passed" if passed else "failed",
                              (output.strip().splitlines() or [""])[-1], seconds))
            logs.append(log)

    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True)
    changes = subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"],
                             cwd=ROOT, capture_output=True, text=True)
    pinned = json.loads(PINNED.read_text())
    record = {
        "kind": "broxser-helium-qualification",
        "created": utc(time.time()),
        "verdict": verdict(steps),
        "manifest": manifest,
        "pinned": {"version": pinned["version"], "sha256": pinned["sha256"]},
        "verified": verified,
        "broxser": {
            "commit": commit.stdout.strip() or "unknown",
            "tracked_changes": bool(changes.stdout.strip()),
            "engine_tests": os.environ.get("BROXSER_ENGINE_TESTS", "cargo test"),
            "desktop": desktop,
        },
        "host": {"system": f"{platform.system()} {platform.release()} {platform.machine()}",
                 "os": os_name()},
        "steps": steps,
        "logs": [str(Path(log).relative_to(ROOT)) for log in logs],
    }
    Path(f"{base}.json").write_text(json.dumps(record, indent=2) + "\n")
    print(f"Helium {version} (Chromium {manifest['chromium_version']}): {record['verdict']}")
    for entry in steps:
        seconds = f" ({entry['seconds']} s)" if "seconds" in entry else ""
        print(f"  {entry['name']}: {entry['result']}{seconds}: {entry['detail']}")
    print(f"Record: {base}.json")
    return record["verdict"] == "qualified"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    proposal = commands.add_parser("propose", help="write a signature-verified manifest for a release")
    proposal.add_argument("version")
    proposal.add_argument("--output", type=Path)
    commands.add_parser("verify", help="check a manifest's signature and SHA-256").add_argument("manifest")
    commands.add_parser("run", help="qualify a manifest's browser and write a record").add_argument("manifest")
    args = parser.parse_args()
    for tool in ("curl", "gpg", "gpgv", "tar"):
        if shutil.which(tool) is None:
            raise SystemExit(f"{tool} is required")
    try:
        if args.command == "propose":
            propose(args.version, args.output)
        elif args.command == "verify":
            verify(args.manifest)
        elif not run(args.manifest):
            raise SystemExit(1)
    except (Refused, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        raise SystemExit(f"Refused: {error}")


if __name__ == "__main__":
    main()
