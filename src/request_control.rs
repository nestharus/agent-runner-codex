//! Opt-in, provider-owned Linux MCP request-control launch binding and CLI.
use crate::envelope::{HostContext, ProviderFailure, RequestEnvelope};
use std::collections::BTreeMap;

pub const DIRECTORY_ENV: &str = "AGENT_RUNNER_CODEX_REQUEST_CONTROL_DIR";
pub const BINDING_ENV: &str = "AGENT_RUNNER_CODEX_REQUEST_CONTROL_BINDING";

pub(crate) fn bind(
    request: &RequestEnvelope,
    env: &mut BTreeMap<String, String>,
    data_root: &std::path::Path,
) -> Result<(), ProviderFailure> {
    // This is an output of this launch, never an inherited launch capability.
    env.remove(BINDING_ENV);
    if !env.contains_key(DIRECTORY_ENV) {
        return Ok(());
    }
    let invalid = || {
        ProviderFailure::invalid_request(
            &request.request_id,
            "request_control_unavailable",
            "Request control requires Linux and a canonical runner invocation binding",
        )
    };
    if !cfg!(target_os = "linux") {
        return Err(invalid());
    }
    let parent: serde_json::Value = env
        .get("OULIPOLY_PARENT_INVOCATION")
        .and_then(|value| serde_json::from_str(value).ok())
        .ok_or_else(invalid)?;
    if parent.as_object().is_none_or(|map| map.len() != 2)
        || !parent["id"]
            .as_str()
            .is_some_and(crate::launch::valid_thread_id)
        || !parent["source"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= 256)
    {
        return Err(invalid());
    }
    let pid = std::process::id();
    let incarnation =
        crate::native_process::process_group_incarnation(pid).map_err(|_| invalid())?;
    env.insert(
        BINDING_ENV.into(),
        serde_json::json!({"version":1,"provider_pid":pid,"provider_incarnation":incarnation,
            "request_id":request.request_id,"provider_instance_id":request.provider_instance_id,
            "parent":parent,"identity_database":env.get("OULIPOLY_DATA_DIR")
                .map(std::path::PathBuf::from).unwrap_or_else(|| data_root.to_owned()).join("pid-identity.db")})
        .to_string(),
    );
    Ok(())
}

pub fn run(args: &[String]) -> i32 {
    match command(args) {
        Ok(mut command) => {
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::process::CommandExt;
                let _ = command.exec();
            }
            #[cfg(not(target_os = "linux"))]
            let _ = &mut command;
        }
        Err(message) => eprintln!("{message}"),
    }
    2
}

fn command(args: &[String]) -> Result<std::process::Command, &'static str> {
    if !cfg!(target_os = "linux") {
        return Err("Request control requires Linux Unix sockets and peer credentials");
    }
    let mut host: HostContext =
        serde_json::from_value(serde_json::json!({"app":"request-control"}))
            .map_err(|_| "Cannot initialize request-control configuration")?;
    let args = if args.first().map(String::as_str) == Some("--config-root") {
        host.config_root = Some(args.get(1).ok_or("Missing configuration root")?.clone());
        &args[2..]
    } else {
        args
    };
    let config = crate::policy::RuntimeConfig::load(&host)
        .map_err(|_| "Cannot load request-control runtime configuration")?;
    let script = config
        .bash_mcp_path
        .with_file_name("request-control-cli.ts");
    if !script.is_file() || !config.bun_bin.is_file() {
        return Err("Request-control runtime is not installed");
    }
    let mut command = std::process::Command::new(config.bun_bin);
    command.arg("--no-install").arg(script).args(args);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> RequestEnvelope {
        serde_json::from_value(json!({
            "contract":"oulipoly.provider/v1", "request_id":"outer", "provider_instance_id":"codex",
            "host":{"app":"fixture"}, "params":{}
        }))
        .unwrap()
    }

    #[test]
    fn unused_control_removes_inherited_binding_without_materializing_state() {
        let root = tempfile::tempdir().unwrap();
        let mut env = BTreeMap::from([(BINDING_ENV.into(), "stale launch".into())]);
        bind(&request(), &mut env, root.path()).unwrap();
        assert!(env.is_empty());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn selected_control_rejects_missing_or_noncanonical_parent() {
        let root = tempfile::tempdir().unwrap();
        for parent in ["null", "{}", r#"{"id":"not-uuid","source":"fixture"}"#] {
            let mut env = BTreeMap::from([
                (
                    DIRECTORY_ENV.into(),
                    root.path().join("slot").display().to_string(),
                ),
                ("OULIPOLY_PARENT_INVOCATION".into(), parent.into()),
            ]);
            assert!(bind(&request(), &mut env, root.path()).is_err());
            assert!(!env.contains_key(BINDING_ENV));
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }
}
