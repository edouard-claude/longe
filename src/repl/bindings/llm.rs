//! `llm.query` (the RLM leaf) and `model.switch`.

use mlua::{Lua, Table};

use super::{ctx, rt_err};
use crate::config::ModelRef;
use crate::llm::{ChatMessage, ChatRequest};

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let llm = lua.create_table()?;
    llm.set(
        "query",
        lua.create_function(|lua, (prompt, opts): (String, Option<Table>)| {
            let c = ctx(lua)?;
            let model = match opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("model").ok().flatten())
            {
                Some(s) => {
                    let (p, n) = s
                        .split_once('/')
                        .ok_or_else(|| rt_err("model must be \"provider/name\""))?;
                    ModelRef {
                        provider: p.into(),
                        name: n.into(),
                    }
                }
                None => c.model.lock().clone(),
            };
            let system = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("system").ok().flatten())
                .unwrap_or_default();
            let req = ChatRequest {
                model: model.name.clone(),
                system,
                messages: vec![ChatMessage::user(prompt)],
                temperature: c.temperature,
                max_tokens: c.max_output_tokens,
            };
            let resp =
                c.rt.block_on(c.llm.complete(&model, &req))
                    .map_err(rt_err)?;
            c.effects.lock().llm_tokens += resp.usage.total();
            Ok(crate::parse::strip_thinking(&resp.text))
        })?,
    )?;
    lua.globals().set("llm", llm)?;

    let model = lua.create_table()?;
    model.set(
        "switch",
        lua.create_function(|lua, (provider, name): (String, String)| {
            let c = ctx(lua)?;
            if !c.llm.has_provider(&provider) {
                return Err(rt_err(format!(
                    "unknown provider `{provider}`; known: {}",
                    c.llm.provider_names().join(", ")
                )));
            }
            let m = ModelRef { provider, name };
            *c.model.lock() = m.clone();
            c.effects.lock().model_switch = Some(m.clone());
            Ok(format!("model switched to {m}"))
        })?,
    )?;
    model.set(
        "current",
        lua.create_function(|lua, ()| Ok(ctx(lua)?.model.lock().to_string()))?,
    )?;
    lua.globals().set("model", model)
}

#[cfg(test)]
mod tests {
    use crate::repl::testkit::fixture;

    #[tokio::test(flavor = "multi_thread")]
    async fn switch_validates_provider_and_records_effect() {
        let f = fixture();
        let run = |code: &str| tokio::task::block_in_place(|| f.repl.exec(code));
        let r = run("model.switch('nope', 'x')");
        assert!(r.error.as_deref().unwrap().contains("unknown provider"));
        let r = run("model.switch('deepseek', 'deepseek-chat')");
        assert_eq!(r.output, "\"model switched to deepseek/deepseek-chat\"");
        assert_eq!(run("model.current()").output, "\"deepseek/deepseek-chat\"");
        let eff = f.repl.take_effects();
        assert_eq!(eff.model_switch.unwrap().name, "deepseek-chat");
    }
}
