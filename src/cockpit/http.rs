//! HTTP JSON on `127.0.0.1:port` (expose it with tailscale if you want it remote).
//!
//! ```text
//! GET  /sessions                    POST /sessions            {name, task, workspace, model?, budget?}
//! GET  /sessions/{id}?tail=20       POST /sessions/{id}/message   {body, from_name?}
//! POST /sessions/{id}/pause|resume|kill|offload
//! POST /reflect/{id}                GET  /store/diffs         GET /store/log?n=20
//! POST /store/diffs/{branch}/accept|reject                   POST /shutdown
//! ```

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use crate::daemon::proto::{Request, Response};
use crate::daemon::Api;

type Reply = (StatusCode, Json<Value>);

fn reply(r: Response) -> Reply {
    match r {
        Response::Ok { data } => (StatusCode::OK, Json(data)),
        Response::Error { message } => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": message})),
        ),
    }
}

#[derive(Deserialize)]
struct TailQ {
    tail: Option<usize>,
}

#[derive(Deserialize)]
struct NQ {
    n: Option<usize>,
}

#[derive(Deserialize)]
struct SpawnBody {
    name: Option<String>,
    task: String,
    workspace: std::path::PathBuf,
    model: Option<crate::config::ModelRef>,
    budget: Option<crate::config::BudgetCfg>,
}

#[derive(Deserialize)]
struct MessageBody {
    body: String,
    from_name: Option<String>,
}

pub fn router(api: Api) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Json(serde_json::json!({"longe": env!("CARGO_PKG_VERSION")})) }),
        )
        .route("/sessions", get(list).post(spawn))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/message", post(send))
        .route("/sessions/{id}/pause", post(|s, p| simple(s, p, 0)))
        .route("/sessions/{id}/resume", post(|s, p| simple(s, p, 1)))
        .route("/sessions/{id}/kill", post(|s, p| simple(s, p, 2)))
        .route("/sessions/{id}/offload", post(|s, p| simple(s, p, 3)))
        .route("/reflect/{id}", post(|s, p| simple(s, p, 4)))
        .route("/store/diffs", get(diffs))
        .route("/store/diffs/{branch}/accept", post(accept))
        .route("/store/diffs/{branch}/reject", post(reject))
        .route("/store/log", get(log))
        .route("/shutdown", post(shutdown))
        .with_state(api)
}

async fn list(State(api): State<Api>) -> Reply {
    reply(api.call(Request::List).await)
}

async fn spawn(State(api): State<Api>, Json(b): Json<SpawnBody>) -> Reply {
    reply(
        api.call(Request::Spawn {
            name: b.name.unwrap_or_else(|| "root".into()),
            task: b.task,
            workspace: b.workspace,
            model: b.model,
            budget: b.budget,
        })
        .await,
    )
}

async fn get_session(
    State(api): State<Api>,
    Path(id): Path<String>,
    Query(q): Query<TailQ>,
) -> Reply {
    reply(
        api.call(Request::Get {
            id,
            tail: q.tail.unwrap_or(20),
        })
        .await,
    )
}

async fn send(State(api): State<Api>, Path(id): Path<String>, Json(b): Json<MessageBody>) -> Reply {
    reply(
        api.call(Request::Send {
            id,
            body: b.body,
            from_name: b.from_name,
        })
        .await,
    )
}

async fn simple(State(api): State<Api>, Path(id): Path<String>, which: u8) -> Reply {
    let req = match which {
        0 => Request::Pause { id },
        1 => Request::Resume { id },
        2 => Request::Kill { id },
        3 => Request::Offload { id },
        _ => Request::Reflect { id },
    };
    reply(api.call(req).await)
}

async fn diffs(State(api): State<Api>) -> Reply {
    reply(api.call(Request::Diffs).await)
}

async fn accept(State(api): State<Api>, Path(branch): Path<String>) -> Reply {
    reply(api.call(Request::Accept { branch }).await)
}

async fn reject(State(api): State<Api>, Path(branch): Path<String>) -> Reply {
    reply(api.call(Request::Reject { branch }).await)
}

async fn log(State(api): State<Api>, Query(q): Query<NQ>) -> Reply {
    reply(
        api.call(Request::Log {
            n: q.n.unwrap_or(20),
        })
        .await,
    )
}

async fn shutdown(State(api): State<Api>) -> Reply {
    reply(api.call(Request::Shutdown).await)
}
