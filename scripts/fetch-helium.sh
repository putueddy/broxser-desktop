#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo 'This baseline is Linux x86_64 only. Use an official build and BROXSER_HELIUM_BIN for another architecture.' >&2
  exit 1
fi
readarray -t metadata < <(python3 -c 'import json; d=json.load(open("runtime/helium-linux-x86_64.json")); print(d["url"]); print(d["sha256"])')
if [[ ${#metadata[@]} -ne 2 ]]; then
  echo 'Invalid runtime manifest' >&2
  exit 1
fi
if [[ -f .local/helium/.broxser-runtime.json ]] && cmp -s runtime/helium-linux-x86_64.json .local/helium/.broxser-runtime.json; then
  if [[ -x .local/helium/helium ]]; then
    echo "Helium already prepared: $PWD/.local/helium/helium"
    exit 0
  fi
fi
if [[ -e .local/helium ]]; then
  echo 'Existing .local/helium differs from this baseline. Move it aside before preparing another version.' >&2
  exit 1
fi
mkdir -p .local
scratch=$(mktemp -d .local/helium-fetch.XXXXXXXX)
trap 'rm -rf -- "$scratch"' EXIT
curl --fail --location --proto '=https' --tlsv1.2 --output "$scratch/helium.tar.xz" "${metadata[0]}"
printf '%s  %s\n' "${metadata[1]}" "$scratch/helium.tar.xz" | sha256sum --check --status
mkdir "$scratch/runtime"
tar -xJf "$scratch/helium.tar.xz" --strip-components=1 -C "$scratch/runtime"
test -x "$scratch/runtime/helium"
cp runtime/helium-linux-x86_64.json "$scratch/runtime/.broxser-runtime.json"
mv "$scratch/runtime" .local/helium
echo "Helium prepared: $PWD/.local/helium/helium"
echo 'Set BROXSER_HELIUM_BIN to this path. The browser is not installed system-wide.'
