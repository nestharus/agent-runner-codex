//! Bounded, no-session/no-model native stdio configuration read before TUI exec.
//! This is a compatibility snapshot, not an exclusive hook inventory or a lock
//! against policy changes between this read and native TUI configuration load.
//! Native app-server startup initializes its configured SQLite store even for
//! config-only requests; this process is NOT a read-only datastore probe.
use crate::{envelope::ProviderFailure, native_process, policy::RuntimeConfig};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn failure() -> ProviderFailure {
    ProviderFailure::invalid_settings("", "registration_native_policy_incompatible",
        "Native Codex configuration could not establish registration compatibility (hooks must be enabled, ordinary trusted hooks permitted, and managed feature requirements compatible). Resolve the native policy with its administrator; the provider will not override managed requirements.", json!({}))
}

struct Probe(Child);
impl Drop for Probe {
    fn drop(&mut self) {
        native_process::terminate_process_group_child(&mut self.0);
    }
}

#[cfg(unix)]
pub(crate) fn check(
    config: &RuntimeConfig,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    native_args: &[String],
) -> Result<(), ProviderFailure> {
    use std::os::fd::AsRawFd;
    let mut command = Command::new(&config.codex_bin);
    command
        .args(["app-server", "--listen", "stdio://"])
        .envs(env)
        // Pinned CLI consumes this native ephemeral marker before worker startup.
        // Never let persisted remote-control settings accept work during a probe.
        .env("CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED", "1")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut expected_features = BTreeMap::new();
    for pair in native_args.windows(2).filter(|p| p[0] == "-c") {
        command.args(pair);
        if let Some((key, value)) = pair[1]
            .strip_prefix("features.")
            .and_then(|s| s.split_once('='))
        {
            expected_features.insert(key.to_owned(), value == "true");
        }
    }
    native_process::configure_process_group(&mut command);
    let mut probe = Probe(command.spawn().map_err(|_| failure())?);
    let mut input = probe.0.stdin.take().ok_or_else(failure)?;
    let mut output = probe.0.stdout.take().ok_or_else(failure)?;
    let fd = output.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(failure());
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut buffer = Vec::new();
    let mut total = 0usize;
    send(
        &mut input,
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"agent-runner-registration-preflight","version":"1"}}}),
    )?;
    receive(&mut output, &mut buffer, &mut total, deadline, 1)?;
    send(&mut input, json!({"method":"initialized"}))?;
    send(
        &mut input,
        json!({"id":2,"method":"config/read","params":{"includeLayers":false,"cwd":cwd}}),
    )?;
    let effective = receive(&mut output, &mut buffer, &mut total, deadline, 2)?;
    send(
        &mut input,
        json!({"id":3,"method":"configRequirements/read","params":null}),
    )?;
    let requirements = receive(&mut output, &mut buffer, &mut total, deadline, 3)?;
    validate(&effective, &requirements, &expected_features)?;
    validate_owned_settings(&effective, &requirements, config, env)
}

#[cfg(not(unix))]
pub(crate) fn check(
    _: &RuntimeConfig,
    _: &BTreeMap<String, String>,
    _: &Path,
    _: &[String],
) -> Result<(), ProviderFailure> {
    Err(failure())
}

fn validate_owned_settings(
    effective: &Value,
    requirements: &Value,
    config: &RuntimeConfig,
    env: &BTreeMap<String, String>,
) -> Result<(), ProviderFailure> {
    let expected = [
        (
            "model_instructions_file",
            None,
            json!(config.system_prompt_file),
        ),
        (
            "model_catalog_json",
            Some("modelCatalogJson"),
            json!(config.bash_mcp_path.parent().unwrap().join("models.json")),
        ),
        (
            "sqlite_home",
            Some("sqliteHome"),
            json!(env.get("CODEX_SQLITE_HOME").ok_or_else(failure)?),
        ),
        (
            "cli_auth_credentials_store",
            Some("cliAuthCredentialsStore"),
            json!("file"),
        ),
    ];
    for (name, requirement, wanted) in expected {
        if effective.get("config").and_then(|v| v.get(name)) != Some(&wanted) {
            return Err(failure());
        }
        if requirement
            .and_then(|key| requirements["requirements"].get(key))
            .is_some_and(|v| !v.is_null() && *v != wanted)
        {
            return Err(failure());
        }
    }
    Ok(())
}

fn send(input: &mut impl Write, value: Value) -> Result<(), ProviderFailure> {
    writeln!(input, "{value}").map_err(|_| failure())
}

fn receive(
    output: &mut impl Read,
    buffer: &mut Vec<u8>,
    total: &mut usize,
    deadline: Instant,
    id: u64,
) -> Result<Value, ProviderFailure> {
    loop {
        if Instant::now() >= deadline {
            return Err(failure());
        }
        if let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = buffer.drain(..=end).collect();
            let value: Value = serde_json::from_slice(&line).map_err(|_| failure())?;
            if value["id"] == id {
                return value
                    .get("result")
                    .cloned()
                    .filter(|_| value.get("error").is_none())
                    .ok_or_else(failure);
            }
            continue;
        }
        let mut bytes = [0; 8192];
        match output.read(&mut bytes) {
            Ok(0) => return Err(failure()),
            Ok(n) => {
                *total += n;
                if *total > 4 * 1024 * 1024 {
                    return Err(failure());
                }
                buffer.extend_from_slice(&bytes[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => (),
            Err(_) => return Err(failure()),
        }
    }
}

fn validate(
    effective: &Value,
    response: &Value,
    expected: &BTreeMap<String, bool>,
) -> Result<(), ProviderFailure> {
    if expected.get("hooks") != Some(&true) {
        return Err(failure());
    }
    let requirements = response.get("requirements").ok_or_else(failure)?;
    if !requirements.is_null() && !requirements.is_object() {
        return Err(failure());
    }
    if requirements
        .get("allowManagedHooksOnly")
        .is_some_and(|v| !v.is_null() && v != false)
    {
        return Err(failure());
    }
    if requirements.get("featureRequirements").is_some_and(|v| {
        !v.is_null()
            && !v
                .as_object()
                .is_some_and(|o| o.values().all(Value::is_boolean))
    }) {
        return Err(failure());
    }
    let features = effective
        .pointer("/config/features")
        .and_then(Value::as_object)
        .ok_or_else(failure)?;
    for (key, wanted) in expected {
        let actual = features.get(key).and_then(|v| {
            v.as_bool()
                .or_else(|| v.get("enabled").and_then(Value::as_bool))
        });
        if actual != Some(*wanted) {
            return Err(failure());
        }
        if let Some(required) = requirements
            .get("featureRequirements")
            .and_then(|f| f.get(key))
        {
            if required.as_bool() != Some(*wanted) {
                return Err(failure());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_managed_incompatibility_rejects_without_overrides() {
        let expected = BTreeMap::from([("hooks".into(), true), ("shell_tool".into(), false)]);
        let config = json!({"config":{"features":{"hooks":true,"shell_tool":false}}});
        assert!(validate(&config, &json!({"requirements":null}), &expected).is_ok());
        for response in [
            json!({}),
            json!({"requirements":false}),
            json!({"requirements":{"featureRequirements":"malformed"}}),
            json!({"requirements":{"featureRequirements":{"hooks":"malformed"}}}),
            json!({"requirements":{"allowManagedHooksOnly":true}}),
            json!({"requirements":{"featureRequirements":{"hooks":false}}}),
            json!({"requirements":{"featureRequirements":{"shell_tool":true}}}),
        ] {
            assert!(validate(&config, &response, &expected).is_err());
        }
        assert!(validate(
            &json!({"config":{"features":{"hooks":false,"shell_tool":false}}}),
            &json!({"requirements":null}),
            &expected
        )
        .is_err());
    }
}
