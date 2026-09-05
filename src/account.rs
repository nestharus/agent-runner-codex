use crate::envelope::{HostContext, ProviderFailure};
use std::path::PathBuf;

pub const ACCOUNTS: &[&str] = &["codex", "codex2", "codex3", "codex4", "codex5"];

pub fn user_home(host: &HostContext) -> Result<PathBuf, ProviderFailure> {
    host.env
        .as_ref()
        .and_then(|env| env.get("HOME").cloned())
        .or_else(|| std::env::var("HOME").ok())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| {
            ProviderFailure::invalid_request("", "missing_home", "HOME must be an absolute path")
        })
}

pub fn home(host: &HostContext, settings_id: &str) -> Result<PathBuf, ProviderFailure> {
    let directory = match settings_id {
        "codex" | "codex1" => ".codex".to_string(),
        "codex2" | "codex3" | "codex4" | "codex5" => format!(".{settings_id}"),
        _ => {
            return Err(ProviderFailure::invalid_settings(
                "",
                "unknown_account",
                "Select codex or codex2 through codex5",
                serde_json::json!({}),
            ))
        }
    };
    Ok(user_home(host)?.join(directory))
}
