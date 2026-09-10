#!/usr/bin/env bash
# A/B protocol (PRD section 8).
#   A: baseline JSON-tool loop.   B: Longe, everything on.   B2: second run, CLI options task.
# Usage: bench/ab.sh <provider/model> [runs_dir]
# Env:   LONGE_STORE (default ./store), WS_ROOT (default ./bench/ws), MIN_SECONDS (3600), MAX_TURNS (400), MAX_TOKENS (6000000)
set -euo pipefail
MODEL="${1:?provider/model}"
RUNS="${2:-runs}"
STORE="${LONGE_STORE:-$PWD/store}"
WS_ROOT="${WS_ROOT:-$PWD/bench/ws}"
MIN_SECONDS="${MIN_SECONDS:-3600}"
MAX_TURNS="${MAX_TURNS:-400}"
MAX_TOKENS="${MAX_TOKENS:-6000000}"
LONGE="${LONGE:-$PWD/target/release/longe}"
EVALS="$STORE/evals"
mkdir -p "$RUNS" "$WS_ROOT"

TASK1='Implémente `rjq`, un clone de `jq` en Rust, stdlib only. Binaire `rjq <filtre> < entrée.json`, sortie JSON compacte sur stdout, code de retour 0/1/5 comme jq. Découpe en sous-tâches si utile. Le projet est un crate cargo dans le répertoire courant (crée Cargo.toml si absent, binaire nommé rjq). Le verifier lance cargo test, clippy et evals/run.sh sur 693 cas ; done() n est accepté qu à 693/693.'
TASK2='Ajoute à rjq les options CLI `-r`, `-s`, `-n`, `-e`, `--arg name value`, `--argjson name json`, `--tab`, et les fonctions `input`/`inputs` sur plusieurs documents JSON en entrée (le second jeu evals2/ les couvre).'

score() { # <ws> <evals_dir> -> "N/TOTAL"
  local bin=""
  for c in "$1/target/release/rjq" "$1/target/debug/rjq"; do [ -x "$c" ] && bin="$c" && break; done
  [ -z "$bin" ] && { echo "0/?"; return; }
  "$2/run.sh" "$bin" | head -1 | awk '{print $1}'
}

metric_line() { # label ws evals_dir json_file
  local label=$1 ws=$2 evals=$3 json=$4
  local green tokens turns secs refused spawned
  green=$(score "$ws" "$evals")
  tokens=$(jq -r '.tokens // .counters.tokens // 0' "$json")
  turns=$(jq -r '.turns // .counters.turns // 0' "$json")
  secs=$(jq -r '.seconds // .counters.elapsed_before // 0' "$json")
  refused=$(jq -r '.counters.done_refused // 0' "$json")
  spawned=$(jq -r '.spawned // 0' "$json")
  printf '| %-8s | %10s | %10s | %6s | %8s | %8s | %8s |\n' "$label" "$green" "$tokens" "$turns" "$secs" "$refused" "$spawned"
}

run_A() {
  local ws="$WS_ROOT/A"; rm -rf "$ws"; mkdir -p "$ws"
  (cd "$ws" && cargo init -q --name rjq . 2>/dev/null || true)
  LONGE_STORE="$STORE" "$LONGE" baseline "$TASK1" --workspace "$ws" --model "$MODEL" --max-turns "$MAX_TURNS" --label A > "$RUNS/A.log" 2>&1
  cp runs/A.json "$RUNS/A.json" 2>/dev/null || true
  metric_line A "$ws" "$EVALS" "$RUNS/A.json" >> "$RUNS/table.md"
}

run_B() { # label task evals_dir
  local label=$1 task=$2 evals=$3
  local ws="$WS_ROOT/B"
  if [ "$label" = "B" ]; then rm -rf "$ws"; mkdir -p "$ws"; (cd "$ws" && cargo init -q --name rjq . 2>/dev/null || true); fi
  local id
  id=$(LONGE_STORE="$STORE" "$LONGE" run "$task" --workspace "$ws" --name "$label" --model "$MODEL" \
        --min-seconds "$MIN_SECONDS" --max-turns "$MAX_TURNS" --max-tokens "$MAX_TOKENS" --detach | awk '/session/{print $2}')
  echo "$label session $id" | tee -a "$RUNS/ids.txt"
  LONGE_STORE="$STORE" "$LONGE" wait "$id" > "$RUNS/$label.json"
  LONGE_STORE="$STORE" "$LONGE" show "$id" --tail 100000 > "$RUNS/$label.detail.json"
  metric_line "$label" "$ws" "$evals" "$RUNS/$label.json" >> "$RUNS/table.md"
}

{
  echo "| run      | green      | tokens     | turns  | seconds  | refused  | spawned  |"
  echo "|----------|------------|------------|--------|----------|----------|----------|"
} > "$RUNS/table.md"

LONGE_STORE="$STORE" "$LONGE" daemon >/dev/null
run_A
run_B B "$TASK1" "$EVALS"
# Second run needs the second eval set: bench/gen-evals2.sh store/evals2
if [ -d "$STORE/evals2" ]; then
  run_B B2 "$TASK2" "$STORE/evals2"
fi
echo "reflect diffs proposed/accepted:"; grep -c '"branch"' "$STORE/reflect.log" 2>/dev/null || echo 0
grep -c '"accepted":true' "$STORE/reflect.log" 2>/dev/null || echo 0
cat "$RUNS/table.md"
