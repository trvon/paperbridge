#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${1:-${HOME}/.local/bin}"
CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME}/.config}"
CONFIG_DIR="${CONFIG_HOME}/paperbridge"
CONFIG_PATH="${CONFIG_DIR}/config.toml"

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

info() {
    printf '==> %s\n' "$*"
}

if ! command -v cargo >/dev/null 2>&1; then
    die "cargo is required. Install Rust from https://rustup.rs and retry."
fi

info "Building paperbridge (release)..."
cargo build --release

info "Installing binary to ${INSTALL_DIR}/"
mkdir -p "${INSTALL_DIR}"
cp "target/release/paperbridge" "${INSTALL_DIR}/paperbridge"

info "Ensuring config directory exists at ${CONFIG_DIR}/"
mkdir -p "${CONFIG_DIR}"

if [[ ! -f "${CONFIG_PATH}" ]]; then
    if [[ "${PAPERBRIDGE_SETUP_NONINTERACTIVE:-0}" == "1" ]]; then
        "${INSTALL_DIR}/paperbridge" config init
    else
        printf "Run interactive config init now? [Y/n]: "
        read -r reply
        if [[ -z "${reply}" || "${reply}" =~ ^[Yy]$ ]]; then
            "${INSTALL_DIR}/paperbridge" config init --interactive
        else
            "${INSTALL_DIR}/paperbridge" config init
        fi
    fi
    info "Initialized ${CONFIG_PATH}"
else
    info "Config already exists, leaving ${CONFIG_PATH} unchanged"
fi

echo ""
echo "==> Next steps"
echo "1) Validate config: ${INSTALL_DIR}/paperbridge config validate"
echo "2) Resolve user id (optional): ${INSTALL_DIR}/paperbridge config resolve-user-id --login <username>"
echo "3) Update values via CLI (example): ${INSTALL_DIR}/paperbridge config set library_type user"
echo ""
echo "==> OpenCode snippet (~/.config/opencode/opencode.json → mcp)"
"${INSTALL_DIR}/paperbridge" config snippet --target opencode --binary-path "${INSTALL_DIR}/paperbridge"
echo ""
echo "==> Pi coding agent CLI snippet"
"${INSTALL_DIR}/paperbridge" config snippet --target pi --binary-path "${INSTALL_DIR}/paperbridge"
echo ""
echo "==> Start MCP server (stdio)"
echo "${INSTALL_DIR}/paperbridge serve"

if ! echo "${PATH}" | tr ':' '\n' | grep -qx "${INSTALL_DIR}"; then
    echo ""
    echo "Note: ${INSTALL_DIR} is not on your PATH."
    echo "  Add it: export PATH=\"${INSTALL_DIR}:\${PATH}\""
fi
