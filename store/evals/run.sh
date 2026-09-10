#!/usr/bin/env bash
# Usage: evals/run.sh path/to/rjq [tier]
# Prints "N/TOTAL OK" then one line per failure: case, tier, filter, expected, got, exit.
set -u
BIN="${1:?path to rjq binary}"
TIER="${2:-}"
DIR="$(cd "$(dirname "$0")" && pwd)"
# `timeout` is GNU coreutils: present on Linux, often absent on macOS (where it is
# `gtimeout` when coreutils is installed). Fall back to running without a timeout.
if command -v timeout >/dev/null 2>&1; then TIMEOUT="timeout 5"
elif command -v gtimeout >/dev/null 2>&1; then TIMEOUT="gtimeout 5"
else TIMEOUT=""; fi
total=0; pass=0; fails=()
for d in "$DIR"/cases/*/; do
  t=$(cat "$d/tier")
  [ -n "$TIER" ] && [ "$t" != "$TIER" ] && continue
  total=$((total+1))
  f=$(cat "$d/filter")
  want=$(cat "$d/expected")
  wexit=$(cat "$d/exit")
  got=$($TIMEOUT "$BIN" "$f" < "$d/input.json" 2>/dev/null)
  gexit=$?
  if [ "$gexit" -eq "$wexit" ] && { [ "$wexit" -ne 0 ] || [ "$got" == "$want" ]; }; then
    pass=$((pass+1))
  else
    fails+=("$(basename "$d") tier=$t filter=$(printf '%q' "$f") expected_exit=$wexit got_exit=$gexit expected=$(printf '%q' "$want" | head -c 200) got=$(printf '%q' "$got" | head -c 200)")
  fi
done
echo "$pass/$total OK"
if [ "${#fails[@]}" -gt 0 ]; then printf '%s\n' "${fails[@]}"; fi
[ "$pass" -eq "$total" ]
