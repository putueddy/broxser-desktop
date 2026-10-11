#!/usr/bin/env bash
# Prepares the assets of a draft GitHub release (ADR 0025, decisions 2 and 3).
# It builds the archive twice with scripts/package.sh and requires one SHA-256,
# checks it as a recipient would (its .sha256, then the SHA256SUMS of the
# unpacked files and the COMMIT they name), and writes the SBOM and the release
# notes beside it. .github/workflows/release.yml then attests the archive and
# creates the draft; this script signs and uploads nothing.
#
#   bash scripts/release-assets.sh [v<workspace version>]
#
# Without a tag it prepares v<workspace version>, as the pull request dry run
# does. The assets go to artifacts/release/<tag>, which must not exist yet.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
version=$(cargo metadata --format-version 1 --no-deps --locked | python3 -c '
import json, sys
print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "broxser-desktop"))')
tag=${1-v$version}
if [[ $tag != "v$version" ]]; then
  echo "The tag $tag does not name the workspace version $version: tag v$version, or change the version in Cargo.toml first." >&2
  exit 1
fi
out=artifacts/release/$tag
if [[ -e $out ]]; then
  echo "$out exists; move it aside first." >&2
  exit 1
fi
name="broxser-$version-linux-x86_64"
archive="$out/$name.tar.xz"
commit=$(git rev-parse HEAD)

bash scripts/package.sh "$out"
first=$(sha256sum < "$archive")
bash scripts/package.sh "$out"
if [[ $(sha256sum < "$archive") != "$first" ]]; then
  echo 'Two builds of this commit gave different archives.' >&2
  exit 1
fi

check=$(mktemp -d)
trap 'rm -rf -- "$check"' EXIT
(cd "$out" && sha256sum --check --quiet --strict "$name.tar.xz.sha256")
tar -xJf "$archive" -C "$check"
(cd "$check/$name" && sha256sum --check --quiet --strict SHA256SUMS)
listed=$(cut -c67- "$check/$name/SHA256SUMS" | LC_ALL=C sort)
present=$(cd "$check/$name" && find . -type f ! -name SHA256SUMS -printf '%P\n' | LC_ALL=C sort)
if [[ $listed != "$present" ]]; then
  echo 'SHA256SUMS does not list exactly the files of the archive.' >&2
  exit 1
fi
if [[ $(<"$check/$name/COMMIT") != "$commit" ]]; then
  echo "The archive's COMMIT does not name $commit." >&2
  exit 1
fi

cp "$check/$name/sbom.spdx.json" "$out/$name.spdx.json"
helium=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' \
  "$check/$name/runtime/helium-linux-x86_64.json")
sha256=$(cut -d' ' -f1 "$out/$name.tar.xz.sha256")
repository=${GITHUB_REPOSITORY:-putueddy/broxser-desktop}
cat > "$out/release-notes.md" <<EOF
Broxser $version for Linux x86_64, built by the release workflow from commit
\`$commit\`.

**For the company's pilot users only.** Broxser's own code is not licensed:
all rights reserved (\`NOTICE.md\`). Do not publish this release or pass it on
outside the pilot (ADR 0025).

| Asset | Content |
| --- | --- |
| \`$name.tar.xz\` | The binaries, notices, SBOM, third-party license texts, the Helium manifest and its fetch script, and \`SHA256SUMS\` |
| \`$name.tar.xz.sha256\` | Its SHA-256: \`$sha256\` |
| \`$name.tar.xz.sigstore.json\` | Its build provenance attestation (a Sigstore bundle) |
| \`$name.spdx.json\` | The SBOM, also inside the archive |

Check the archive before use. With the GitHub CLI, the attestation shows that
this repository's release workflow built it from this tag and commit:

\`\`\`sh
sha256sum -c $name.tar.xz.sha256
gh attestation verify $name.tar.xz --repo $repository \\
  --signer-workflow $repository/.github/workflows/release.yml \\
  --source-ref refs/tags/$tag --source-digest $commit --deny-self-hosted-runners
\`\`\`

To check against the attached bundle rather than GitHub's attestation API, add
\`--bundle $name.tar.xz.sigstore.json\`.

Helium $helium is not included. After unpacking, \`scripts/fetch-helium.sh\`
downloads it and checks its SHA-256; then run
\`BROXSER_HELIUM_BIN=\$PWD/.local/helium/helium bin/broxser-desktop\`.
EOF
echo "Release assets of $tag in $out (archive SHA-256 $sha256):"
ls -l "$out"
