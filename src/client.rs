//! Talk to `longed` over its unix socket. Starts it on demand.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::daemon::proto::{Request, Response};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon not reachable at {0}")]
    Unreachable(PathBuf),
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn call(socket: &Path, req: &Request) -> Result<Value, ClientError> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|_| ClientError::Unreachable(socket.to_path_buf()))?;
    let (r, mut w) = stream.into_split();
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    let mut reader = BufReader::new(r);
    let mut resp = String::new();
    reader.read_line(&mut resp).await?;
    if resp.trim().is_empty() {
        return Err(ClientError::Daemon("empty reply".into()));
    }
    match serde_json::from_str::<Response>(&resp)? {
        Response::Ok { data } => Ok(data),
        Response::Error { message } => Err(ClientError::Daemon(message)),
    }
}

pub async fn ping(socket: &Path) -> bool {
    call(socket, &Request::Ping).await.is_ok()
}

/// Start `longe __daemon` detached if nothing answers, then wait for it.
pub async fn ensure_daemon(store_root: &Path, socket: &Path) -> Result<(), ClientError> {
    if ping(socket).await {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::daemon::log_path(store_root))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__daemon")
        .arg("--store")
        .arg(store_root)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    tracing::info!(pid = child.id(), "started longed");
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if ping(socket).await {
            return Ok(());
        }
    }
    Err(ClientError::Unreachable(socket.to_path_buf()))
}
