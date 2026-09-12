//! Longe: one binary, one REPL tool, persistent sub-agents, budgets, verification,
//! sandbox, reflection. The model is the engine; the runtime is the car.

mod agent_loop;
mod baseline;
mod budget;
mod client;
mod cockpit;
mod compact;
mod config;
mod cron;
mod daemon;
mod llm;
mod parse;
mod reflect;
mod repl;
mod sandbox;
mod session;
mod store;
mod verify;

#[cfg(test)]
mod e2e_tests;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::config::{BudgetCfg, ModelRef};
use crate::daemon::proto::Request;

#[derive(Parser)]
#[command(
    name = "longe",
    version,
    about = "A self-improving harness for any LLM. The model is the engine; the runtime is the car."
)]
struct Cli {
    /// Store directory (default: $LONGE_STORE or ./store).
    #[arg(long, global = true, env = "LONGE_STORE", default_value = "store")]
    store: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a store skeleton (prompt.md, harness.toml, git).
    Init,
    /// Start the daemon (`longed`) in the background.
    Daemon {
        /// Stay in the foreground (logs to stderr).
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the daemon.
    Stop,
    /// Start a root session and follow it (Ctrl-C detaches, the session keeps running).
    Run {
        task: String,
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long, default_value = "root")]
        name: String,
        /// provider/name
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        min_seconds: Option<u64>,
        #[arg(long)]
        min_turns: Option<u32>,
        #[arg(long)]
        max_turns: Option<u32>,
        #[arg(long)]
        max_tokens: Option<u64>,
        /// Return immediately after spawning.
        #[arg(long)]
        detach: bool,
    },
    /// List sessions.
    Ls,
    /// Show one session.
    Show {
        id: String,
        #[arg(long, default_value_t = 20)]
        tail: usize,
    },
    /// Follow a session's trajectory.
    Tail {
        id: String,
    },
    /// Send a message to a session.
    Send {
        id: String,
        message: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Kill {
        id: String,
    },
    Offload {
        id: String,
    },
    /// Open the cockpit TUI.
    Cockpit,
    /// Reflect over a finished session now.
    Reflect {
        id: String,
    },
    /// Pending reflection branches.
    Diffs,
    Accept {
        branch: String,
    },
    Reject {
        branch: String,
    },
    /// Store git log.
    Log {
        #[arg(long, default_value_t = 20)]
        n: usize,
    },
    /// Protocol A: plain JSON-tool loop, no harness. Writes runs/<label>.json.
    Baseline {
        task: String,
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 400)]
        max_turns: u32,
        #[arg(long, default_value = "A")]
        label: String,
    },
    /// Wait for a session to finish and print its outcome (for scripts).
    Wait {
        id: String,
        #[arg(long, default_value_t = 0)]
        timeout_seconds: u64,
    },
    #[command(hide = true, name = "__daemon")]
    DaemonFg,
    #[command(hide = true, name = "__sandbox-exec")]
    SandboxExec {
        spec: String,
        #[arg(trailing_var_arg = true)]
        argv: Vec<String>,
    },
}

fn parse_model(s: &str) -> anyhow::Result<ModelRef> {
    let (p, n) = s.split_once('/').context("model must be provider/name")?;
    Ok(ModelRef {
        provider: p.into(),
        name: n.into(),
    })
}

fn init_tracing(stderr: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,hyper=warn,reqwest=warn".into());
    let b = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false);
    if stderr {
        b.with_writer(std::io::stderr).init();
    } else {
        b.with_ansi(false).init();
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let store_root = cli.store.clone();
    match cli.cmd {
        Cmd::SandboxExec { spec, argv } => return sandbox_exec(&spec, argv),
        Cmd::DaemonFg => {
            init_tracing(false);
            return daemon::serve(store_root).await;
        }
        Cmd::Daemon { foreground: true } => {
            init_tracing(true);
            return daemon::serve(store_root).await;
        }
        _ => {}
    }
    init_tracing(true);
    let store = store::Store::open(&store_root)?;
    let store_root = std::fs::canonicalize(store.root())?;
    let harness = store.harness()?;
    let socket = daemon::socket_path(&store_root, &harness);
    let call = |req: Request| {
        let socket = socket.clone();
        async move {
            client::call(&socket, &req)
                .await
                .map_err(anyhow::Error::from)
        }
    };
    match cli.cmd {
        Cmd::Init => {
            println!("store ready at {}", store_root.display());
            println!(
                "edit {}/harness.toml, then: longe run \"task\" --workspace <dir>",
                store_root.display()
            );
        }
        Cmd::Daemon { .. } => {
            client::ensure_daemon(&store_root, &socket).await?;
            let v = call(Request::Ping).await?;
            println!("longed running (pid {}) on {}", v["pid"], socket.display());
        }
        Cmd::Stop => {
            let _ = call(Request::Shutdown).await?;
            println!("stopping");
        }
        Cmd::Run {
            task,
            workspace,
            name,
            model,
            min_seconds,
            min_turns,
            max_turns,
            max_tokens,
            detach,
        } => {
            client::ensure_daemon(&store_root, &socket).await?;
            let model = model.as_deref().map(parse_model).transpose()?;
            let b = harness.budget;
            let budget = Some(BudgetCfg {
                min_seconds: min_seconds.unwrap_or(b.min_seconds),
                min_turns: min_turns.unwrap_or(b.min_turns),
                max_turns: max_turns.unwrap_or(b.max_turns),
                max_tokens: max_tokens.unwrap_or(b.max_tokens),
            });
            let workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
            let v = call(Request::Spawn {
                name,
                task,
                workspace,
                model,
                budget,
            })
            .await?;
            let id = v["id"].as_str().unwrap_or_default().to_string();
            println!("session {id} started");
            if !detach {
                follow(&socket, &id).await?;
            }
        }
        Cmd::Ls => {
            let v = call(Request::List).await?;
            let list: Vec<session::SessionInfo> = serde_json::from_value(v)?;
            for s in list {
                println!(
                    "{} {:<9} {:<12} parent={} turns={} tokens={} verify={} {}",
                    s.id,
                    format!("{:?}", s.state).to_lowercase(),
                    s.name,
                    s.parent.map_or("-".into(), |p| p.to_string()),
                    s.counters.turns,
                    s.counters.tokens,
                    s.last_verify_ok.map_or("-".into(), |b| b.to_string()),
                    s.outcome.map(|o| o.to_string()).unwrap_or_default()
                );
            }
        }
        Cmd::Show { id, tail } => print_json(&call(Request::Get { id, tail }).await?),
        Cmd::Tail { id } => follow(&socket, &id).await?,
        Cmd::Send { id, message } => {
            call(Request::Send {
                id,
                body: message,
                from_name: Some("cli".into()),
            })
            .await?;
            println!("sent");
        }
        Cmd::Pause { id } => print_json(&call(Request::Pause { id }).await?),
        Cmd::Resume { id } => print_json(&call(Request::Resume { id }).await?),
        Cmd::Kill { id } => print_json(&call(Request::Kill { id }).await?),
        Cmd::Offload { id } => print_json(&call(Request::Offload { id }).await?),
        Cmd::Cockpit => {
            client::ensure_daemon(&store_root, &socket).await?;
            cockpit::tui::run(&socket).await?;
        }
        Cmd::Reflect { id } => print_json(&call(Request::Reflect { id }).await?),
        Cmd::Diffs => print_json(&call(Request::Diffs).await?),
        Cmd::Accept { branch } => print_json(&call(Request::Accept { branch }).await?),
        Cmd::Reject { branch } => print_json(&call(Request::Reject { branch }).await?),
        Cmd::Log { n } => {
            for l in call(Request::Log { n })
                .await?
                .as_array()
                .into_iter()
                .flatten()
            {
                println!("{}", l.as_str().unwrap_or(""));
            }
        }
        Cmd::Baseline {
            task,
            workspace,
            model,
            max_turns,
            label,
        } => {
            let model = match model {
                Some(m) => parse_model(&m)?,
                None => harness.model.model_ref(),
            };
            let llm = Arc::new(llm::LlmRegistry::from_harness(&harness)?);
            let sandbox: Arc<dyn sandbox::Sandbox> = Arc::from(sandbox::select(
                harness.sandbox.backend,
                harness.sandbox.mode,
            ));
            let workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
            let args = baseline::BaselineArgs {
                task: &task,
                workspace: &workspace,
                model,
                max_turns,
                label: &label,
            };
            let report = baseline::run(&store, &harness, llm, sandbox, args).await?;
            std::fs::create_dir_all("runs")?;
            std::fs::write(
                format!("runs/{label}.json"),
                serde_json::to_vec_pretty(&report)?,
            )?;
            print_json(&serde_json::to_value(&report)?);
        }
        Cmd::Wait {
            id,
            timeout_seconds,
        } => {
            let start = std::time::Instant::now();
            loop {
                let v = call(Request::Get {
                    id: id.clone(),
                    tail: 0,
                })
                .await?;
                let d: session::SessionDetail = serde_json::from_value(v)?;
                if d.info.state != session::SessionState::Running {
                    print_json(&serde_json::to_value(&d.info)?);
                    break;
                }
                if timeout_seconds > 0 && start.elapsed().as_secs() > timeout_seconds {
                    anyhow::bail!("timeout");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
        Cmd::DaemonFg | Cmd::SandboxExec { .. } => {}
    }
    Ok(())
}

/// Print trajectory events as they arrive until Ctrl-C (which detaches only).
async fn follow(socket: &std::path::Path, id: &str) -> anyhow::Result<()> {
    let mut seen = 0usize;
    let mut printed_state = None;
    loop {
        let req = Request::Get {
            id: id.to_string(),
            tail: 10_000,
        };
        let v = tokio::select! {
            r = client::call(socket, &req) => r?,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\ndetached; session {id} keeps running in longed (longe tail {id} to reattach)");
                return Ok(());
            }
        };
        let d: session::SessionDetail = serde_json::from_value(v)?;
        for e in d.tail.iter().skip(seen) {
            println!("{}", render_event(e));
        }
        seen = d.tail.len();
        if printed_state != Some(d.info.state) {
            printed_state = Some(d.info.state);
            eprintln!(
                "[{}] state={:?} {}",
                d.info.id,
                d.info.state,
                d.info
                    .outcome
                    .as_ref()
                    .map_or(String::new(), |o| o.to_string())
            );
        }
        if d.info.state != session::SessionState::Running {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

fn render_event(e: &Value) -> String {
    let kind = e["kind"].as_str().unwrap_or("?");
    let turn = e["turn"].as_u64().unwrap_or(0);
    match kind {
        "assistant" => format!(
            "--- turn {turn} (assistant, {} tok) ---\n{}",
            e["usage"]["output_tokens"],
            e["text"].as_str().unwrap_or("")
        ),
        "exec" => format!(
            "--- exec result ---\n{}{}",
            e["output"].as_str().unwrap_or(""),
            e["error"]
                .as_str()
                .map(|s| format!("\nerror: {s}"))
                .unwrap_or_default()
        ),
        "note" => format!("[note] {}", e["text"].as_str().unwrap_or("")),
        "message" => format!(
            "[message from {}] {}",
            e["from"].as_str().unwrap_or("?"),
            e["body"].as_str().unwrap_or("")
        ),
        "verify" => format!(
            "[verify] ok={} code={} {}s",
            e["ok"], e["code"], e["seconds"]
        ),
        "done_refused" => format!("[done refused] {}", e["reason"].as_str().unwrap_or("")),
        "compact" => format!(
            "[compacted {} -> {} turns]",
            e["turns_before"], e["turns_after"]
        ),
        "truncated" => format!(
            "[truncated at {} tok ({} reasoning); {}]",
            e["output_tokens"],
            e["reasoning_tokens"],
            e["retried_with"].as_u64().map_or_else(
                || "fed back to the model".to_string(),
                |m| format!("retrying with max_tokens={m}")
            )
        ),
        "finish" => format!("[finish] {}", e["outcome"]),
        "start" => format!("[start] model={}", e["model"]),
        other => format!("[{other}] {}", e),
    }
}

/// Linux helper: confine this process, then exec the command.
fn sandbox_exec(spec: &str, argv: Vec<String>) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let spec: sandbox::linux::HelperSpec = serde_json::from_str(spec)?;
        sandbox::linux::apply(&spec)?;
        let (cmd, args) = argv.split_first().context("missing command")?;
        let err = std::process::Command::new(cmd).args(args).exec();
        anyhow::bail!("exec failed: {err}");
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (spec, argv);
        anyhow::bail!("__sandbox-exec is a Linux helper");
    }
}
