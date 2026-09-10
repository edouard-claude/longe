#!/usr/bin/env bash
# Fitness of the store for the rjq benchmark: fraction of eval cases passed by the
# rjq binary found in $LONGE_FITNESS_WORKSPACE (or ./fitness-workspace), as a number.
# Last stdout line = score. Higher is better. Missing binary = 0.
set -u
DIR="$(cd "$(dirname "$0")" && pwd)"
WS="${LONGE_FITNESS_WORKSPACE:-$DIR/../fitness-workspace}"
BIN=""
for cand in "$WS/target/release/rjq" "$WS/target/debug/rjq" "$WS/rjq"; do
  [ -x "$cand" ] && BIN="$cand" && break
done
if [ -z "$BIN" ]; then echo "no rjq binary under $WS"; echo 0; exit 0; fi
line=$("$DIR/run.sh" "$BIN" | head -1)
pass=${line%%/*}
echo "$line"
echo "$pass"
