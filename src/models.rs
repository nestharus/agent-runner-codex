use serde_json::{json, Value};
pub const ASTRA: &str = "gpt-6-astra";
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const BENCH: &str = "codex-exec-bench";

pub fn route(name: &str) -> Option<(&'static str, &'static str)> {
    if name == BENCH {
        return Some(("gpt-5.6-luna", "low"));
    }
    let effort = name.strip_prefix("codex-gpt-")?;
    EFFORTS
        .iter()
        .find(|value| **value == effort)
        .map(|value| (ASTRA, *value))
}

pub fn args(model: &str, effort: &str) -> Vec<String> {
    vec![
        "-m".into(),
        model.into(),
        "-c".into(),
        format!("model_reasoning_effort=\"{effort}\""),
    ]
}

pub fn catalog() -> Vec<Value> {
    EFFORTS
        .iter()
        .map(|effort| {
            json!({"name": format!("codex-gpt-{effort}"), "provider_model": ASTRA,
               "provider_args": args(ASTRA, effort), "eligible_accounts": crate::account::ACCOUNTS})
        })
        .collect()
}
