#!/usr/bin/env bash
# Second eval set: CLI options (-r -s -n -e --arg --argjson --tab) and input/inputs on
# several documents. Same layout as evals/, plus an `args` file (one argv element per
# line) and input.json that may hold several documents.
# Usage: bench/gen-evals2.sh [out_dir]   (default: store/evals2)
set -euo pipefail
OUT="${1:-store/evals2}"
command -v jq >/dev/null || { echo "jq not found" >&2; exit 1; }
rm -rf "$OUT"; mkdir -p "$OUT/cases" "$OUT/inputs"
cat > "$OUT/inputs/people.json" <<'J'
[{"name":"Ana","age":34,"city":"Lyon"},{"name":"Bob","age":27,"city":"Paris"},{"name":"Chloé","age":41,"city":"Lyon"}]
J
cat > "$OUT/inputs/docs.json" <<'J'
{"id":1,"v":"a"}
{"id":2,"v":"b"}
{"id":3,"v":"c"}
J
cat > "$OUT/inputs/scalars.json" <<'J'
1 2 3 "four" null true
J
cat > "$OUT/inputs/nested.json" <<'J'
{"a":{"b":[1,2,{"c":"deep"}]},"s":"line1\nline2","n":-3.75,"t":"x\ty"}
J
# tier|input|args (tab-separated argv)
CASES="$OUT/cases.txt"
cat > "$CASES" <<'EOF2'
1|people.json|-r	.[0].name
1|people.json|-r	.[].name
1|nested.json|-r	.s
1|nested.json|-r	.t
1|nested.json|-r	.a.b[2].c
1|nested.json|-r	.n
1|nested.json|-r	.a
1|people.json|-r	.[] | "\(.name) lives in \(.city)"
1|people.json|-r	map(.name) | join(",")
1|people.json|-r	.[] | [.name, .age] | @tsv
1|people.json|-r	.[] | [.name, .age] | @csv
1|people.json|--raw-output	.[1].city
2|docs.json|-s	.
2|docs.json|-s	length
2|docs.json|-s	map(.id) | add
2|docs.json|-s	.[1].v
2|docs.json|-s	map(.v) | join("")
2|scalars.json|-s	.
2|scalars.json|-s	length
2|scalars.json|-s	map(type)
2|scalars.json|-s	.[3]
2|docs.json|--slurp	map(.id)
2|docs.json|-s	-c	.
2|docs.json|-s	-r	.[0].v
3|people.json|-n	1 + 1
3|people.json|-n	[range(3)]
3|people.json|-n	"hello"
3|people.json|-n	-r	"hello"
3|people.json|-n	null
3|people.json|-n	{a: 1} | .a
3|people.json|--null-input	[1,2] | add
3|docs.json|-n	input
3|docs.json|-n	input | .id
3|docs.json|-n	[inputs]
3|docs.json|-n	[inputs | .id]
3|docs.json|-n	[inputs] | length
3|docs.json|-n	reduce inputs as $d (0; . + $d.id)
3|docs.json|-n	input, input
3|docs.json|-n	[input, input] | map(.v)
3|docs.json|-n	first(inputs)
3|scalars.json|-n	[inputs]
3|scalars.json|-n	[inputs | numbers] | add
3|docs.json|.id, (input | .id)
3|docs.json|[., input]
3|docs.json|[., inputs] | length
3|docs.json|-c	[., input] | map(.v)
4|people.json|-e	.[0].name
4|people.json|-e	.[0].missing
4|people.json|-e	.[0].age > 30
4|people.json|-e	.[1].age > 30
4|people.json|-e	null
4|people.json|-e	false
4|people.json|-e	true
4|people.json|-e	0
4|people.json|-e	""
4|people.json|-e	empty
4|people.json|-e	-r	.[0].city
4|people.json|--exit-status	.[0].missing
4|docs.json|-e	.id == 2
4|docs.json|-e	-s	length > 2
4|docs.json|-e	-n	input | .id
5|people.json|--arg	who	Ana	.[] | select(.name == $who) | .age
5|people.json|--arg	city	Lyon	[.[] | select(.city == $city) | .name]
5|people.json|--arg	x	42	$x
5|people.json|--arg	x	42	$x | type
5|people.json|--arg	x	42	$x | tonumber + 1
5|people.json|--arg	a	1	--arg	b	2	$a + $b
5|people.json|--arg	a	1	--arg	b	2	[$a, $b]
5|people.json|--argjson	n	42	$n + 1
5|people.json|--argjson	n	42	$n | type
5|people.json|--argjson	o	{"k":[1,2]}	$o.k[1]
5|people.json|--argjson	l	[1,2,3]	$l | add
5|people.json|--argjson	min	30	[.[] | select(.age > $min) | .name]
5|people.json|--argjson	b	true	if $b then "yes" else "no" end
5|people.json|--argjson	z	null	$z
5|people.json|--arg	s	hello	--argjson	n	2	[$s, $n]
5|people.json|--arg	s	hello	$s | ascii_upcase
5|people.json|--arg	s	a,b,c	$s | split(",")
5|people.json|--arg	k	name	.[0][$k]
5|people.json|--arg	k	name	map(.[$k])
5|people.json|--argjson	i	1	.[$i].name
5|people.json|--arg	who	Ana	-r	.[] | select(.name == $who) | .city
5|people.json|-n	--arg	x	7	$x
5|people.json|-n	--argjson	x	7	$x * 6
5|people.json|-n	--argjson	x	[1]	$x[0]
5|people.json|--arg	x	y	$__loc__ | type
5|people.json|$ENV | type
5|people.json|--arg	x	1	$ARGS.named.x
5|people.json|--argjson	x	1	$ARGS.named.x
5|people.json|--arg	x	1	--arg	y	2	$ARGS.named | keys
5|people.json|--arg	x	1	$ARGS.positional
6|nested.json|--tab	.a
6|nested.json|--tab	.
6|people.json|--tab	.[0]
6|people.json|--tab	.[0] | {name}
6|people.json|--tab	[1,[2]]
6|nested.json|--tab	.a.b
6|nested.json|--tab	{}
6|nested.json|--tab	[]
6|nested.json|--tab	-r	.s
6|nested.json|--indent	4	.a
6|nested.json|--indent	0	.a
6|nested.json|--indent	1	.a.b
6|nested.json|.a
6|nested.json|.
6|people.json|.[0]
6|nested.json|-c	.a
6|nested.json|--compact-output	.
6|nested.json|-S	.
6|nested.json|--sort-keys	.a
6|people.json|-S	.[0]
6|people.json|-S	-c	.[0]
6|nested.json|-r	--tab	.a
7|docs.json|.nope.deeper
7|docs.json|-e	.nope
7|docs.json|-n	input | .id | error
7|docs.json|-n	[inputs] | .[5].id
7|docs.json|-n	input, error("x")
7|people.json|--argjson	bad	{notjson	$bad
7|people.json|--arg	$x
7|people.json|--argjson	x	$x
7|people.json|-n	$undefined
7|people.json|--tab	.[
7|people.json|-e	.[0].name | select(. == "Nobody")
7|docs.json|-s	.[0].id | error
7|scalars.json|-s	.[3] + 1
7|scalars.json|.[]
7|scalars.json|-e	.
7|docs.json|--unknown-flag	.
EOF2
n=0; ok=0; err=0
while IFS='|' read -r tier input args; do
  [ -z "$tier" ] && continue
  n=$((n+1))
  d="$OUT/cases/$(printf '%03d' "$n")"
  mkdir -p "$d"
  printf '%s' "$args" | tr '\t' '\n' > "$d/args"
  cp "$OUT/inputs/$input" "$d/input.json"
  echo "$tier" > "$d/tier"
  set +e
  argv=()
  IFS=$'\t' read -r -a argv <<< "$args"
  jq ${argv[@]+"${argv[@]}"} < "$d/input.json" > "$d/expected" 2> "$d/stderr"
  code=$?
  set -e
  echo "$code" > "$d/exit"
  if [ "$code" -eq 0 ]; then ok=$((ok+1)); else err=$((err+1)); fi
done < "$CASES"
cat > "$OUT/run.sh" <<'EOF2'
#!/usr/bin/env bash
# Usage: evals2/run.sh path/to/rjq [tier]
# Each case has an `args` file (one argv element per line). Exact stdout match (jq's
# own formatting), same exit code.
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
  argv=()
  while IFS= read -r line || [ -n "$line" ]; do argv+=("$line"); done < "$d/args"
  want=$(cat "$d/expected")
  wexit=$(cat "$d/exit")
  got=$($TIMEOUT "$BIN" ${argv[@]+"${argv[@]}"} < "$d/input.json" 2>/dev/null)
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
EOF2
chmod +x "$OUT/run.sh"
echo "generated $n cases in $OUT/ ($ok exit 0, $err non-zero)"
printf '#!/bin/sh\nexec jq "$@"\n' > /tmp/jq2 && chmod +x /tmp/jq2
"$OUT/run.sh" /tmp/jq2 | head -3
