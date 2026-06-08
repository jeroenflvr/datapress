#!/bin/sh
# shellcheck shell=sh
#
# datapress-cli installer for Linux and macOS.
#
#   curl -LsSf https://datap-rs.org/install-cli.sh | sh
#
# Downloads the prebuilt `datapress-cli` command-line client from the GitHub
# release, verifies its SHA-256 checksum, and installs it into a per-user
# directory (no sudo). It never edits your shell profile; if the install
# directory is not on your PATH it prints the exact line to add.
#
# `datapress-cli` is the standalone HTTP *client* for a DataPress server
# (built on the datapress-client crate). For the *server* binary, use
# install.sh instead.
#
# Environment overrides:
#   DATAPRESS_CLI_VERSION      Version/tag to install (e.g. "0.4.11" or
#                              "v0.4.11"). Defaults to the latest release.
#   DATAPRESS_CLI_INSTALL_DIR  Directory to install the binary into.
#                              Defaults to $XDG_BIN_HOME, else $HOME/.local/bin.
#   DATAPRESS_NO_MODIFY_PATH   If set, suppresses the PATH hint (it is never
#                              modified automatically regardless).
#
# Flags:
#   --version <V>     same as DATAPRESS_CLI_VERSION
#   --bin-dir <DIR>   same as DATAPRESS_CLI_INSTALL_DIR
#   --help            show this help

set -eu

REPO="jeroenflvr/datapress"
BIN_NAME="datapress-cli"

main() {
    VERSION="${DATAPRESS_CLI_VERSION:-}"
    INSTALL_DIR="${DATAPRESS_CLI_INSTALL_DIR:-}"

    # ---- parse flags -----------------------------------------------------
    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                VERSION="${2:-}"; shift 2 || err "--version requires an argument" ;;
            --version=*)
                VERSION="${1#*=}"; shift ;;
            --bin-dir)
                INSTALL_DIR="${2:-}"; shift 2 || err "--bin-dir requires an argument" ;;
            --bin-dir=*)
                INSTALL_DIR="${1#*=}"; shift ;;
            -h|--help)
                usage; exit 0 ;;
            *)
                err "unknown option: $1 (try --help)" ;;
        esac
    done

    need_cmd uname
    need_cmd mkdir
    need_cmd chmod
    need_cmd tar

    # ---- resolve install dir --------------------------------------------
    if [ -z "$INSTALL_DIR" ]; then
        if [ -n "${XDG_BIN_HOME:-}" ]; then
            INSTALL_DIR="$XDG_BIN_HOME"
        else
            INSTALL_DIR="$HOME/.local/bin"
        fi
    fi

    # ---- detect platform -------------------------------------------------
    target="$(detect_target)"
    say "Detected platform: $target"

    # ---- resolve version -------------------------------------------------
    if [ -z "$VERSION" ]; then
        VERSION="$(latest_version)"
        [ -n "$VERSION" ] || err "could not determine the latest version; set DATAPRESS_CLI_VERSION"
    fi
    # Normalise to a tag (prefixed with v).
    case "$VERSION" in
        v*) TAG="$VERSION" ;;
        *)  TAG="v$VERSION" ;;
    esac
    say "Installing datapress-cli $TAG"

    # ---- download + verify + extract ------------------------------------
    archive="${BIN_NAME}-${TAG}-${target}.tar.gz"
    base_url="https://github.com/${REPO}/releases/download/${TAG}"

    tmp="$(mktemp -d 2>/dev/null || mktemp -d -t datapress-cli)"
    trap 'rm -rf "$tmp"' EXIT

    say "Downloading $archive"
    download "${base_url}/${archive}" "$tmp/$archive" \
        || err "failed to download ${base_url}/${archive}"

    if download "${base_url}/${archive}.sha256" "$tmp/$archive.sha256" 2>/dev/null; then
        verify_checksum "$tmp/$archive" "$tmp/$archive.sha256"
        say "Checksum OK"
    else
        warn "checksum file not published for this release; skipping verification"
    fi

    tar -xzf "$tmp/$archive" -C "$tmp" || err "failed to extract $archive"
    [ -f "$tmp/$BIN_NAME" ] || err "archive did not contain '$BIN_NAME'"

    mkdir -p "$INSTALL_DIR" || err "could not create $INSTALL_DIR"
    install_bin "$tmp/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
    chmod +x "$INSTALL_DIR/$BIN_NAME"

    say ""
    say "Installed $BIN_NAME to $INSTALL_DIR/$BIN_NAME"

    path_hint "$INSTALL_DIR"
}

usage() {
    sed -n '3,33p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//' || true
    cat <<EOF
Usage: install-cli.sh [--version <V>] [--bin-dir <DIR>]
EOF
}

# Map uname output to a release target triple.
detect_target() {
    _os="$(uname -s)"
    _arch="$(uname -m)"

    case "$_arch" in
        x86_64|amd64)        _arch="x86_64" ;;
        aarch64|arm64)       _arch="aarch64" ;;
        *) err "unsupported architecture: $_arch" ;;
    esac

    case "$_os" in
        Linux)
            echo "${_arch}-unknown-linux-gnu" ;;
        Darwin)
            if [ "$_arch" != "aarch64" ]; then
                err "no prebuilt macOS binary for $_arch. Install on Apple Silicon, or use: cargo install datapress-cli"
            fi
            echo "aarch64-apple-darwin" ;;
        *)
            err "unsupported OS: $_os (Windows users: use install-cli.ps1)" ;;
    esac
}

# Resolve the latest release tag by following the /releases/latest redirect
# (no API token, no jq required).
latest_version() {
    _url="https://github.com/${REPO}/releases/latest"
    if check_cmd curl; then
        _eff="$(curl -sSfL -o /dev/null -w '%{url_effective}' "$_url" 2>/dev/null || true)"
    elif check_cmd wget; then
        _eff="$(wget -q -S -O /dev/null "$_url" 2>&1 | awk '/^  Location: /{print $2}' | tail -n1)"
    else
        err "need curl or wget to resolve the latest version"
    fi
    # Trailing path component is the tag, e.g. .../tag/v0.4.11
    printf '%s\n' "${_eff##*/}"
}

download() {
    # download <url> <dest>
    if check_cmd curl; then
        curl -fSL --proto '=https' --tlsv1.2 -o "$2" "$1"
    elif check_cmd wget; then
        wget -q -O "$2" "$1"
    else
        err "need curl or wget to download files"
    fi
}

verify_checksum() {
    # verify_checksum <file> <sha256-file>
    _file="$1"; _sumfile="$2"
    _expected="$(awk '{print $1}' "$_sumfile" | head -n1)"
    [ -n "$_expected" ] || err "empty checksum file"

    if check_cmd sha256sum; then
        _actual="$(sha256sum "$_file" | awk '{print $1}')"
    elif check_cmd shasum; then
        _actual="$(shasum -a 256 "$_file" | awk '{print $1}')"
    else
        warn "no sha256sum/shasum available; skipping checksum verification"
        return 0
    fi

    if [ "$_expected" != "$_actual" ]; then
        err "checksum mismatch: expected $_expected, got $_actual"
    fi
}

install_bin() {
    # Prefer install(1) for atomic mode-preserving copy; fall back to cp.
    if check_cmd install; then
        install -m 755 "$1" "$2"
    else
        cp -f "$1" "$2"
    fi
}

path_hint() {
    _dir="$1"
    [ -z "${DATAPRESS_NO_MODIFY_PATH:-}" ] || return 0

    case ":${PATH}:" in
        *":${_dir}:"*)
            say "$_dir is already on your PATH. Run: $BIN_NAME --version"
            return 0 ;;
    esac

    _shell_name="$(basename "${SHELL:-sh}")"
    case "$_shell_name" in
        zsh)  _rc="~/.zshrc" ;;
        bash) _rc="~/.bashrc" ;;
        fish) _rc="~/.config/fish/config.fish" ;;
        *)    _rc="your shell profile" ;;
    esac

    say ""
    warn "$_dir is not on your PATH."
    if [ "$_shell_name" = "fish" ]; then
        say "Add it by running:"
        say "    fish_add_path $_dir"
    else
        say "Add it to your PATH by adding this line to $_rc:"
        say "    export PATH=\"$_dir:\$PATH\""
    fi
    say ""
    say "Then restart your shell (or 'source $_rc') and run: $BIN_NAME --version"
}

# ---- small helpers -------------------------------------------------------
say()  { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }
check_cmd() { command -v "$1" >/dev/null 2>&1; }
need_cmd()  { check_cmd "$1" || err "required command not found: $1"; }

main "$@"
