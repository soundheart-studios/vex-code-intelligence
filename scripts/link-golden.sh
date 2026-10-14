#!/bin/sh
# Link golden/data to the Cortex golden export pinned by golden/REVISION.
# The data is never committed; only the pin is. `cargo xtask parity` also
# finds the export without this link, so the link is a convenience.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REV="$(tr -d '[:space:]' < "$ROOT/golden/REVISION")"
CORPORA="${VEX_CORTEX_CORPORA:-$HOME/Documents/cortex-corpora}"
TARGET="$CORPORA/golden/$REV"
if [ ! -f "$TARGET/manifest.json" ]; then
  echo "no golden export at $TARGET (run scripts/export_golden.py in Cortex at $REV)" >&2
  exit 1
fi
if [ "${1:-}" = "--check" ]; then
  [ "$(readlink "$ROOT/golden/data" 2>/dev/null)" = "$TARGET" ] && echo "golden/data -> $TARGET" && exit 0
  echo "golden/data is not linked to $TARGET" >&2
  exit 1
fi
ln -sfn "$TARGET" "$ROOT/golden/data"
echo "golden/data -> $TARGET"
