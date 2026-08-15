#!/usr/bin/env bash
# Download a pinned cursor-sdk-bridge standalone archive, verify SHA256, and
# extract bin/cursor-sdk-bridge (or .exe) to an output directory.
#
# Usage:
#   fetch-cursor-sdk-bridge.sh [--os linux|darwin|win32] [--arch x64|arm64] [--out DIR]
#
# Defaults: host OS/arch, output ~/.grok/bin
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PIN_DIR="$(cd "$SCRIPT_DIR/../../../../third_party/cursor-sdk-bridge" && pwd)"
VERSION="$(tr -d '[:space:]' < "$PIN_DIR/VERSION")"
SUMS="$PIN_DIR/SHA256SUMS"

os=""
arch=""
out_dir="${GROK_HOME:-$HOME/.grok}/bin"

while [ $# -gt 0 ]; do
    case "$1" in
        --os) os="$2"; shift 2 ;;
        --arch) arch="$2"; shift 2 ;;
        --out) out_dir="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [ -z "$os" ]; then
    case "$(uname -s)" in
        Linux) os=linux ;;
        Darwin) os=darwin ;;
        MINGW*|MSYS*|CYGWIN*) os=win32 ;;
        *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
    esac
fi

if [ -z "$arch" ]; then
    case "$(uname -m)" in
        x86_64|amd64|AMD64) arch=x64 ;;
        arm64|aarch64|ARM64) arch=arm64 ;;
        *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;;
    esac
fi

if [ "$os" = win32 ] && [ "$arch" = arm64 ]; then
    echo "cursor-sdk-bridge has no win32-arm64 build (x64 only)." >&2
    exit 1
fi

archive="cursor-sdk-bridge-standalone-${os}-${arch}.tar.gz"
expected="$(awk -v f="$archive" '$2==f {print $1; exit}' "$SUMS")"
if [ -z "$expected" ]; then
    echo "no checksum for $archive in $SUMS" >&2
    exit 1
fi

url="https://github.com/cursor/sdk-bridge/releases/download/${VERSION}/${archive}"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "Fetching $url" >&2
curl -fsSL -o "$tmpdir/$archive" "$url"

got="$(
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$tmpdir/$archive" | awk '{print $1}'
    else
        shasum -a 256 "$tmpdir/$archive" | awk '{print $1}'
    fi
)"
if [ "$got" != "$expected" ]; then
    echo "SHA256 mismatch for $archive" >&2
    echo "  expected $expected" >&2
    echo "  got      $got" >&2
    exit 1
fi

tar -xzf "$tmpdir/$archive" -C "$tmpdir"
src="$tmpdir/bin/cursor-sdk-bridge"
if [ "$os" = win32 ]; then
    src="$tmpdir/bin/cursor-sdk-bridge.exe"
fi
if [ ! -f "$src" ]; then
    echo "archive missing $src" >&2
    exit 1
fi

mkdir -p "$out_dir"
dest="$out_dir/$(basename "$src")"
cp -f "$src" "$dest"
chmod +x "$dest" 2>/dev/null || true
echo "Installed $dest (sdk-bridge $VERSION)" >&2
