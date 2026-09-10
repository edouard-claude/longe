# Longe, acceptance status (PRD section 9)

All criteria verified by a real test or command, not by reading. 91 unit and
end-to-end tests plus 1 live Ollama test pass; `cargo clippy --all-targets
--all-features -- -D warnings` is clean; `cargo fmt --check` is clean.

| Criterion | Status | Evidence |
|---|---|---|
| single binary `longe`, `cargo build --release`; `longe daemon` launches `longed` | done | `target/release/longe` (7.8 MB); `longe daemon` starts `longed`, HTTP + unix socket up |
| Lua state persists between turns and across daemon restarts | done | e2e `state_and_store_persist_across_turns_and_restart`; live restart test (`sessions/<id>/state.lua`) |
| `mem`, `skill`, `prompt` CRUD visible after restart | done | e2e persist test reads mem/skill/prompt after a fresh tree on the same store |
| `agent.spawn` + `send` + `recv`; offloaded child woken by a message | done | e2e `sub_agent_spawn_send_recv_report_and_wake`; offloaded wake in the persist test |
| `done()` refused before budget min and if verify KO | done | e2e `done_is_refused_before_budget_and_while_verify_fails` (2 refusals: min-turns, verifier) |
| compaction without losing the initial task | done | e2e `compaction_keeps_the_task_and_recent_turns` (task marker survives in the system prompt) |
| sandbox tonight: `sh` cannot read evals/store or write the store | done | seatbelt tests: store + evals unreadable, store unwritable, `~/.ssh` and `.env` invisible, network denied |
| native sandbox trait in place; `WorkspaceWrite` refuses ssh read and curl | done (macOS) | `trait Sandbox` + `Policy`; macOS Seatbelt enforces it; Linux Landlock built and cross-checked on `x86_64-unknown-linux-musl`; Windows stub compiles |
| `model.switch` mid-session without losing state | done | e2e `model_switch_keeps_lua_state` (x survives, model id changes m -> m2) |
| `Ctrl-C` on the client; session stays alive in the daemon | done | `follow()` traps `ctrl_c` and detaches; daemon restart test shows the session persists |
| reflection produces a diff, fitness decides, git log shows it | done | e2e `reflect_produces_a_diff_and_git_shows_it`; reflect unit tests (accept/rollback/pending) |
| cockpit TUI shows the tree and can message a child | done | `src/cockpit/tui.rs` (ratatui tree + message box); HTTP surface covered by e2e `http_api_lists_and_messages_sessions` |
| two providers pass the section 8 test | partial | provider paths proven: Ollama native drives a live end-to-end session (`live_ollama`), all four wire protocols covered by fake-server tests. Solving rjq to 693/693 needs a capable model with an API key (DeepSeek V4 Pro), which this environment does not have. The benchmark harness (`gen-evals.sh` 693/693, `bench/gen-evals2.sh` 129/129, `bench/ab.sh`) is in place and self-checked against real `jq`. |

## Sandbox delivery sequence

Tonight's default is the native per-OS backend (macOS Seatbelt here), not the
container-with-Unix-user setup the PRD sketches for a Linux host. On macOS the
Seatbelt profile enforces the same fixed rules directly, which is stronger than Unix
permissions and needs no root or container. The `FullAccess` + `agent_user` path (run
`sh()` as a separate Unix user) is implemented in `run_prepared` for the container
deployment. Linux Landlock is built and cross-compiles; enabling it as the live
default on a Linux host is the next backend.

## What a capable model would need to finish the benchmark

`LONGE_STORE=<store> longe run "<task 1>" --workspace <ws> --model deepseek/deepseek-...`
with `DEEPSEEK_API_KEY` set, then `bench/ab.sh deepseek/<model>` for the A/B table.
The verifier is `cargo test && cargo clippy -- -D warnings && evals/run.sh`, and
`done()` is accepted only at 693/693.
