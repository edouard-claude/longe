#!/usr/bin/env bash
# Generate evals/ for the rjq benchmark using the real jq.
# Usage: ./gen-evals.sh [out_dir]   (default: ./evals)
# Requires: jq >= 1.6
set -euo pipefail

OUT="${1:-evals}"
command -v jq >/dev/null || { echo "jq not found" >&2; exit 1; }
rm -rf "$OUT"
mkdir -p "$OUT/cases" "$OUT/inputs"

# ---------- inputs ----------
cat > "$OUT/inputs/people.json" <<'EOF'
[{"name":"Ana","age":34,"city":"Lyon","tags":["dev","go"],"score":8.5},
 {"name":"Bob","age":27,"city":"Paris","tags":["ops"],"score":6},
 {"name":"Chloé","age":41,"city":"Lyon","tags":["dev","rust","lead"],"score":9.25},
 {"name":"Dan","age":19,"city":"Nice","tags":[],"score":null},
 {"name":"Eve","age":34,"city":"Paris","tags":["qa","dev"],"score":7}]
EOF
cat > "$OUT/inputs/nested.json" <<'EOF'
{"id":42,"meta":{"created":"2026-09-09","owner":{"name":"root","uid":0},"flags":[true,false,null]},
 "items":[{"sku":"a1","qty":3,"price":2.5},{"sku":"b2","qty":0,"price":10},{"sku":"c3","qty":7,"price":0.99}],
 "empty":{},"list":[],"text":"hello, world","n":-3.75}
EOF
cat > "$OUT/inputs/numbers.json" <<'EOF'
[3,1,4,1,5,9,2,6,5,3,5]
EOF
cat > "$OUT/inputs/strings.json" <<'EOF'
["alpha","beta","gamma","delta","alpha","Épsilon"]
EOF
cat > "$OUT/inputs/scalar.json" <<'EOF'
"just a string"
EOF
cat > "$OUT/inputs/kv.json" <<'EOF'
{"b":2,"a":1,"c":{"z":26,"y":25},"d":[1,2,3]}
EOF
cat > "$OUT/inputs/invalid.json" <<'EOF'
{"a": 1, "b": [1, 2,]
EOF

# ---------- filters ----------
# format: tier|input|filter
# cases list written to a file: bash 3.2 (macOS) cannot parse a heredoc with unbalanced quotes inside $(...)
cat > "$OUT/cases.txt" <<'EOF'
1|people.json|.
1|people.json|.[0]
1|people.json|.[0].name
1|people.json|.[-1]
1|people.json|.[1:3]
1|people.json|.[:2]
1|people.json|.[3:]
1|people.json|.[]
1|people.json|.[].name
1|people.json|.[].tags[0]
1|people.json|.[].tags[0]?
1|people.json|.[10]
1|nested.json|.id
1|nested.json|.meta.owner.name
1|nested.json|.meta.flags[1]
1|nested.json|.items[1].sku
1|nested.json|.missing
1|nested.json|.missing.deeper
1|nested.json|.meta["created"]
1|nested.json|."text"
1|nested.json|.items[]
1|nested.json|.items[].qty
1|nested.json|.text[0:5]
1|nested.json|.empty
1|nested.json|.list
1|numbers.json|.[2:5]
1|numbers.json|.[0]
1|numbers.json|.[-2:]
1|scalar.json|.
1|scalar.json|.[0:4]
1|kv.json|.c.z
1|kv.json|.d[1]
1|kv.json|.c["y"]
1|people.json|.[0]?
1|scalar.json|.a?
1|scalar.json|.[]?
2|people.json|{name: .[0].name}
2|people.json|{name}
2|people.json|.[] | {name, city}
2|people.json|.[] | {n: .name, a: .age}
2|people.json|[.[] | .name]
2|people.json|[.[] | .age]
2|people.json|[.[].tags]
2|people.json|[.[].tags[]]
2|nested.json|[..]
2|nested.json|[.. | numbers]
2|nested.json|[.. | strings]
2|nested.json|{id, owner: .meta.owner.name}
2|nested.json|[.items[] | {sku, total: (.qty * .price)}]
2|kv.json|{a, b}
2|kv.json|[.d[] | . * 2]
2|kv.json|{(.c | keys[0]): 1}
2|people.json|[.[] | .name, .age]
2|people.json|.[0] | [.name, .age, .city]
2|nested.json|[.meta.flags[] | not]
2|numbers.json|[.[] | . + 1]
2|strings.json|[.[] | ascii_upcase]
2|people.json|{names: [.[].name], count: length}
2|nested.json|.meta | {created, owner}
2|nested.json|[.items[] | select(.qty > 0) | .sku]
2|people.json|[.[] | {(.name): .age}]
2|people.json|[.[] | {(.name): .age}] | add
3|people.json|.[] | select(.age > 30) | .name
3|people.json|[.[] | select(.city == "Lyon")]
3|people.json|[.[] | select(.city == "Lyon") | .name]
3|people.json|map(.name)
3|people.json|map(.age)
3|people.json|map(select(.age >= 34))
3|people.json|map(select(.age >= 34) | .name)
3|people.json|map(.tags | length)
3|people.json|map(has("score"))
3|people.json|map(.score) | map(type)
3|people.json|.[0] | keys
3|people.json|.[0] | keys_unsorted
3|people.json|length
3|people.json|.[0] | length
3|people.json|.[0].name | length
3|people.json|.[0].tags | length
3|people.json|map(.name) | join(", ")
3|people.json|.[0] | has("name")
3|people.json|.[0] | has("zip")
3|people.json|.[0] | type
3|people.json|.[0].age | type
3|people.json|.[3].score | type
3|nested.json|keys
3|nested.json|.meta | keys
3|nested.json|.items | map(.price)
3|nested.json|.items | map(select(.price < 5)) | length
3|nested.json|.items | map(.sku) | join("-")
3|nested.json|[.items[] | .qty] | add
3|nested.json|.text | split(", ")
3|nested.json|.text | split(", ") | .[1]
3|nested.json|.meta.flags | map(type)
3|nested.json|.text | type
3|nested.json|.n | type
3|nested.json|.empty | type
3|nested.json|.list | type
3|nested.json|.meta.flags[2] | type
3|numbers.json|map(select(. > 3))
3|numbers.json|map(select(. % 2 == 0))
3|numbers.json|length
3|strings.json|map(select(startswith("a")))
3|strings.json|map(select(test("^[a-d]")))
3|strings.json|map(length)
3|kv.json|keys
3|kv.json|to_entries | map(.key)
3|kv.json|.c | to_entries
3|kv.json|.d | map(. + 10)
3|people.json|.[] | .name, .city
3|people.json|.[0] | .name, .age
3|people.json|.[0].tags | .[0], .[1]
3|nested.json|.id, .n
3|people.json|[.[] | .age] | add / length
3|people.json|first(.[] | select(.age < 30)) | .name
3|people.json|.[] | select(.tags | index("dev")) | .name
3|nested.json|.items | any(.qty == 0)
3|nested.json|.items | all(.price > 0)
3|people.json|map(.score // 0)
3|people.json|[.[] | .score // "n/a"]
3|people.json|.[0] | del(.tags)
3|people.json|.[0] | del(.tags, .score)
3|nested.json|.meta | del(.flags)
3|people.json|[.[] | .name] | reverse
3|numbers.json|reverse
3|people.json|.[0] | to_entries | map(.key) | sort
4|numbers.json|add
4|numbers.json|add / length
4|numbers.json|min
4|numbers.json|max
4|numbers.json|sort
4|numbers.json|unique
4|numbers.json|map(. * 2)
4|numbers.json|map(. - 1)
4|numbers.json|map(. / 2)
4|numbers.json|map(. % 3)
4|numbers.json|.[0] + .[1]
4|numbers.json|.[5] * .[6]
4|numbers.json|.[3] == .[1]
4|numbers.json|.[0] != .[1]
4|numbers.json|.[5] > .[0]
4|numbers.json|.[5] < .[0]
4|numbers.json|.[0] >= 3
4|numbers.json|.[0] <= 3
4|numbers.json|(.[0] > 1) and (.[1] > 1)
4|numbers.json|(.[0] > 1) or (.[1] > 1)
4|numbers.json|(.[0] > 1) | not
4|numbers.json|map(. > 4) | any
4|numbers.json|map(. > 4) | all
4|numbers.json|[.[] | select(. > 2 and . < 6)]
4|numbers.json|[.[] | select(. == 1 or . == 9)]
4|nested.json|.n * -1
4|nested.json|.n + .id
4|nested.json|.id / 4
4|nested.json|.id % 5
4|nested.json|.missing // "default"
4|nested.json|.meta.flags[1] // "fallback"
4|nested.json|.meta.flags[2] // "fallback"
4|nested.json|.meta.flags[0] // "fallback"
4|nested.json|.id // 0
4|nested.json|.text + "!"
4|nested.json|.text + " " + .meta.created
4|strings.json|.[0] + .[1]
4|strings.json|.[0] < .[1]
4|strings.json|map(. == "alpha")
4|kv.json|.a + .b
4|kv.json|.c.z - .c.y
4|kv.json|.d + [4]
4|kv.json|.d - [2]
4|kv.json|.c + {"x": 24}
4|kv.json|. * {"c": {"w": 23}}
4|kv.json|.a == 1 and .b == 2
4|kv.json|.a > .b
4|kv.json|[.d[] | . * .]
4|kv.json|(.d | add) * 2
4|people.json|map(.age) | add
4|people.json|map(.age) | max
4|people.json|map(.age) | min
4|people.json|[.[] | .age * 12]
4|people.json|map(.age > 30)
4|people.json|map(.age > 30 and .city == "Lyon")
4|people.json|map(.score // 0) | add
4|people.json|.[0].age - .[1].age
4|people.json|(.[0].age + .[1].age) / 2
4|nested.json|.items | map(.qty * .price) | add
4|nested.json|.items | map(.qty * .price) | add | floor
4|nested.json|.n | floor
4|nested.json|.n | fabs
4|nested.json|.n | -.
4|numbers.json|map(. * 1.5)
4|numbers.json|10 / 4
4|numbers.json|1e3
4|numbers.json|0.1 + 0.2
5|people.json|sort_by(.age)
5|people.json|sort_by(.age) | map(.name)
5|people.json|sort_by(.name) | map(.name)
5|people.json|sort_by(-.age) | .[0].name
5|people.json|sort_by(.age, .name) | map(.name)
5|people.json|group_by(.city)
5|people.json|group_by(.city) | map(length)
5|people.json|group_by(.city) | map({city: .[0].city, n: length})
5|people.json|group_by(.age) | map(map(.name))
5|people.json|map(.city) | unique
5|people.json|map(.tags[]) | unique
5|people.json|unique_by(.city) | map(.name)
5|people.json|[.[].tags[]] | group_by(.) | map({tag: .[0], n: length})
5|people.json|.[0] | to_entries
5|people.json|.[0] | to_entries | map(select(.value | type == "string")) | from_entries
5|people.json|.[0] | with_entries(.value |= tostring)
5|people.json|.[0] | with_entries(select(.key != "tags"))
5|people.json|map(.age) | add
5|people.json|map(.tags) | add
5|people.json|map(.name) | add
5|people.json|map(.age | tostring)
5|people.json|map(.age | tostring | tonumber)
5|people.json|map(.name | ascii_downcase)
5|people.json|map(.name | ascii_upcase)
5|people.json|map(.name | length)
5|people.json|map(.name | split("")) | .[0]
5|people.json|map(.name) | join("|")
5|people.json|map(.name | test("^[A-C]"))
5|people.json|map(select(.name | test("é")))
5|people.json|map(.name | startswith("A"))
5|people.json|map(.name | endswith("e"))
5|people.json|map(.name | ltrimstr("A"))
5|people.json|map(.name | rtrimstr("a"))
5|people.json|map(.name | explode | length)
5|people.json|map(.name | @text)
5|people.json|.[0].name | @json
5|people.json|.[0] | tojson
5|people.json|.[0] | tojson | fromjson | .name
5|people.json|map(.tags | contains(["dev"]))
5|people.json|map(.tags | inside(["dev","rust","lead","go","ops","qa"]))
5|people.json|map(.tags | index("dev"))
5|people.json|map(.tags | indices("dev"))
5|people.json|map(.tags | first)
5|people.json|map(.tags | last)
5|people.json|map(.tags | first // "none")
5|people.json|[limit(2; .[])] | map(.name)
5|people.json|first(.[]) | .name
5|people.json|last(.[]) | .name
5|people.json|nth(2; .[]) | .name
5|people.json|[range(3)]
5|people.json|[range(1;4)]
5|people.json|[range(0;10;3)]
5|people.json|[range(length)] | map(. * 10)
5|people.json|to_entries | map({i: .key, name: .value.name})
5|people.json|map(.age) | [.[] | . * 2] | sort | reverse
5|people.json|map(.age) | sort | .[length/2|floor]
5|people.json|map(.age) | (add / length) | floor
5|people.json|map(.score) | map(select(. != null)) | add
5|people.json|map(.score) | map(values) | length
5|people.json|map(.score) | map(nulls) | length
5|people.json|map(.tags | join(",") )
5|people.json|[paths] | length
5|people.json|[paths(type == "number")] | length
5|people.json|.[0] | [paths]
5|people.json|.[0] | getpath(["tags", 1])
5|people.json|.[0] | setpath(["age"]; 35) | .age
5|people.json|.[0] | delpaths([["tags"],["score"]])
5|people.json|.[0] | leaf_paths | join(".")
5|people.json|map(.name) | index("Chloé")
5|people.json|map(.name) | sort | .[0]
5|people.json|map(.name | ascii) | .[0]
5|people.json|map(.name | @base64)
5|people.json|map(.name | @base64 | @base64d)
5|people.json|map(.name | @uri)
5|people.json|.[0] | [.name, .age] | @csv
5|people.json|.[0] | [.name, .city] | @tsv
5|people.json|.[0] | [.name, .age] | @sh
5|people.json|.[0] | "\(.name) is \(.age)"
5|people.json|map("\(.name):\(.city)")
5|people.json|.[0] | "\(.name | ascii_upcase)!"
5|people.json|[.[] | select(.age > 30)] | length
5|people.json|any(.[]; .age > 40)
5|people.json|all(.[]; .age > 18)
5|people.json|map(.age) | map(if . > 30 then "senior" else "junior" end)
5|people.json|map(if .score == null then "n/a" elif .score > 8 then "high" else "low" end)
5|people.json|.[0] | if .tags | length > 0 then .tags[0] else "none" end
5|people.json|map(.age) | map(. as $a | $a * 2)
5|people.json|. as $all | $all | length
5|people.json|.[0] as {name: $n, age: $a} | "\($n)/\($a)"
5|people.json|reduce .[] as $p (0; . + $p.age)
5|people.json|reduce .[] as $p ({}; .[$p.city] += 1)
5|people.json|[foreach .[] as $p (0; . + 1)]
5|people.json|[foreach .[] as $p (0; . + $p.age; [$p.name, .])]
5|people.json|def double: . * 2; map(.age | double)
5|people.json|def inc(f): f + 1; map(.age | inc(.))
5|people.json|[.[] | .name] | [.[0], .[-1]]
5|people.json|map(.name) | .[1:] | join("")
5|people.json|map(.age) | max_by(.)
5|people.json|max_by(.age) | .name
5|people.json|min_by(.age) | .name
5|people.json|map(select(.tags | length > 1)) | map(.name)
5|people.json|flatten
5|people.json|map(.tags) | flatten
5|people.json|map(.tags) | flatten(1)
5|nested.json|.meta.flags | map(if . == null then "nil" else tostring end)
5|nested.json|.items | map(.price | floor)
5|nested.json|.items | map(.price | ceil)
5|nested.json|.items | map(.price | round)
5|nested.json|.items | map(.price | sqrt | floor)
5|nested.json|.items | map(.price | pow(.; 2))
5|nested.json|.items | map(.price | log10 | floor)
5|nested.json|[.items[] | .sku | ascii_upcase]
5|nested.json|.items | map({(.sku): .qty}) | add
5|nested.json|.items | map(select(.qty == 0)) | map(.sku)
5|nested.json|.items | sort_by(.price) | map(.sku)
5|nested.json|.items | sort_by(-.qty) | .[0].sku
5|nested.json|.items | to_entries | map({idx: .key, sku: .value.sku})
5|nested.json|.text | ascii_upcase
5|nested.json|.text | split(" ") | map(length)
5|nested.json|.text | sub(", "; "-")
5|nested.json|.text | gsub("l"; "L")
5|nested.json|.text | test("world")
5|nested.json|.text | match("w.r") | .string
5|nested.json|.text | [match("l"; "g")] | length
5|nested.json|.text | capture("(?<first>\\w+), (?<second>\\w+)")
5|nested.json|.text | scan("o")
5|nested.json|.text | splits(", ")
5|nested.json|.text | ascii_downcase | explode | implode
5|nested.json|.text | @html
5|nested.json|.meta.created | split("-") | map(tonumber)
5|nested.json|.meta.created | strptime("%Y-%m-%d") | mktime
5|nested.json|.meta.created | strptime("%Y-%m-%d") | strftime("%d/%m/%Y")
5|nested.json|.meta | walk(if type == "boolean" then (. | not) else . end)
5|nested.json|walk(if type == "number" then . + 1 else . end) | .id
5|nested.json|[.. | select(type == "number")] | add
5|nested.json|[.. | objects | keys[]] | unique
5|nested.json|.items | INDEX(.sku)
5|nested.json|.items | INDEX(.sku) | keys
5|nested.json|.items | map(.sku) | IN("a1", "zz")
5|nested.json|.items[] | select(.sku | IN("a1","c3")) | .qty
5|nested.json|.items | map(select(.sku | IN("a1","c3"))) | length
5|nested.json|.meta | tostream | select(length == 2) | .[0] | join(".")
5|nested.json|[.meta | tostream] | length
5|nested.json|.meta | [leaf_paths | map(tostring) | join(".")]
5|nested.json|.items | map(.qty) | @json
5|nested.json|.items | map([.sku, .qty]) | map(@csv) | join("\n")
5|nested.json|.items | map(.price) | map(. * 100 | round / 100)
5|nested.json|.n | tostring
5|nested.json|.id | tostring | length
5|nested.json|.meta.owner | tojson
5|nested.json|.meta.owner | to_entries | map("\(.key)=\(.value)") | join("&")
5|nested.json|.items | map(.sku) | ascii_downcase? // "err"
5|nested.json|.items | map(.qty) | (max - min)
5|nested.json|[.items[] | .qty] | [., (add / length)]
5|nested.json|.items | length as $n | map(.qty / $n)
5|nested.json|[splits(", ")] | length
5|nested.json|.text | ascii
5|nested.json|.text | @base32
5|nested.json|.text | @base32 | @base32d
5|nested.json|.items | map(.price) | sort | reverse | .[0]
5|nested.json|.items | map(.price | tostring | length) | add
5|nested.json|getpath(["meta","owner","uid"])
5|nested.json|path(.meta.owner.uid)
5|nested.json|[paths(..)] | length
5|nested.json|.meta | with_entries(.key |= ascii_upcase)
5|nested.json|.meta | map_values(type)
5|nested.json|.items | map_values(.sku)
5|nested.json|.items | map(.qty) | map(. > 0) | index(false)
5|nested.json|.items | map(.qty) | any(. == 0)
5|nested.json|.items | map(.qty) | all(. >= 0)
5|nested.json|.items | map(select(.qty > 0)) | map(.sku) | @sh
5|nested.json|.items | map(.price) | map(. * 1.2 | floor)
5|nested.json|.items | [.[] | .qty] | [min, max, add]
5|nested.json|.items | map(.qty) | sort | .[1]
5|nested.json|.items | map(.price) | (add / length) | . * 100 | round / 100
5|nested.json|ltrimstr("x") | type
5|nested.json|.meta.owner.uid | . == 0
5|nested.json|.meta.owner | has("uid")
5|nested.json|.meta.owner | del(.uid) | keys
5|nested.json|.meta.owner + {"gid": 0} | keys
5|nested.json|.meta * {"owner": {"shell": "/bin/sh"}} | .owner | keys
5|nested.json|.list | length
5|nested.json|.list | first // "empty"
5|nested.json|.empty | keys
5|nested.json|.empty | length
5|nested.json|.empty | to_entries
5|nested.json|.empty == {}
5|nested.json|.list == []
5|nested.json|[.list[]] | length
5|nested.json|.meta.flags | map(. == true) | index(true)
5|nested.json|.meta.flags | map(not)
5|nested.json|.meta.flags | map(type) | unique
5|nested.json|.meta.flags | del(.[1])
5|nested.json|.meta.flags | to_entries | map(select(.value != null)) | map(.key)
5|nested.json|.meta.flags | [.[] | select(. != null)] | length
5|nested.json|.meta.flags | map(. // "nil")
5|nested.json|.meta.flags | .[0] and .[1]
5|nested.json|.meta.flags | .[0] or .[1]
5|nested.json|.meta.flags | .[2] // false | not
5|nested.json|.meta.flags | (.[0] | not) and (.[1] | not)
5|nested.json|.meta.flags | map(if . then 1 else 0 end) | add
5|nested.json|.n | . * . | floor
5|nested.json|.n | fabs | floor
5|nested.json|.n | tostring | split(".") | .[0] | tonumber
5|nested.json|.n < 0
5|nested.json|.n | if . < 0 then "neg" else "pos" end
5|nested.json|[.n, .id] | sort
5|nested.json|[.n, .id, .text] | map(type)
5|nested.json|[.n, .id, .text, .meta] | sort | map(type)
5|nested.json|[.n, null, true, "a", [], {}] | sort | map(type)
5|nested.json|.text | length
5|nested.json|.text | utf8bytelength
5|nested.json|.text | .[7:]
5|nested.json|.text | .[:5] + "…"
5|nested.json|.text | explode | map(select(. > 100)) | implode
5|nested.json|.text | split("") | reverse | join("")
5|nested.json|.text | ascii_downcase == .
5|nested.json|.text | test("^h")
5|nested.json|.text | test("^H")
5|nested.json|.text | test("^H"; "i")
5|nested.json|.text | [scan("[a-z]+")] | length
5|nested.json|.text | sub("(?<w>\\w+)"; "<\(.w)>")
5|nested.json|.text | gsub("(?<v>[aeiou])"; "\(.v | ascii_upcase)")
5|nested.json|.text | splits("[, ]+") 
5|nested.json|.text | [splits("[, ]+")]
5|nested.json|.text | ascii_upcase | ascii_downcase == .
5|nested.json|.text | tojson | fromjson == .
5|nested.json|.text | @json | length
5|nested.json|.text | @sh
5|nested.json|.text | @uri
5|nested.json|.text | @html | length
5|nested.json|[.text, .meta.created] | @csv
5|nested.json|[.text, .meta.created] | @tsv
5|nested.json|[.id, .n] | @csv
5|nested.json|{a: .id} | @json
5|nested.json|.meta.created | ascii_downcase
5|nested.json|.meta.created | split("-") | reverse | join("/")
5|nested.json|.meta.created | strptime("%Y-%m-%d") | .[0]
5|nested.json|.meta.created | strptime("%Y-%m-%d") | mktime | . > 0
5|nested.json|.meta.created + "T00:00:00Z" | fromdate | todate
5|nested.json|1757376000 | todate
5|nested.json|1757376000 | gmtime | .[0]
5|kv.json|to_entries | map(select(.value | type == "number")) | from_entries
5|kv.json|to_entries | sort_by(.key) | from_entries | keys_unsorted
5|kv.json|keys_unsorted
5|kv.json|with_entries(.value |= tojson)
5|kv.json|[to_entries[] | .key] | join("")
5|kv.json|.c | to_entries | map(.value) | add
5|kv.json|.d | map(tostring) | join("")
5|kv.json|.d | map(. * .) | add
5|kv.json|.d | reduce .[] as $x (1; . * $x)
5|kv.json|.d | [foreach .[] as $x (0; . + $x)]
5|kv.json|.d | combinations | select(length == 1)
5|kv.json|[.d, .d] | [combinations] | length
5|kv.json|.d | [.[] as $x | .[] as $y | select($x < $y) | [$x,$y]]
5|kv.json|.d | transpose? // "no"
5|kv.json|[.d, .d] | transpose
5|kv.json|.d | until(length == 0; .[1:]) | length
5|kv.json|.d | [recurse(if length > 1 then .[1:] else empty end)] | length
5|kv.json|.d | [.[] | select(. > 1)] | first
5|kv.json|.d | [limit(2; .[])]
5|kv.json|.d | [.[] | tostring | ascii_downcase] | join(",")
5|kv.json|.d | map(. * 3) | map(select(. > 5)) | length
5|kv.json|.d | (.[0] + .[1]) * .[2]
5|kv.json|.d | .[0] / .[1] | floor
5|kv.json|.d | .[2] % .[1]
5|kv.json|.d | map(. - 2) | map(fabs)
5|kv.json|.d | index(2)
5|kv.json|.d | indices(3)
5|kv.json|.d | inside([1,2,3,4])
5|kv.json|.d | contains([2])
5|kv.json|.d + .d | unique
5|kv.json|.d - [1,3]
5|kv.json|.d | .[1:] + .[:1]
5|kv.json|.d | [.[-1]] + .[:-1]
5|kv.json|.d | del(.[0])
5|kv.json|.d | del(.[0,2])
5|kv.json|.d | to_entries | map(.key)
5|kv.json|.d | with_entries(.value |= . * 10)
5|kv.json|.d | map(select(. != 2))
5|kv.json|.a = 5 | .a
5|kv.json|.a |= . + 1 | .a
5|kv.json|.e = .a + .b | .e
5|kv.json|.c.z += 1 | .c.z
5|kv.json|.c |= keys | .c
5|kv.json|.d[1] = "two" | .d
5|kv.json|del(.c) | keys
5|kv.json|del(.c.z) | .c
5|kv.json|del(.d[0]) | .d
5|kv.json|to_entries | map(select(.key != "c")) | from_entries | keys
5|kv.json|.c |= with_entries(.value |= . * 2) | .c
5|kv.json|.c | to_entries | map(.value) | add
5|kv.json|{a, c: .c.z}
5|kv.json|{a, b} | add
5|kv.json|[.a, .b] | max
5|kv.json|[.a, .b, .c.z] | sort | reverse
5|kv.json|.c | [.z, .y] | @csv
5|kv.json|.c | keys | map(ascii_upcase)
5|kv.json|.c | to_entries | map("\(.key)\(.value)") | join("")
5|kv.json|.c | map_values(. + 1)
5|kv.json|.c | map(. + 1)
5|kv.json|.c | with_entries(select(.value > 25))
5|kv.json|.c | to_entries | length
5|kv.json|.c | [.[]] | add
5|kv.json|.c | length
5|kv.json|.c | has("z")
5|kv.json|.c | has("w")
5|kv.json|.c | .z > .y
5|kv.json|.c | .z - .y == 1
5|kv.json|.c | (.z + .y) / 2 | floor
5|kv.json|.c.z | tostring | length
5|kv.json|.c.z | . * 2 | tostring
5|kv.json|.c.z | [., . + 1, . + 2]
5|kv.json|.c.z | [range(.; . + 3)]
5|kv.json|.c.z | [limit(3; repeat(. + 1))] 
5|kv.json|.c.z | [., (. | tostring)] | map(type)
5|kv.json|.c.z as $z | .c.y as $y | $z - $y
5|kv.json|.c as {z: $z} | $z
5|kv.json|. as {a: $a, d: [$first]} | $a + $first
5|kv.json|[.a, .b] as [$x, $y] | $x + $y
5|kv.json|.c | to_entries[] | select(.value == 25) | .key
5|kv.json|.c | to_entries | map(select(.value > 25)) | .[0].key
5|kv.json|[.c | to_entries[] | .key] | index("y")
5|kv.json|.c | keys | first
5|kv.json|.c | keys | last
5|kv.json|.c | keys | join("+")
5|kv.json|.c | keys | map(length) | add
5|kv.json|.c | keys | map(. + "!") | .[0]
5|kv.json|.c | keys | map(ascii) | .[0]
5|kv.json|.c | keys | map(explode[0]) | .[1]
6|nested.json|.[
6|nested.json|.a.
6|nested.json|foo
6|nested.json|.a | bar(1)
6|nested.json|{
6|nested.json|]
6|nested.json|.a +
6|nested.json|"unterminated
6|nested.json|.text | tonumber
6|nested.json|.text | .[0]
6|nested.json|.text | keys
6|nested.json|.id | keys
6|nested.json|.id | .a
6|nested.json|.id | .[0]
6|nested.json|.id + "x"
6|nested.json|.items | .a
6|nested.json|.meta | .[0]
6|nested.json|.text | length | .a
6|nested.json|error("boom")
6|nested.json|.id | error
6|nested.json|{a: 1} | error
6|nested.json|null | error
6|nested.json|.text | error("\(.)")
6|nested.json|.items[] | if .qty == 0 then error("zero qty") else .sku end
6|nested.json|.text | tonumber? // "not a number"
6|nested.json|try (.text | tonumber) catch "caught"
6|nested.json|try error("x") catch .
6|nested.json|try (.id | .a) catch "caught"
6|nested.json|(.id | .a)? // "safe"
6|nested.json|.text | try tonumber catch "nope"
6|nested.json|[.items[] | try (.sku | tonumber) catch "bad"]
6|nested.json|empty
6|nested.json|.items[] | empty
6|nested.json|[empty]
6|nested.json|[.items[] | empty] | length
6|nested.json|.id | select(. > 100)
6|nested.json|.id | select(. > 100) // "none"
6|nested.json|[.id | select(. > 100)] | length
6|nested.json|first(empty) // "nothing"
6|nested.json|limit(0; .items[])
6|nested.json|[limit(0; .items[])]
6|nested.json|.text | @base64d
6|nested.json|"====" | @base64d
6|nested.json|"!!!" | @base64d | length
6|nested.json|.id | @base64
6|nested.json|.items | @csv
6|nested.json|.items | @tsv
6|nested.json|.meta | @sh
6|nested.json|[.meta] | @csv
6|nested.json|.id | ascii_upcase
6|nested.json|.id | split(",")
6|nested.json|.id | test("x")
6|nested.json|.id | ltrimstr("x")
6|nested.json|.text | ltrimstr(1)
6|nested.json|.id | startswith("x")
6|nested.json|.text | startswith(1)
6|nested.json|.id | explode
6|nested.json|.text | implode
6|nested.json|.meta | implode
6|nested.json|.id | fromjson
6|nested.json|"{" | fromjson
6|nested.json|"nope" | fromjson
6|nested.json|.text | fromjson? // "invalid"
6|nested.json|.id | strptime("%Y")
6|nested.json|.text | strptime("%Y-%m-%d")
6|nested.json|.text | strptime("%Y-%m-%d")? // "bad date"
6|nested.json|.id | todate | length > 0
6|nested.json|.text | todate
6|nested.json|.id | mktime
6|nested.json|.id | gmtime | length
6|nested.json|.text | gmtime
6|nested.json|.id | tostring | tonumber == .id
6|nested.json|.n | tostring | tonumber == .n
6|nested.json|.meta | tostring | length > 0
6|nested.json|.meta | tonumber
6|nested.json|.list | tonumber
6|nested.json|.meta.flags[2] | tonumber
6|nested.json|.meta.flags[0] | tonumber
6|nested.json|.meta.flags[0] | tostring
6|nested.json|.meta.flags[2] | tostring
6|nested.json|.meta.flags[2] | length
6|nested.json|.meta.flags[0] | length
6|nested.json|.n | length
6|nested.json|.id | -length
6|nested.json|.meta.flags[2] | keys
6|nested.json|.meta.flags[2] | .[0]
6|nested.json|.meta.flags[2] | .a
6|nested.json|.meta.flags[2] | .[]
6|nested.json|.meta.flags[2] | .[]?
6|nested.json|[.meta.flags[2] | .[]?] | length
6|nested.json|.meta.flags[2] | has("a")
6|nested.json|.meta.flags[2] | to_entries
6|nested.json|.meta.flags[2] | add
6|nested.json|.meta.flags[2] | sort
6|nested.json|.meta.flags[2] | unique
6|nested.json|.meta.flags[2] | reverse
6|nested.json|.meta.flags[2] | join(",")
6|nested.json|.meta.flags[2] | map(.)
6|nested.json|.meta.flags[2] | first
6|nested.json|.meta.flags[2] | flatten
6|nested.json|.meta.flags[2] | tojson
6|nested.json|.meta.flags[2] | @json
6|nested.json|.meta.flags[2] | @csv
6|nested.json|.meta.flags[2] | @base64
6|nested.json|.meta.flags[2] | ascii_downcase
6|nested.json|.meta.flags[2] | not
6|nested.json|.meta.flags[2] | . == null
6|nested.json|.meta.flags[2] | . // "d"
6|nested.json|.meta.flags[2] | if . then 1 else 0 end
6|nested.json|.meta.flags[2] | type
6|nested.json|.meta.flags[2] | isnan? // "not number"
6|nested.json|.meta.flags[2] | floor
6|nested.json|.meta.flags[2] | fabs
6|nested.json|.meta.flags[2] | sqrt
6|nested.json|.meta.flags[2] | . + 1
6|nested.json|.meta.flags[2] | . + "a"
6|nested.json|.meta.flags[2] | . + [1]
6|nested.json|.meta.flags[2] | . + {}
6|nested.json|.meta.flags[2] | . + null
6|nested.json|.meta.flags[2] | . - 1
6|nested.json|.meta.flags[2] | . * 2
6|nested.json|.meta.flags[2] | . / 2
6|nested.json|.meta.flags[2] | . % 2
6|nested.json|.meta.flags[2] | . < 1
6|nested.json|.meta.flags[2] | . > 1
6|nested.json|.meta.flags[2] | . == false
6|nested.json|.meta.flags[2] | . and true
6|nested.json|.meta.flags[2] | . or true
6|nested.json|.meta.flags[2] | [.] | length
6|nested.json|.meta.flags[2] | {a: .} | .a
6|nested.json|.meta.flags[2] | [., .] | unique | length
6|nested.json|.meta.flags[2] | tostring | ascii_upcase
6|nested.json|.meta.flags[2] | @text
6|nested.json|.meta.flags[2] | @sh
6|nested.json|.meta.flags[2] | @html
6|nested.json|.meta.flags[2] | @uri
6|nested.json|.meta.flags[2] | @tsv
6|nested.json|[.meta.flags[2]] | @tsv
6|nested.json|[.meta.flags[2]] | @csv
6|nested.json|[.meta.flags[2]] | @sh
6|nested.json|[.meta.flags[2]] | @json
6|nested.json|[.meta.flags[2]] | @base64
6|nested.json|[.meta.flags[2]] | @base64 | @base64d
6|nested.json|[.meta.flags[2]] | tojson | fromjson
6|nested.json|[.meta.flags[2]] | tostring | length
6|nested.json|[.meta.flags[2]] | add
6|nested.json|[.meta.flags[2]] | sort
6|nested.json|[.meta.flags[2]] | unique
6|nested.json|[.meta.flags[2]] | flatten
6|nested.json|[.meta.flags[2]] | reverse
6|nested.json|[.meta.flags[2]] | join(",")
6|nested.json|[.meta.flags[2]] | map(. // 0)
6|nested.json|[.meta.flags[2]] | first
6|nested.json|[.meta.flags[2]] | last
6|nested.json|[.meta.flags[2]] | length
6|nested.json|[.meta.flags[2]] | index(null)
6|nested.json|[.meta.flags[2]] | contains([null])
6|nested.json|[.meta.flags[2]] | inside([null, 1])
6|nested.json|[.meta.flags[2]] | has(0)
6|nested.json|[.meta.flags[2]] | has(1)
6|nested.json|[.meta.flags[2]] | keys
6|nested.json|[.meta.flags[2]] | to_entries
6|invalid.json|.
6|invalid.json|.a
6|invalid.json|length
6|invalid.json|keys
6|invalid.json|.a?
6|invalid.json|try . catch "x"
6|invalid.json|empty
6|invalid.json|error("x")
6|invalid.json|"literal"
EOF

# ---------- generate ----------
n=0; ok=0; err=0
while IFS='|' read -r tier input filter; do
  [ -z "$tier" ] && continue
  n=$((n+1))
  d="$OUT/cases/$(printf '%03d' "$n")"
  mkdir -p "$d"
  printf '%s' "$filter" > "$d/filter"
  cp "$OUT/inputs/$input" "$d/input.json"
  echo "$tier" > "$d/tier"
  set +e
  jq -c "$filter" < "$d/input.json" > "$d/expected" 2> "$d/stderr"
  code=$?
  set -e
  echo "$code" > "$d/exit"
  if [ "$code" -eq 0 ]; then ok=$((ok+1)); else err=$((err+1)); fi
done < "$OUT/cases.txt"

# ---------- runner ----------
cat > "$OUT/run.sh" <<'EOF'
#!/usr/bin/env bash
# Usage: evals/run.sh path/to/rjq [tier]
# Prints "N/TOTAL OK" then one line per failure: case, tier, filter, expected, got, exit.
set -u
BIN="${1:?path to rjq binary}"
TIER="${2:-}"
DIR="$(cd "$(dirname "$0")" && pwd)"
total=0; pass=0; fails=()
for d in "$DIR"/cases/*/; do
  t=$(cat "$d/tier")
  [ -n "$TIER" ] && [ "$t" != "$TIER" ] && continue
  total=$((total+1))
  f=$(cat "$d/filter")
  want=$(cat "$d/expected")
  wexit=$(cat "$d/exit")
  got=$(timeout 5 "$BIN" "$f" < "$d/input.json" 2>/dev/null)
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
EOF
chmod +x "$OUT/run.sh"

echo "generated $n cases in $OUT/ ($ok exit 0, $err non-zero)"
echo "tiers: $(for t in 1 2 3 4 5 6; do printf 't%s=%s ' "$t" "$(grep -lx "$t" "$OUT"/cases/*/tier | wc -l)"; done)"
echo "self-check with real jq:"
printf '#!/bin/sh\nexec jq -c "$@"\n' > /tmp/jqc && chmod +x /tmp/jqc
"$OUT/run.sh" /tmp/jqc | head -1
