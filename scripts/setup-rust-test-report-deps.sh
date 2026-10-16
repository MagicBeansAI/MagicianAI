#!/usr/bin/env bash
set -euo pipefail

CARGO_NEXTEST_VERSION="${CARGO_NEXTEST_VERSION:-0.9.140}"
CARGO_LLVM_COV_VERSION="${CARGO_LLVM_COV_VERSION:-0.8.7}"

installed_version() {
    local cargo_subcommand="$1"
    cargo "$cargo_subcommand" --version 2>/dev/null | awk 'NR == 1 { print $2 }'
}

install_cargo_tool() {
    local package="$1"
    local cargo_subcommand="$2"
    local expected_version="$3"
    local current_version

    current_version="$(installed_version "$cargo_subcommand" || true)"
    if [[ "$current_version" == "$expected_version" ]]; then
        echo "✅ $package $expected_version is already installed."
        return
    fi

    if [[ -n "$current_version" ]]; then
        echo "Updating $package from $current_version to $expected_version..."
    else
        echo "Installing $package $expected_version..."
    fi
    cargo install "$package" --locked --version "$expected_version"
}

if ! command -v cargo >/dev/null 2>&1 || ! command -v rustup >/dev/null 2>&1; then
    echo "❌ cargo and rustup are required. Install Rust with rustup, then rerun this script." >&2
    exit 1
fi

echo "Installing the LLVM tools used by Rust source coverage..."
rustup component add llvm-tools-preview

install_cargo_tool cargo-nextest nextest "$CARGO_NEXTEST_VERSION"
install_cargo_tool cargo-llvm-cov llvm-cov "$CARGO_LLVM_COV_VERSION"

echo "✅ Rust test-report dependencies are installed."
