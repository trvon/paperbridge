#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?Usage: $0 v0.1.0}"
FORMULA="Formula/paperbridge.rb"
REPO="trvon/paperbridge"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu)
BASE="https://github.com/${REPO}/releases/download/${VERSION}"

declare -A SHAS

for target in "${TARGETS[@]}"; do
    ARCHIVE="paperbridge-${VERSION}-${target}.tar.gz"
    URL="${BASE}/${ARCHIVE}"
    echo "Fetching ${ARCHIVE}..."
    SHA=$(curl -fsSL "$URL" | shasum -a 256 | awk '{print $1}')
    SHAS["$target"]="$SHA"
    echo "  ${target}: ${SHA}"
done

echo ""
echo "Patching ${FORMULA}..."

# Build sed expressions to replace PLACEHOLDER or existing sha256 values
# We match the url line for each target, then replace the sha256 on the next line
for target in "${TARGETS[@]}"; do
    SHA="${SHAS[$target]}"
    # Use perl for reliable multi-line matching
    perl -i -0pe "s|(paperbridge-v.*?-${target}\.tar\.gz\"\n\s+sha256 \")([a-fA-F0-9]+\|PLACEHOLDER)(\")|\\1${SHA}\\3|g" "$FORMULA"
done

echo "Done. Verify with: git diff ${FORMULA}"
