//! Native Lua bindings. Each namespace is a table of Rust functions; every function
//! fetches the [`ReplCtx`] from Lua app data and never blocks the runtime: async work
//! goes through `ctx.rt.block_on` (the loop runs `exec` inside `block_in_place`).

mod agent;
mod control;
mod fs;
mod llm;
mod sh;
mod store;

use mlua::{AppDataRef, Lua};

use super::ReplCtx;

pub(crate) fn ctx(lua: &Lua) -> mlua::Result<AppDataRef<'_, ReplCtx>> {
    lua.app_data_ref::<ReplCtx>()
        .ok_or_else(|| mlua::Error::runtime("repl context missing"))
}

pub(crate) fn rt_err(e: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(e.to_string())
}

/// Workspace-confined path resolution, shared with the baseline harness.
pub fn fs_resolve(ws: &std::path::Path, p: &str) -> Result<std::path::PathBuf, String> {
    fs::resolve(ws, p)
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let out_print = lua.create_function(|lua, args: mlua::MultiValue| {
        let c = ctx(lua)?;
        let mut parts = Vec::with_capacity(args.len());
        for v in args {
            parts.push(match v {
                mlua::Value::String(s) => s.to_str()?.to_string(),
                other => lua
                    .globals()
                    .get::<mlua::Function>("tostring")?
                    .call::<String>(other)?,
            });
        }
        let mut out = c.out.lock();
        out.push_str(&parts.join("\t"));
        out.push('\n');
        Ok(())
    })?;
    lua.globals().set("print", out_print)?;
    fs::install(lua)?;
    sh::install(lua)?;
    store::install(lua)?;
    agent::install(lua)?;
    llm::install(lua)?;
    control::install(lua)?;
    Ok(())
}

/// The binding reference shown to the model.
pub const REFERENCE: &str = r#"fs.read(path) -> string | fs.write(path, text) | fs.list(dir) -> {names} (dirs end with /) | fs.rm(path)   [paths relative to the workspace, confined to it]
sh(cmd, {timeout=seconds}) -> {stdout=, stderr=, code=, timed_out=}   [sandboxed shell in the workspace; the store is invisible; no network unless configured]
mem.get(key) -> text|nil | mem.set(key, text) | mem.del(key) | mem.list() -> {{name=,head=,bytes=}} | mem.search(q) -> {{key=,snippet=}}
skill.get(name) | skill.set(name, text) | skill.del(name) | skill.list()
subagent.get(name) | subagent.set(name, text) | subagent.del(name) | subagent.list()   [reusable sub-agent task specs]
prompt.get() -> text | prompt.set(text)   [your own system prompt, persisted]
agent.spawn(name, task, {model="provider/name", budget={min_seconds=,min_turns=,max_turns=,max_tokens=}, workspace="subdir" or "."}) -> id
agent.id() -> your own id | agent.send(id, text) | agent.recv() -> {{from=,from_name=,kind=,body=,ts=}} | agent.list() -> {{id=,name=,state=,parent=,outcome=}} | agent.pause(id) | agent.resume(id) | agent.kill(id)
llm.query(prompt, {model="provider/name", system=text}) -> text   [stateless call, no history]
model.switch(provider, name)   [switch the model driving this session; state is kept]
compact(hint) | verify() -> {ok=, report=} | note(text) | done(summary)
_last holds the full output of the previous exec when it was truncated."#;
