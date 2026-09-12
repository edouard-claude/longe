<h1 align="center">Longe</h1>

<p align="center">
  <b>A self-improving harness for any LLM, in one Rust binary.</b><br>
  <i>The model is the engine; the runtime is the car.</i>
</p>

<p align="center">
  <a href="https://github.com/edouard-claude/longe/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/edouard-claude/longe/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="Rust 1.85+" src="https://img.shields.io/badge/rust-1.85%2B-orange?logo=rust&logoColor=white">
  <img alt="unsafe: forbidden" src="https://img.shields.io/badge/unsafe-forbidden-success.svg">
  <img alt="one binary" src="https://img.shields.io/badge/binaries-1-informational.svg">
  <img alt="platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg">
</p>

<p align="center">
  <a href="#the-thesis-we-build-on">Thesis</a> ·
  <a href="#the-authors-and-what-we-took-from-each">Credits</a> ·
  <a href="#what-longe-is">Architecture</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#the-lua-surface">Lua surface</a> ·
  <a href="#sandbox">Sandbox</a> ·
  <a href="#benchmark-prd-section-8">Benchmark</a>
</p>

A *longe* is the long rein used to let a horse run in a circle: it lets the animal
go, and it sets the radius and the duration. Longe does the same for a language
model. It lets the model run, and it imposes the budget, the verifier and the sandbox.

Longe is a from-scratch implementation of the ideas presented at the
**YC Harness Club** evening (Y Combinator, 2026), where four projects made the same
argument from four directions: Prime Agent and Continual Harness (Prime Intellect,
Princeton), Open Jarvis (Stanford, Hazy Research) and QM (Y Combinator). This README
credits those authors, states their thesis, and shows where each idea lives in the
code.

---

## The thesis we build on

> "I'm not sure this kind of prompt engineering belongs at a top tier machine learning
> conference." (a reviewer, quoted at the opening of the evening)

The evening's answer, and ours: **the harness is not a wrapper, it is the part of the
system that turns a fixed weight file into an agent.** The same weights give an 18 %
gap between two harnesses. Claude Opus scored about 30 % on the private ARC-AGI-3
hold-out; with a general-purpose harness and no ARC-specific tuning, Prime Agent took
it to 95.5 %, and NVIDIA's AVO to 100 %. Nothing changed in the model. Everything
changed around it.

The host framed six years of progress as two eras:

1. **The static harness era.** Each step added a capability to the loop around the
   model, and none of it was "research": few-shot examples (GPT-3, 2020), chain of
   thought, tool calling (WebGPT, Toolformer), read/write access to one's own context
   (MemGPT), skills distilled from experience (Voyager), code as the action space
   (InterCode), reflection and self-critique (ReAct, Self-Refine, Reflexion),
   multi-agent spawning, and finally recursion (Recursive Language Models). The
   "harness v1" is an agent spec: a system prompt, turn and tool limits, a tool list,
   a skill list, a sub-agent list, in a loop.
2. **The self-improving harness era.** DSPy learns the prompt; Darwin Gödel machines
   learn the harness code itself; Continual Harness gives the agent CRUD over its own
   skills, memory, sub-agents and system prompt, and scores the result.

Longe is a harness v1 with the self-improvement loop of era two bolted on, packaged
as one binary you can point at any model, API or local.

---

## The authors and what we took from each

### Prime Agent and Continual Harness (Seth, Prime Intellect / Princeton; RLM co-author Alex Zhang)

Seth's talk asked for a first-principles view: a raw LLM is a sequential processor,
tokens in, tokens out, with fixed weights and a visible context. The harness is the
layer between it and the world that adds **persistent state, tools and compute**.
Three ideas structure Longe:

- **The memory hierarchy as a cache.** L1 is the model's context (the fastest, and the
  first thing to overflow; compaction is the oldest harness feature and still the most
  general). L2 is a live REPL: variables in RAM the agent manipulates programmatically
  instead of pushing them through the context. L3 is the file system: skills, memory,
  prompt, all under explicit create/read/update/delete. Longe implements exactly this
  ladder: `compact.rs` (L1), the persistent Lua REPL in `repl/` (L2), and the git
  versioned store in `store.rs` (L3).
- **A Turing machine becomes a von Neumann computer.** A model that can only append to
  its tape is weaker than one that can read and write external memory. So the single
  tool exposed to the model is `exec(code)`, a Lua 5.4 VM whose globals survive
  across turns and across daemon restarts. Every other capability is a native
  binding inside that VM: `fs`, `sh`, `mem`, `skill`, `prompt`, `agent`, `llm`,
  `model`, `compact`, `verify`, `note`, `done`.
- **Persistent sub-agents as a nuclear family.** Sub-agents are not one-shot calls;
  they are sessions that finish, go idle in RAM, get offloaded to disk, and are woken
  by a message. Parents, children and siblings message each other directly, because,
  in Seth's words, it is far better for them to share context than for the human to
  relay it. This is `session/tree.rs` and `session/bus.rs`.

From Continual Harness we took the post-run reflection: load the full trajectory,
ask a model what worked, what failed and what cost the most, apply its proposed diffs
to skills, memory, sub-agent specs, prompt and config on a git branch, replay a
fitness set, keep or roll back. That is `reflect.rs`. From the RLM paper we took the
leaf: `llm.query` is a stateless model call the agent can invoke from inside its REPL,
recursively, on data it never had to put in its own context.

Seth also insisted on **long-horizon evaluation**: run a model until its practical
plateau, not until the first budget cut, and compare at equal spend. Longe's budget is
therefore a floor before it is a ceiling.

### Open Jarvis (John and Ivanka, Stanford, Hazy Research, advised by Christopher Ré)

Open Jarvis argues that personal AI is cloud-bound for no good reason: local models
trail the frontier by six to twelve months, laptops now ship accelerators, and a
year of API calls costs thousands of dollars, your private data, and orders of
magnitude more energy. Its move is to describe the whole stack with five primitives
(interfaces, agentic logic, intelligence, inference engine, tools and memory plus a
learning system) and to let a **cloud model optimise the local stack**, which then
runs at roughly 800 times lower cost.

Longe keeps the provider layer deliberately thin and symmetric so the same harness
drives a frontier API and a model on the machine: `llm/anthropic.rs`,
`llm/openai.rs` (DeepSeek, OpenRouter, any compatible endpoint) and `llm/ollama.rs`
(the native Ollama API, which returns clean content from thinking models). The model
can switch providers mid-session with `model.switch` without losing state, so a
cheap local model can do the grind and a frontier model the hard steps, or a strong
model can be used to optimise the store that a local one will run with. The PRD's
benchmark protocol runs the same task with an API model and a local Ollama model for
exactly this reason.

### QM (Josh and Regan, Y Combinator)

QM is YC's open-source agent for work: every employee gets an OpenClaw-like assistant
with its own files and crons, reachable from Slack or the web. Its lessons are
operational, and they shaped Longe's daemon:

- **Pull the brain out of the sandbox.** OpenClaw gives the agent its own computer,
  which makes it powerful and traps it: sessions live and die inside that box, and a
  fleet of them is a whack-a-mole. QM centralises state and treats sandboxes as a
  resource the agent dips into. Longe keeps sessions, history, Lua state, trajectories
  and the store outside the sandbox, in the daemon (`daemon.rs`), and `sh()` gets a
  per-process sandbox with a policy (`sandbox/`). Closing the terminal closes the
  client, never the session.
- **Keep the harness extremely thin.** QM's core is three tools (execute in a remote
  sandbox, read/write object storage, publish an app); everything else is a temporary
  patch over rough edges. Longe's core is one tool and a policy.
- **Agents give up too early.** QM's "grind" sets a wall-clock or token budget the
  agent is not allowed to finish before, and it produces markedly better research and
  reports; the same technique is behind recent open-problem results in math. Longe's
  `budget.rs` refuses `done()` before the minimum and forces a clean exit at the
  maximum, and `done()` is also refused until the external verifier passes.
- **Human-reviewed writes.** QM lets the agent propose bulk database edits that a
  person rubber-stamps. Longe applies reflection diffs to a git branch and lets the
  fitness replay, or a human in the cockpit, accept or reject them.
- **Social context is a permission system.** An agent leaks privileged information
  into contexts where it does not belong unless the runtime bounds what it can see.
  Longe's fixed sandbox rules make the store, `~/.ssh`, credential files, `.env` and
  the binary itself invisible to `sh()` whatever the mode, and the model can only edit
  its store through the Rust bindings.

### The lineage the evening credited

Karpathy's auto-researcher (the host's own cockpit grew out of forking it), DSPy
(Omar Khattab et al.), Darwin Gödel machines, Voyager, MemGPT, ReAct, Self-Refine,
Reflexion, Toolformer, WebGPT, InterCode, Recursive Language Models, and ARC-AGI-3
(François Chollet and Greg Kamradt) as the measure of how fast a system adapts to a
new distribution. Names above follow the talk; the projects are the real citation.

---

## What Longe is

One binary, `longe`, that runs a background daemon, `longed`, owning a tree of
persistent sessions.

```text
┌─ longed ────────────────────────────────────────────────────┐
│  scheduler ── cron ── budget guard ── reflect job            │
│      │                                                       │
│      ▼                                                       │
│  session tree                                                │
│   root ──┬── child A (idle, state in RAM)                    │
│          ├── child B (running)                               │
│          └── child C (offloaded, serialized on disk)         │
│      │         ▲                                             │
│      │         └── message bus (parent / child / siblings)   │
│      ▼                                                       │
│  session = { history, lua state, sandbox, budget }           │
│      │                                                       │
│      ▼                                                       │
│  context compiler → llm client → parser → lua repl           │
│                                              │               │
│                        verifier ◄────────────┘               │
│                                                              │
│  store (git) : prompt.md skills/ memory/ subagents/          │
│                sessions/ trajectories/ evals/ harness.toml   │
└──────────────────────────────────────────────────────────────┘
       ▲
       │ unix socket + HTTP JSON
  cockpit TUI / curl / tailscale
```

The session loop, per turn:

```text
drain bus → compile context (prompt.md + protocol + store index + lua state size
+ budget + last verify) → llm → parse
  Exec(code) → lua.exec → feedback (head, `[repl] max_output_bytes`; rest in _last) → apply effects
  Done(s)    → budget.ok() && verify.ok() ? finish : refusal pushed back
  Text       → "reply with one lua block"
context > 70 % of window → compact (task stays in the system prompt)
budget exhausted → forced clean finish
finish → report to parent, trajectory written, state → idle, reflect scheduled
```

---

## Build

```bash
cargo build --release      # one binary: target/release/longe
```

Requires a Rust toolchain and `git` on the path (the store is versioned with it).

## Quick start

```bash
export LONGE_STORE=~/.longe/store          # prompt.md, skills/, memory/, harness.toml, git
longe daemon                               # start longed in the background
longe run "Implement rjq, a jq clone in Rust, stdlib only" --workspace ./rjq
longe cockpit                              # TUI: tree, budget, notes, trajectory, message box
```

`Ctrl-C` on `run` or `tail` detaches; the session keeps running in the daemon.
Reattach with `longe tail <id>`. `longe ls`, `longe show <id>`, `longe send <id>
"..."`, `longe pause|resume|kill|offload <id>`, `longe reflect <id>`, `longe diffs`,
`longe accept|reject <branch>`, `longe log`, `longe stop`.

The HTTP surface mirrors the socket on `127.0.0.1:7878` (`GET /sessions`,
`GET /sessions/{id}`, `POST /sessions/{id}/message`, `GET /store/diffs`, ...) and is
meant to be exposed through tailscale.

## The cockpit

`longe cockpit` opens a TUI over the daemon socket: the session tree on the left, the
selected session's budget, last note and verifier state on the right, a live tail of
its trajectory below, and a box to message any session, running or asleep.

```text
┌ sessions (3) ────────────────────┐┌ detail ──────────────────────────────────────┐
│ 4a49e954 running root t12 41k    ││ root (4a49e954) parent=- model=openrouter/…  │
│   c73551ed idle   parser t8 9k   ││ state=Running outcome=-                      │
│     [done: parser ready]         ││ budget: turn 12/400 (min 20) tokens 41k/6M   │
│   ffa656ff paused tester t3 2k   ││   elapsed≈610s (min 3600s) refused 1         │
│                                  ││ verify: FAILED  pending msgs: 0              │
│                                  ││ note: parser OK, evaluator next              │
│                                  ││ task: Implement rjq, a jq clone in Rust…     │
│                                  │└──────────────────────────────────────────────┘
│                                  │┌ trajectory tail ─────────────────────────────┐
│                                  ││ t10 exec: fs.write('src/parse.rs', … → ok    │
│                                  ││ t11 verify ok=false (42s)                    │
│                                  ││ t11 done refused: verifier failed… 612/693   │
│                                  ││ t12 note: parser OK, evaluator next          │
│                                  ││ t12 message from parser: report…             │
└──────────────────────────────────┘└──────────────────────────────────────────────┘
┌ message: press i to type ───────────────────────────────────────────────────────┐
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
↑↓ select  i message  p pause  r resume  k kill  o offload  R reflect  d diffs  q quit
```

Each event is one line, coloured by kind, with the Lua of an `exec` lightly
highlighted: keywords, strings, numbers, comments and the runtime bindings. The tail
keeps exactly the events that fit once wrapped, so the newest one is always the last
visible row.

`d` switches to the pending `reflect/` branches: the list on the left, the git diff on
the right, `a` to accept, `x` to reject. Everything refreshes over the socket, so the
cockpit can run on another machine through tailscale.

## Configuration

Everything lives in `<store>/harness.toml`: default model, providers, budget,
verifier command, sandbox mode, reflection, crons. `longe init` writes the default.

```toml
[model]
provider = "ollama"            # or anthropic, deepseek, openrouter, any [providers.x]
name = "qwen2.5-coder:14b"
context_window = 32768

[providers.openrouter]
kind = "openai"
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"   # the name of the variable, never the key

[budget]
min_seconds = 1800     # done() refused before this
min_turns   = 20
max_turns   = 400      # forced clean exit at this
max_tokens  = 4000000

[verify]
command = "cargo test && cargo clippy -- -D warnings && $LONGE_EVALS/run.sh target/release/rjq"

[sandbox]
mode = "workspace-write"   # read-only | workspace-write | full-access
network = false

[reflect]
enabled = true
fitness = "evals/fitness.sh"   # last stdout line is the score; keep if >= previous

[[cron]]
name = "nightly-reflect"
schedule = "0 3 * * *"
action = "reflect"
```

Any extra key under a provider is merged into the request body (for example
`think = false` for Ollama models that support it).

## The Lua surface

The model sees this reference in its system prompt:

```text
fs.read(p) | fs.write(p, s) | fs.list(d) | fs.rm(p)           confined to the workspace
fs.lines(p, from, to) -> numbered lines | fs.grep(text, p)    read by ranges, find sections
sh(cmd, {timeout=s}) -> {stdout, stderr, code, timed_out}      sandboxed, store invisible
mem.get/set/del/list/search   skill.get/set/del/list   subagent.get/set/del/list
prompt.get() / prompt.set(s)                                   its own system prompt
agent.spawn(name, task, {model, budget, workspace}) -> id
agent.id() | agent.send(id, s) | agent.recv() | agent.list() | agent.pause/resume/kill(id)
llm.query(prompt, {model, system}) -> text                     the RLM leaf, stateless
model.switch(provider, name)                                   state is kept
compact(hint) | verify() -> {ok, report} | note(s) | done(summary)
```

Output over `[repl] max_output_bytes` (8 KB by default, 16 KB from `longe init`) is cut
after its head and kept whole in `_last`. Globals persist; the serialized
size and the largest globals are shown to the model so it can garbage-collect its own
state.

## Sandbox

`sh()` runs confined behind one `Policy` and one `Sandbox` trait with a native
backend per OS: a generated Seatbelt profile on macOS (`sandbox-exec -p`), Landlock on
Linux (applied by a helper invocation of the binary, no `unsafe`), a restricted-token
stub on Windows. Three modes (read-only, workspace-write, full-access) and fixed rules
no mode lifts: the store, `~/.ssh`, credential files, `.env` files and the binary are
invisible; secrets are scrubbed from the environment; network is denied unless
configured. The verifier runs under the same confinement with the evals visible.

## Reflection

On every accepted `done()` and on cron, the runtime condenses the trajectory (code,
outputs, errors, notes, verify results, refusals), asks a model for full-file changes
to `skills/`, `memory/`, `subagents/`, `prompt.md` and `harness.toml`, validates the
paths (the `[verify]`, `[sandbox]`, `[reflect]`, `[daemon]` and `[providers]`
sections are immutable to reflection), commits them on a `reflect/<session>` branch,
runs the fitness command on main and on the branch, and merges with `--no-ff` when
the score does not drop, or deletes the branch. Without a fitness command the branch
waits for a human in the cockpit. Everything is logged to `reflect.log` and visible
in `git log`.

## Benchmark (PRD section 8)

The reference task is `rjq`, a `jq` clone in Rust. `gen-evals.sh` generates 693
cases across six tiers with the real `jq` (self-check 693/693); `bench/gen-evals2.sh`
generates 129 cases for the CLI-options follow-up task. The generated cases are not
versioned: each run derives the expected outputs from whichever `jq` is installed, so
the set is always self-consistent. A few expectations do depend on the `jq` version
(`ltrimstr` on a non-string errors in 1.8 and does not in 1.7), which is exactly why
the cases are regenerated rather than committed. `bench/ab.sh` runs protocol A
(a plain JSON-tool loop, `longe baseline`) against protocol B (Longe, everything on)
and a second B run, then prints green cases, tokens, turns, wall time, refused
`done()` calls, sub-agents spawned and reflection diffs proposed and accepted.

## Tests

```bash
cargo test                                      # 91 unit + end-to-end tests
cargo test --test live_ollama -- --nocapture    # live smoke test if Ollama is up
cargo clippy --all-targets --all-features -- -D warnings
```

The end-to-end tests drive the real daemon tree, Lua VM, store and git through a
scripted fake model server: sub-agent spawn/send/recv and wake from disk, `done()`
refused before the budget and while the verifier fails, model switch with state kept,
compaction that keeps the task, persistence across a daemon restart, pause/resume/kill,
budget exhaustion, reflection producing a merged diff, and the HTTP surface. The
sandbox tests exercise the real Seatbelt backend.

## Layout

```text
src/
  main.rs          CLI          daemon.rs      socket, HTTP, crons, reflect trigger
  agent_loop.rs    the loop     client.rs      talk to longed, start it on demand
  session/         ids, state, bus, tree actor
  repl/            Lua VM, prelude, bindings/{fs,sh,store,agent,llm,control}
  llm/             provider types, anthropic, openai, ollama, registry with retry
  sandbox/         policy, trait, unconfined, macos (seatbelt), linux (landlock), windows
  store.rs  budget.rs  verify.rs  compact.rs  reflect.rs  parse.rs  cron.rs  baseline.rs
  cockpit/         http.rs (axum), tui.rs (ratatui)
docs/PRD.md        the specification      docs/STATUS.md   acceptance evidence
gen-evals.sh  bench/gen-evals2.sh  bench/ab.sh
```

## Status

See `docs/STATUS.md` for the acceptance table. Twelve of the thirteen criteria are
verified by tests or commands; the thirteenth, both providers solving rjq to 693/693,
needs a capable API model and its key.

## License

MIT.
