#!/usr/bin/env bash
# Build this checkout and install it as supergrok in ~/.local/bin.
# Local build for peer messaging and Cursor agent.
#
#   ./scripts/install-supergrok.sh           # build + install current tree
#   ./scripts/install-supergrok.sh --pull    # git pull --ff-only, then build + install
#
# Env:
#   PREFIX   install directory (default: ~/.local/bin)
#   NAME     installed binary name (default: supergrok)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local/bin}"
NAME="${NAME:-supergrok}"
PULL=0

usage() {
  cat <<EOF
Usage: $(basename "$0") [--pull] [--prefix DIR] [--name NAME]

Build xai-grok-pager-bin --release and install it as ${NAME}.

  --pull          git pull --ff-only before building
  --prefix DIR    install directory (default: ${PREFIX})
  --name NAME     installed binary name (default: ${NAME})
  -h, --help      show this help

Env overrides: PREFIX, NAME
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pull) PULL=1; shift ;;
    --prefix)
      PREFIX="${2:?--prefix requires a directory}"
      shift 2
      ;;
    --name)
      NAME="${2:?--name requires a name}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

cd "$REPO_ROOT"

if [[ "$PULL" -eq 1 ]]; then
  echo ">> git pull --ff-only"
  git pull --ff-only
fi

echo ">> cargo build -p xai-grok-pager-bin --release"
cargo build -p xai-grok-pager-bin --release

src="$REPO_ROOT/target/release/xai-grok-pager"
if [[ ! -x "$src" ]]; then
  echo "build succeeded but $src is missing or not executable" >&2
  exit 1
fi

mkdir -p "$PREFIX"
dest="$PREFIX/$NAME"
echo ">> install $src -> $dest"
install -m 755 "$src" "$dest"

echo
echo "Installed $dest"
"$dest" --version
