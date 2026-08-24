#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?Usage: $0 v0.1.0}"
FORMULA="Formula/paperbridge.rb"
REPO="trvon/paperbridge"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu)
BASE="https://github.com/${REPO}/releases/download/${VERSION}"

declare -A SHAS

if command -v sha256sum >/dev/null 2>&1; then
    SHA256_COMMAND=(sha256sum)
elif command -v shasum >/dev/null 2>&1; then
    SHA256_COMMAND=(shasum -a 256)
else
    echo "A SHA-256 tool is required (sha256sum or shasum)" >&2
    exit 1
fi

for target in "${TARGETS[@]}"; do
    ARCHIVE="paperbridge-${VERSION}-${target}.tar.gz"
    URL="${BASE}/${ARCHIVE}"
    echo "Fetching ${ARCHIVE}..."
    SHA=$(curl --retry 3 --retry-all-errors -fsSL "$URL" | "${SHA256_COMMAND[@]}" | awk '{print $1}')
    if [[ ! "$SHA" =~ ^[a-fA-F0-9]{64}$ ]]; then
        echo "Invalid SHA-256 for ${ARCHIVE}: ${SHA}" >&2
        exit 1
    fi
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
    perl -i -0pe "s~(paperbridge-v.*?-${target}\.tar\.gz\"\n\s+sha256 \")([a-fA-F0-9]{64}|PLACEHOLDER)(\")~\${1}${SHA}\${3}~g" "$FORMULA"
    if ! grep -Fq "sha256 \"${SHA}\"" "$FORMULA"; then
        echo "Failed to update ${target} SHA in ${FORMULA}" >&2
        exit 1
    fi
done

if grep -Fq 'sha256 "PLACEHOLDER"' "$FORMULA"; then
    echo "Formula still contains SHA placeholders" >&2
    exit 1
fi

echo "Done. Verify with: git diff ${FORMULA}"
