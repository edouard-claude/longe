//! `sh(cmd, {timeout})`: the sandboxed shell.

use std::time::Duration;

use mlua::{Lua, Table};

use super::{ctx, rt_err};

pub fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "sh",
        lua.create_function(|lua, (cmd, opts): (String, Option<Table>)| {
            let c = ctx(lua)?;
            let mut policy = c.policy.clone();
            if let Some(t) = opts
                .as_ref()
                .and_then(|o| o.get::<Option<f64>>("timeout").ok().flatten())
            {
                if t > 0.0 {
                    policy.timeout = Duration::from_secs_f64(t);
                }
            }
            let out = c.sandbox.run(&policy, &cmd).map_err(rt_err)?;
            let t = lua.create_table()?;
            t.set("stdout", out.stdout)?;
            t.set("stderr", out.stderr)?;
            t.set("code", out.code)?;
            t.set("timed_out", out.timed_out)?;
            Ok(t)
        })?,
    )
}

#[cfg(test)]
mod tests {
    use crate::repl::testkit::fixture;

    #[tokio::test(flavor = "multi_thread")]
    async fn sh_runs_in_workspace_with_timeout() {
        let f = fixture();
        let run = |code: &str| tokio::task::block_in_place(|| f.repl.exec(code));
        let r = run(
            "local r = sh('echo hi; echo oops 1>&2; exit 2'); return r.stdout, r.stderr, r.code",
        );
        assert_eq!(r.output, "\"hi\\\n\"\t\"oops\\\n\"\t2");
        let r = run("sh('pwd').stdout");
        let ws = std::fs::canonicalize(&f.workspace).unwrap();
        assert!(
            r.output.contains(&ws.to_string_lossy().to_string()),
            "{r:?}"
        );
        let r = run("sh('sleep 3', {timeout = 0.3}).timed_out");
        assert_eq!(r.output, "true");
        let r = run("sh('echo ${LONGE_SANDBOX}').stdout");
        assert!(r.output.contains("workspacewrite"), "{r:?}");
    }
}
