//! `mem.*`, `skill.*`, `prompt.*`: L3 store CRUD.

use mlua::{Lua, Table};

use super::{ctx, rt_err};
use crate::store::{IndexEntry, StoreError};

fn entries(lua: &Lua, list: Vec<IndexEntry>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (i, e) in list.into_iter().enumerate() {
        let row = lua.create_table()?;
        row.set("name", e.name)?;
        row.set("head", e.head)?;
        row.set("bytes", e.bytes)?;
        t.set(i + 1, row)?;
    }
    Ok(t)
}

fn get_or_nil(r: Result<String, StoreError>) -> mlua::Result<Option<String>> {
    match r {
        Ok(s) => Ok(Some(s)),
        Err(StoreError::NotFound(_)) => Ok(None),
        Err(e) => Err(rt_err(e)),
    }
}

fn collection(lua: &Lua, which: &'static str) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "get",
        lua.create_function(move |lua, k: String| {
            let c = ctx(lua)?;
            get_or_nil(match which {
                "mem" => c.store.mem_get(&k),
                "skill" => c.store.skill_get(&k),
                _ => c.store.subagent_get(&k),
            })
        })?,
    )?;
    t.set(
        "set",
        lua.create_function(move |lua, (k, v): (String, String)| {
            let c = ctx(lua)?;
            match which {
                "mem" => c.store.mem_set(&k, &v),
                "skill" => c.store.skill_set(&k, &v),
                _ => c.store.subagent_set(&k, &v),
            }
            .map_err(rt_err)?;
            Ok(true)
        })?,
    )?;
    t.set(
        "del",
        lua.create_function(move |lua, k: String| {
            let c = ctx(lua)?;
            match which {
                "mem" => c.store.mem_del(&k),
                "skill" => c.store.skill_del(&k),
                _ => c.store.subagent_del(&k),
            }
            .map_err(rt_err)
        })?,
    )?;
    t.set(
        "list",
        lua.create_function(move |lua, ()| {
            let c = ctx(lua)?;
            let l = match which {
                "mem" => c.store.mem_list(),
                "skill" => c.store.skill_list(),
                _ => c.store.subagent_list(),
            }
            .map_err(rt_err)?;
            entries(lua, l)
        })?,
    )?;
    if which == "mem" {
        t.set(
            "search",
            lua.create_function(|lua, q: String| {
                let c = ctx(lua)?;
                let hits = c.store.mem_search(&q).map_err(rt_err)?;
                let t = lua.create_table()?;
                for (i, (key, snippet)) in hits.into_iter().enumerate() {
                    let row = lua.create_table()?;
                    row.set("key", key)?;
                    row.set("snippet", snippet)?;
                    t.set(i + 1, row)?;
                }
                Ok(t)
            })?,
        )?;
    }
    Ok(t)
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set("mem", collection(lua, "mem")?)?;
    lua.globals().set("skill", collection(lua, "skill")?)?;
    lua.globals()
        .set("subagent", collection(lua, "subagent")?)?;
    let prompt = lua.create_table()?;
    prompt.set(
        "get",
        lua.create_function(|lua, ()| ctx(lua)?.store.prompt_get().map_err(rt_err))?,
    )?;
    prompt.set(
        "set",
        lua.create_function(|lua, s: String| {
            ctx(lua)?.store.prompt_set(&s).map_err(rt_err)?;
            Ok(true)
        })?,
    )?;
    lua.globals().set("prompt", prompt)
}

#[cfg(test)]
mod tests {
    use crate::repl::testkit::fixture;

    #[tokio::test(flavor = "multi_thread")]
    async fn store_bindings_persist_on_disk() {
        let f = fixture();
        let run = |code: &str| tokio::task::block_in_place(|| f.repl.exec(code));
        assert_eq!(run("mem.set('lesson', 'clippy is strict')").output, "true");
        assert_eq!(run("mem.get('lesson')").output, "\"clippy is strict\"");
        assert_eq!(run("mem.get('nope')").output, "");
        assert_eq!(run("#mem.search('CLIPPY')").output, "1");
        assert_eq!(run("mem.list()[1].name").output, "\"lesson\"");
        assert_eq!(
            run("skill.set('build', 'cargo build --release')").output,
            "true"
        );
        assert_eq!(
            run("skill.list()[1].head").output,
            "\"cargo build --release\""
        );
        assert_eq!(run("skill.del('build')").output, "true");
        assert_eq!(run("prompt.set('be terse')").output, "true");
        assert_eq!(run("prompt.get()").output, "\"be terse\"");
        assert_eq!(f.store.prompt_get().unwrap(), "be terse");
        assert_eq!(f.store.mem_get("lesson").unwrap(), "clippy is strict");
        let r = run("mem.set('../x', 'y')");
        assert!(r.error.is_some());
    }
}
