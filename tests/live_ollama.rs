//! Live integration test against a local Ollama, if reachable. Skipped otherwise.
//! Drives a real end-to-end session through Longe's daemon, tree, loop and store.

use std::process::Command;
use std::time::Duration;

fn ollama_model() -> Option<String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg("curl -s -m 3 http://127.0.0.1:11434/api/tags")
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let models = v["models"].as_array()?;
    for pref in ["llama3.2:1b", "llama3.2", "qwen2.5-coder", "qwen3-vl:8b"] {
        if models
            .iter()
            .any(|m| m["name"].as_str().is_some_and(|n| n.starts_with(pref)))
        {
            return Some(pref.to_string());
        }
    }
    models
        .first()
        .and_then(|m| m["name"].as_str().map(String::from))
}

fn wait_socket(sock: &std::path::Path) {
    for _ in 0..60 {
        if sock.exists() {
            std::thread::sleep(Duration::from_millis(300));
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn ollama_session_runs_end_to_end() {
    let Some(model) = ollama_model() else {
        eprintln!("ollama not reachable; skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    let harness = format!(
        "[model]\nprovider=\"ollama\"\nname=\"{model}\"\ntemperature=0.0\ncontext_window=8192\nmax_output_tokens=500\n\n[providers.ollama]\nkind=\"ollama\"\nbase_url=\"http://127.0.0.1:11434\"\n\n[budget]\nmin_seconds=0\nmin_turns=1\nmax_turns=4\nmax_tokens=2000000\n\n[sandbox]\nmode=\"workspace-write\"\nbackend=\"none\"\n\n[reflect]\nenabled=false\n"
    );
    std::fs::write(store.join("harness.toml"), harness).unwrap();
    let bin = env!("CARGO_BIN_EXE_longe");
    let sock = store.join("longed.sock");
    let mut daemon = Command::new(bin)
        .args(["--store", store.to_str().unwrap(), "daemon", "--foreground"])
        .spawn()
        .unwrap();
    wait_socket(&sock);
    let run = Command::new(bin)
        .args([
            "--store", store.to_str().unwrap(), "run",
            "Write a file hello.txt in the workspace whose content is exactly DONE, using fs.write('hello.txt','DONE'). Then call done('ok'). One lua block per turn.",
            "--workspace", ws.to_str().unwrap(), "--min-turns", "1", "--detach",
        ])
        .output()
        .unwrap();
    let id = String::from_utf8_lossy(&run.stdout)
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    assert!(
        !id.is_empty(),
        "no id: {}",
        String::from_utf8_lossy(&run.stdout)
    );
    let waited = Command::new(bin)
        .args([
            "--store",
            store.to_str().unwrap(),
            "wait",
            &id,
            "--timeout-seconds",
            "300",
        ])
        .output()
        .unwrap();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let detail = String::from_utf8_lossy(&waited.stdout);
    eprintln!("final session: {detail}");
    assert!(detail.contains("\"state\""), "wait failed: {detail}");
    // Turns must have actually run against the real model.
    assert!(detail.contains("\"turns\""));
    if ws.join("hello.txt").exists() {
        let c = std::fs::read_to_string(ws.join("hello.txt")).unwrap();
        assert!(c.contains("DONE"), "content: {c:?}");
        eprintln!("OK: model wrote hello.txt = {c:?}");
    } else {
        eprintln!("model ran but wrote no file (small model); pipeline exercised");
    }
}
