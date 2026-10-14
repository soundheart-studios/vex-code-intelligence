#!/bin/sh
# Install (or remove) the pre-commit structural-guard hook.
#
# WHY THIS ONE BLOCKS. vex's product hooks (`vex hook`) are fail-open by
# design: they keep an index fresh, and failing them must never cost anyone
# their work. This hook does the opposite job. It refuses a commit that
# breaks a structural invariant about the SHAPE of the codebase, which no
# passing behaviour test catches. The guards only read source, so they cost a
# fraction of a second; a gate slow enough to invite --no-verify is worse than
# none. Guards (`cargo xtask guards --list`):
#   pure-crates thin-adapters one-rrf no-interpreters no-http golden-untracked
#
# .git/hooks is not version controlled, so this script is the only durable
# copy. It is idempotent, never touches anything outside its markers, and
# --uninstall removes exactly what it added. The hooks directory comes from
# `git rev-parse --git-path hooks`, which honours core.hooksPath and worktrees.
set -eu

MARKER_START="# >>> vex guards >>>"
MARKER_END="# <<< vex guards <<<"

UNINSTALL=0
REPO=""
for arg in "$@"; do
  case "$arg" in
    --uninstall) UNINSTALL=1 ;;
    *) REPO="$arg" ;;
  esac
done
[ -n "$REPO" ] || REPO="$(git rev-parse --show-toplevel)"

HOOK_DIR="$(cd "$REPO" && git rev-parse --path-format=absolute --git-path hooks)"
HOOK="$HOOK_DIR/pre-commit"
mkdir -p "$HOOK_DIR"

if [ -f "$HOOK" ]; then
  awk -v s="$MARKER_START" -v e="$MARKER_END" '
    $0 == s {skip = 1} skip == 0 {print} $0 == e {skip = 0}
  ' "$HOOK" > "$HOOK.tmp"
  cat "$HOOK.tmp" > "$HOOK"
  rm -f "$HOOK.tmp"
else
  printf '#!/bin/sh\n' > "$HOOK"
fi

if [ "$UNINSTALL" -eq 1 ]; then
  chmod +x "$HOOK"
  echo "removed the vex guards block from $HOOK"
  exit 0
fi

cat >> "$HOOK" <<'BLOCK'
# >>> vex guards >>>
# Structural invariants (pure-crates thin-adapters one-rrf no-interpreters
# no-http golden-untracked). Fast on purpose; see scripts/install-git-hooks.sh.
cargo xtask guards || {
  echo ""
  echo "A structural guard failed. This is not a flaky test: something that"
  echo "should be true of the codebase's shape has stopped being true."
  echo "Fix it, or commit with --no-verify if you genuinely mean to."
  exit 1
}
# <<< vex guards <<<
BLOCK

chmod +x "$HOOK"
echo "installed the vex guards pre-commit hook in $HOOK"
