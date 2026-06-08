#!/usr/bin/env bash
#
# giga-harness installer.
#
# Usage:
#   curl -sSf https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.sh | bash
#
# Detects OS+arch, downloads the matching release tarball/zip from
# the latest GitHub release, and drops `giga` into ~/.local/bin
# (creating it if absent). Re-run any time to upgrade.

set -euo pipefail

REPO="mickfixesjunk/giga-harness"
BIN_NAME="giga"
INSTALL_DIR="${GIGA_INSTALL_DIR:-$HOME/.local/bin}"

err() { printf 'error: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

detect_target() {
    local uname_s uname_m
    uname_s="$(uname -s)"
    uname_m="$(uname -m)"

    case "$uname_s" in
        Linux*)
            case "$uname_m" in
                x86_64|amd64) echo "x86_64-unknown-linux-musl" ;;
                *) err "unsupported linux arch: $uname_m (only x86_64 published)" ;;
            esac
            ;;
        Darwin*)
            case "$uname_m" in
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                x86_64) echo "x86_64-apple-darwin" ;;
                *) err "unsupported darwin arch: $uname_m" ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*)
            echo "x86_64-pc-windows-msvc"
            ;;
        *)
            err "unsupported OS: $uname_s. For Windows native, use install.ps1 (TBD) or download the .zip from releases."
            ;;
    esac
}

main() {
    local target archive url
    target="$(detect_target)"

    case "$target" in
        *-windows-msvc) archive="${BIN_NAME}-${target}.zip" ;;
        *)              archive="${BIN_NAME}-${target}.tar.gz" ;;
    esac

    url="https://github.com/${REPO}/releases/latest/download/${archive}"
    say "target:   $target"
    say "archive:  $archive"
    say "download: $url"

    # tmpdir is at file scope on purpose: the EXIT trap fires after
    # main() returns, at which point any `local` would be out of
    # scope and `set -u` would fire on the empty expansion.
    tmpdir="$(mktemp -d)"
    trap '[ -n "${tmpdir:-}" ] && rm -rf "$tmpdir"' EXIT

    curl --proto '=https' --tlsv1.2 -fL "$url" -o "${tmpdir}/${archive}" \
        || err "download failed. Does $url exist? Has a release been published?"

    case "$archive" in
        *.tar.gz) tar -xzf "${tmpdir}/${archive}" -C "$tmpdir" ;;
        *.zip)    (cd "$tmpdir" && unzip -q "$archive") ;;
    esac

    local extracted
    if [ -f "${tmpdir}/${BIN_NAME}.exe" ]; then
        extracted="${tmpdir}/${BIN_NAME}.exe"
    elif [ -f "${tmpdir}/${BIN_NAME}" ]; then
        extracted="${tmpdir}/${BIN_NAME}"
    else
        err "didn't find ${BIN_NAME}{,.exe} inside ${archive}"
    fi

    mkdir -p "$INSTALL_DIR"
    install -m 0755 "$extracted" "${INSTALL_DIR}/$(basename "$extracted")"

    say ""
    say "installed: ${INSTALL_DIR}/$(basename "$extracted")"

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            say ""
            say "warning: ${INSTALL_DIR} is not on your PATH."
            say "         add this to your shell profile:"
            say ""
            say "             export PATH=\"${INSTALL_DIR}:\$PATH\""
            ;;
    esac

    say ""
    say "try it:   giga --help"
}

main "$@"
