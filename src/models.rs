use serde_json::{json, Value};
pub const ASTRA: &str = "gpt-6-astra";
pub const LUNA: &str = "gpt-5.6-luna";
pub const TERRA: &str = "gpt-5.6-terra";
pub const SOL: &str = "gpt-5.6-sol";
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const BENCH: &str = "codex-exec-bench";
const FAMILIES: &[(&str, &str)] = &[
    ("gpt-", ASTRA),
    ("codex-gpt-", ASTRA),
    ("gpt-luna-", LUNA),
    ("gpt-terra-", TERRA),
    ("gpt-sol-", SOL),
];

pub fn route(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        BENCH => Some((LUNA, "low")),
        // Standard execution tiers are aliases, not native effort names.
        "gpt-high" | "gpt-xhigh" | "gpt-max" => Some((ASTRA, "medium")),
        _ => FAMILIES
            .iter()
            .find_map(|(prefix, model)| native_route(name, prefix, model)),
    }
}

fn native_route(
    name: &str,
    prefix: &str,
    model: &'static str,
) -> Option<(&'static str, &'static str)> {
    let effort = name.strip_prefix(prefix)?;
    EFFORTS
        .iter()
        .find(|value| **value == effort)
        .map(|value| (model, *value))
}

pub fn args(model: &str, effort: &str) -> Vec<String> {
    vec![
        "-m".into(),
        model.into(),
        "-c".into(),
        format!("model_reasoning_effort=\"{effort}\""),
    ]
}

fn catalog_entry(name: String) -> Value {
    let (model, effort) = route(&name).expect("catalog names must be supported routes");
    json!({"name": name, "provider_model": model,
        "provider_args": args(model, effort), "eligible_accounts": crate::account::ACCOUNTS})
}

pub fn catalog() -> Vec<Value> {
    FAMILIES
        .iter()
        .flat_map(|(prefix, _)| {
            EFFORTS
                .iter()
                .map(move |effort| format!("{prefix}{effort}"))
        })
        .map(catalog_entry)
        .collect()
}
