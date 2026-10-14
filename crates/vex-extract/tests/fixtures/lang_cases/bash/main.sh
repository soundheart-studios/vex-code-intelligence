#!/usr/bin/env bash
set -euo pipefail
source ./lib/util.sh
. lib/util.sh
. "./lib/missing.sh"
source ../../escape.sh
source "$HOME/.bashrc"
source /etc/profile

greet() {
  local who="$1"
  echo "hi $who"
  helper "$who"
}

function helper {
  printf '%s\n' "$1"
}

out=$(greet world)
greet "again"
helper x | tr a b
