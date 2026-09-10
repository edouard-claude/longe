//! `agent.*`: persistent sub-agents and the message bus, through the tree actor.

use std::path::PathBuf;

use mlua::{Lua, Table};

use super::{ctx, rt_err};
use crate::config::{BudgetCfg, ModelRef};
use crate::session::{Message, SessionId, SpawnSpec};

fn parse_model(v: mlua::Value) -> mlua::Result<Option<ModelRef>> {
    match v {
        mlua::Value::Nil => Ok(None),
        mlua::Value::String(s) => {
            let s = s.to_str()?;
            let (p, n) = s
                .split_once('/')
                .ok_or_else(|| rt_err("model must be \"provider/name\""))?;
            Ok(Some(ModelRef {
                provider: p.to_string(),
                name: n.to_string(),
            }))
        }
        mlua::Value::Table(t) => Ok(Some(ModelRef {
            provider: t.get("provider")?,
            name: t.get("name")?,
        })),
        _ => Err(rt_err("model must be a string or a table")),
    }
}

fn parse_budget(base: BudgetCfg, v: Option<Table>) -> mlua::Result<Option<BudgetCfg>> {
    let Some(t) = v else { return Ok(None) };
    Ok(Some(BudgetCfg {
        min_seconds: t
            .get::<Option<u64>>("min_seconds")?
            .unwrap_or(base.min_seconds),
        min_turns: t.get::<Option<u32>>("min_turns")?.unwrap_or(base.min_turns),
        max_turns: t.get::<Option<u32>>("max_turns")?.unwrap_or(base.max_turns),
        max_tokens: t
            .get::<Option<u64>>("max_tokens")?
            .unwrap_or(base.max_tokens),
    }))
}

fn sid(s: &str) -> mlua::Result<SessionId> {
    SessionId::parse(s).map_err(rt_err)
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set(
        "id",
        lua.create_function(|lua, ()| Ok(ctx(lua)?.session_id.to_string()))?,
    )?;
    t.set(
        "spawn",
        lua.create_function(|lua, (name, task, opts): (String, String, Option<Table>)| {
            let c = ctx(lua)?;
            crate::store::safe_name(&name).map_err(rt_err)?;
            let model = parse_model(
                opts.as_ref()
                    .map_or(Ok(mlua::Value::Nil), |o| o.get("model"))?,
            )?;
            let budget = parse_budget(
                c.session_budget,
                opts.as_ref().map_or(Ok(None), |o| o.get("budget"))?,
            )?;
            let ws_opt: Option<String> = opts.as_ref().map_or(Ok(None), |o| o.get("workspace"))?;
            let workspace: PathBuf = match ws_opt.as_deref() {
                None => c.workspace.join("agents").join(&name),
                Some(".") => c.workspace.clone(),
                Some(rel) => super::fs::resolve(&c.workspace, rel).map_err(rt_err)?,
            };
            std::fs::create_dir_all(&workspace).map_err(rt_err)?;
            let spec = SpawnSpec {
                name,
                task,
                workspace,
                parent: Some(c.session_id.clone()),
                model,
                budget,
            };
            let id = c.rt.block_on(c.tree.spawn(spec)).map_err(rt_err)?;
            c.effects.lock().spawned += 1;
            Ok(id.to_string())
        })?,
    )?;
    t.set(
        "send",
        lua.create_function(|lua, (id, body): (String, String)| {
            let c = ctx(lua)?;
            let msg = Message::text(
                Some(c.session_id.clone()),
                c.session_name.clone(),
                sid(&id)?,
                body,
            );
            c.rt.block_on(c.tree.send(msg)).map_err(rt_err)?;
            c.effects.lock().sent += 1;
            Ok(true)
        })?,
    )?;
    t.set(
        "recv",
        lua.create_function(|lua, ()| {
            let c = ctx(lua)?;
            let msgs =
                c.rt.block_on(c.tree.drain(c.session_id.clone()))
                    .map_err(rt_err)?;
            let t = lua.create_table()?;
            for (i, m) in msgs.into_iter().enumerate() {
                let row = lua.create_table()?;
                row.set("from", m.from.map(|f| f.to_string()))?;
                row.set("from_name", m.from_name)?;
                row.set("kind", format!("{:?}", m.kind).to_lowercase())?;
                row.set("body", m.body)?;
                row.set("ts", m.ts)?;
                t.set(i + 1, row)?;
            }
            Ok(t)
        })?,
    )?;
    t.set(
        "list",
        lua.create_function(|lua, ()| {
            let c = ctx(lua)?;
            let list = c.rt.block_on(c.tree.list()).map_err(rt_err)?;
            let t = lua.create_table()?;
            for (i, s) in list.into_iter().enumerate() {
                let row = lua.create_table()?;
                row.set("id", s.id.to_string())?;
                row.set("name", s.name)?;
                row.set("state", format!("{:?}", s.state).to_lowercase())?;
                row.set("parent", s.parent.map(|p| p.to_string()))?;
                row.set("outcome", s.outcome.map(|o| o.to_string()))?;
                row.set("turns", s.counters.turns)?;
                row.set("last_note", s.last_note)?;
                t.set(i + 1, row)?;
            }
            Ok(t)
        })?,
    )?;
    for (name, which) in [("pause", 0u8), ("resume", 1), ("kill", 2)] {
        t.set(
            name,
            lua.create_function(move |lua, id: String| {
                let c = ctx(lua)?;
                let id = sid(&id)?;
                let r = match which {
                    0 => c.rt.block_on(c.tree.pause(id)),
                    1 => c.rt.block_on(c.tree.resume(id)),
                    _ => c.rt.block_on(c.tree.kill(id)),
                };
                r.map_err(rt_err)?;
                Ok(true)
            })?,
        )?;
    }
    lua.globals().set("agent", t)
}
