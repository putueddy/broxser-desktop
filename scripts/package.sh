#!/usr/bin/env bash
# Builds the unsigned Linux x86_64 release archive (P3.1, ADR 0025): the release
# binaries, the notices, the SBOM and license inventory, the Helium manifest and
# its fetch script, a SHA256SUMS of every file, and next to the archive its own
# SHA-256. Helium itself is not included; scripts/fetch-helium.sh in the archive
# downloads and verifies it. The archive is deterministic for a commit and its
# binaries: order, owners, modes and times come from the commit, not the build.
#
#   bash scripts/package.sh [output directory, default artifacts/release]
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
umask 022
out=${1:-artifacts/release}
if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo 'The release archive is Linux x86_64 only.' >&2
  exit 1
fi
if [[ -n $(git status --porcelain --untracked-files=no) ]]; then
  echo 'Commit or stash tracked changes first: the archive must match a commit.' >&2
  exit 1
fi
version=$(cargo metadata --format-version 1 --no-deps --locked | python3 -c '
import json, sys
print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "broxser-desktop"))')
commit=$(git rev-parse HEAD)
epoch=$(git log -1 --format=%ct)
name="broxser-$version-linux-x86_64"

cargo build --release --locked -p broxser-desktop -p broxser-cli

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
root=$stage/$name
mkdir -p "$root/bin" "$root/licenses" "$root/runtime" "$root/scripts"
install -m 0755 target/release/broxser-desktop target/release/broxser "$root/bin/"
install -m 0644 README.md NOTICE.md SECURITY.md "$root/"
install -m 0644 runtime/helium-linux-x86_64.json "$root/runtime/"
install -m 0755 scripts/fetch-helium.sh "$root/scripts/"
install -m 0644 vendor/gpui-0.2.2/LICENSE-APACHE "$root/licenses/gpui-LICENSE-APACHE"
python3 scripts/sbom.py --output "$root/sbom.spdx.json" --summary "$root/THIRD-PARTY.md"
printf '%s\n' "$commit" > "$root/COMMIT"
(cd "$root" && find . -type f ! -name SHA256SUMS -printf '%P\n' | LC_ALL=C sort | xargs sha256sum > SHA256SUMS)
find "$root" -exec touch --no-dereference --date="@$epoch" {} +

mkdir -p "$out"
tar --sort=name --format=posix --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
  --pax-option=exthdr.name=%d/PaxHeaders/%f,delete=atime,delete=ctime \
  -C "$stage" -cf - "$name" | xz -9 -T1 > "$out/$name.tar.xz"
(cd "$out" && sha256sum "$name.tar.xz" > "$name.tar.xz.sha256")
echo "Built $out/$name.tar.xz (unsigned; commit $commit)"
cat "$out/$name.tar.xz.sha256"
echo "After unpacking: scripts/fetch-helium.sh, then BROXSER_HELIUM_BIN=\$PWD/.local/helium/helium bin/broxser-desktop"
