#!/usr/bin/env bash
# Usage: evals2/run.sh path/to/rjq [tier]
# Each case has an `args` file (one argv element per line). Exact stdout match (jq's
# own formatting), same exit code.
set -u
BIN="${1:?path to rjq binary}"
TIER="${2:-}"
DIR="$(cd "$(dirname "$0")" && pwd)"
total=0; pass=0; fails=()
for d in "$DIR"/cases/*/; do
  t=$(cat "$d/tier")
  [ -n "$TIER" ] && [ "$t" != "$TIER" ] && continue
  total=$((total+1))
  argv=()
  while IFS= read -r line || [ -n "$line" ]; do argv+=("$line"); done < "$d/args"
  want=$(cat "$d/expected")
  wexit=$(cat "$d/exit")
  got=$(timeout 5 "$BIN" ${argv[@]+"${argv[@]}"} < "$d/input.json" 2>/dev/null)
  gexit=$?
  if [ "$gexit" -eq "$wexit" ] && { [ "$wexit" -ne 0 ] || [ "$got" == "$want" ]; }; then
    pass=$((pass+1))
  else
    fails+=("$(basename "$d") tier=$t args=$(printf '%q ' ${argv[@]+"${argv[@]}"}) expected_exit=$wexit got_exit=$gexit expected=$(printf '%q' "$want" | head -c 200) got=$(printf '%q' "$got" | head -c 200)")
  fi
done
echo "$pass/$total OK"
if [ "${#fails[@]}" -gt 0 ]; then printf '%s\n' "${fails[@]}"; fi
[ "$pass" -eq "$total" ]
