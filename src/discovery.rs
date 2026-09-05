use serde_json::{json, Value};
pub fn models() -> Value {
    json!({"models": crate::models::catalog(), "warnings": []})
}
pub fn accounts() -> Value {
    json!({"accounts": crate::account::ACCOUNTS.iter().map(|name| json!({"id": name})).collect::<Vec<_>>(), "warnings": []})
}
