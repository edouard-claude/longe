//! `compact`, `verify`, `note`, `done`: requests to the loop.

use mlua::Lua;

use super::{ctx, rt_err};

pub fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "compact",
        lua.create_function(|lua, hint: Option<String>| {
            ctx(lua)?.effects.lock().compact = Some(hint.unwrap_or_default());
            Ok("compaction scheduled after this exec")
        })?,
    )?;
    lua.globals().set(
        "verify",
        lua.create_function(|lua, ()| {
            let c = ctx(lua)?;
            let outcome =
                crate::verify::run(&c.verify_cfg, c.sandbox.as_ref(), &c.policy, &c.evals_dir)
                    .map_err(rt_err)?;
            let t = lua.create_table()?;
            t.set("ok", outcome.ok)?;
            t.set("report", outcome.report.clone())?;
            t.set("code", outcome.code)?;
            c.effects.lock().verify = Some(outcome);
            Ok(t)
        })?,
    )?;
    lua.globals().set(
        "note",
        lua.create_function(|lua, s: String| {
            ctx(lua)?.effects.lock().notes.push(s);
            Ok(true)
        })?,
    )?;
    lua.globals().set(
        "done",
        lua.create_function(|lua, summary: Option<String>| {
            ctx(lua)?.effects.lock().done = Some(summary.unwrap_or_default());
            Ok("done requested: the runtime will check the budget and run the verifier")
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::repl::testkit::fixture;

    #[tokio::test(flavor = "multi_thread")]
    async fn control_bindings_record_effects() {
        let f = fixture();
        let run = |code: &str| tokio::task::block_in_place(|| f.repl.exec(code));
        let r = run("local v = verify(); return v.ok, v.code");
        assert_eq!(r.output, "false\t1");
        run("fs.write('ok.txt', '')");
        assert_eq!(run("verify().ok").output, "true");
        run("note('half way'); compact('keep the parser design'); done('all green')");
        let eff = f.repl.take_effects();
        assert_eq!(eff.notes, vec!["half way".to_string()]);
        assert_eq!(eff.compact.as_deref(), Some("keep the parser design"));
        assert_eq!(eff.done.as_deref(), Some("all green"));
        assert!(eff.verify.unwrap().ok);
        assert!(f.repl.take_effects().done.is_none());
    }
}
