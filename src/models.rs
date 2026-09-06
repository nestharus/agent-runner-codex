use serde_json::{json, Value};
pub const ASTRA: &str = "gpt-6-astra";
pub const LUNA: &str = "gpt-5.6-luna";
pub const TERRA: &str = "gpt-5.6-terra";
pub const SOL: &str = "gpt-5.6-sol";
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const BENCH: &str = "codex-exec-bench";

pub fn route(name: &str) -> Option<(&'static str, &'static str)> {
    if name == BENCH {
        return Some((LUNA, "low"));
    }
    for (prefix, model) in [
        ("gpt-luna-", LUNA),
        ("gpt-terra-", TERRA),
        ("gpt-sol-", SOL),
    ] {
        if let Some(effort) = name.strip_prefix(prefix) {
            return EFFORTS
                .iter()
                .find(|value| **value == effort)
                .map(|value| (model, *value));
        }
    }
    let effort = name
        .strip_prefix("codex-gpt-")
        .or_else(|| name.strip_prefix("gpt-"))?;
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
    [
        ("gpt-", ASTRA, EFFORTS),
        ("codex-gpt-", ASTRA, EFFORTS),
        ("gpt-luna-", LUNA, EFFORTS),
        ("gpt-terra-", TERRA, EFFORTS),
        ("gpt-sol-", SOL, EFFORTS),
    ]
        .into_iter()
        .flat_map(|(prefix, model, efforts)| {
            efforts.iter().map(move |effort| {
                json!({"name": format!("{prefix}{effort}"), "provider_model": model,
                   "provider_args": args(model, effort), "eligible_accounts": crate::account::ACCOUNTS})
            })
        })
        .collect()
}
