//! Resident Codex sessions (`oulipoly.resident_session/v1`).
//!
//! `resident.prepare` admits the host's policy-evaluated launch template,
//! checks it the way a launch would (route, managed argv, account, runtime
//! configuration) and durably records it, content-addressed, under the
//! provider's state. Its result names the arguments the host appends to this
//! same registered provider executable: `resident.serve --config <path>`.
//!
//! `resident.serve` serves the SDK's resident ACP v2 endpoint on stdio. Each
//! turn is an ordinary `codex exec --json` launch of the recorded template run
//! by [`crate::launch`] under the session's launch state root and its
//! session-scoped stop flag: the first turn lets Codex choose the thread
//! (`thread.started`), later turns resume it, `turn.started` is the
//! consumption evidence and each `agent_message` item is one agent message.
//! Logical session ownership, admission and scheduling stay with the host.

use crate::{
    account,
    encoding::sha256_hex,
    envelope::{HostContext, ProviderFailure, RequestEnvelope, CONTRACT},
    launch, policy,
};
use agent_provider_contract::resident_session::{self, ResidentPrepareResult};
use agent_provider_execution::encoding::canonical_json_bytes;
use agent_provider_execution::resident::{
    self as endpoint, ResidentTurns, TurnFailure, TurnFailureKind, TurnRequest,
};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub const PREPARE: &str = resident_session::PREPARE_SUBCOMMAND;
pub const SERVE: &str = "resident.serve";
const MAX_CONFIG_BYTES: usize = 1024 * 1024;

fn invalid(code: &'static str, message: impl Into<String>) -> ProviderFailure {
    ProviderFailure::invalid_request("", code, message)
}

fn io_failure(error: std::io::Error) -> ProviderFailure {
    ProviderFailure::internal("", "resident_io", error.to_string())
}

fn resident_root(host: &HostContext) -> Result<PathBuf, ProviderFailure> {
    let data_root = match &host.data_root {
        Some(root) => PathBuf::from(root),
        None => account::user_home(host)?.join(".local/share/oulipoly-agent-runner"),
    };
    Ok(data_root.join("provider-state/codex/resident"))
}

/// The provider/v1 launch request of one resident turn.
fn turn_request(
    config: &Value,
    request_id: &str,
    cwd: &Path,
    prompt: &str,
    native_session: Option<&str>,
) -> Result<RequestEnvelope, ProviderFailure> {
    let mut host = config["host"].clone();
    host["env"][launch::HOST_LAUNCH_OUTPUT_ENV] = json!("1");
    host["working_directory"] = json!(cwd);
    let mut params = config["launch"].clone();
    params["model"]["inputs"]["prompt"] = json!(prompt);
    params["working_directory"] = json!(cwd);
    params["output_delivery"] = json!({"protocol": launch::LAUNCH_OUTPUT_PROTOCOL});
    if let Some(id) = native_session {
        params["session"] = json!({"known_provider_session_id": id, "start_mode": "resume"});
    }
    serde_json::from_value(json!({"contract": CONTRACT, "request_id": request_id,
        "provider_instance_id": null, "host": host, "params": params}))
    .map_err(|error| invalid("invalid_resident_template", error.to_string()))
}

/// `resident.prepare`.
pub fn prepare(request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    if !resident_session::FAMILY
        .advertised(
            resident_session::SUPPORTED_VERSIONS,
            request.host.env.as_ref(),
        )
        .contains(&1)
    {
        return Err(ProviderFailure::unsupported(
            "",
            "resident_session_not_selected",
            "resident.prepare requires host.env.OULIPOLY_HOST_RESIDENT_SESSION_V1=1",
        ));
    }
    let params = resident_session::decode_prepare_params(&request.params)
        .map_err(|error| invalid("invalid_resident_prepare", error.to_string()))?;
    let mut host = serde_json::to_value(&request.host)
        .map_err(|error| invalid("invalid_host", error.to_string()))?;
    // A resident endpoint outlives this request: its deadline does not apply.
    host["deadline_unix_ms"] = Value::Null;
    if host["env"].is_null() {
        host["env"] = json!({});
    }
    let config = json!({"protocol": resident_session::PROTOCOL, "provider": "codex",
        "host": host, "launch": params.launch});
    // Check the template as a launch of it would, before recording it.
    let probe = turn_request(
        &config,
        "resident-prepare",
        Path::new("/"),
        "resident probe",
        None,
    )?;
    policy::plan(&probe, false)?;
    policy::RuntimeConfig::load(&probe.host)?.validate()?;
    let bytes = canonical_json_bytes(&config);
    let digest = sha256_hex(&bytes);
    let directory = resident_root(&request.host)?.join("configs");
    crate::durable_fs::create_private_directories(&directory).map_err(io_failure)?;
    let path = directory.join(format!("{digest}.json"));
    if !path.is_file() {
        let mut file = tempfile::NamedTempFile::new_in(&directory).map_err(io_failure)?;
        file.write_all(&bytes).map_err(io_failure)?;
        file.as_file().sync_all().map_err(io_failure)?;
        file.persist(&path)
            .map_err(|error| io_failure(error.error))?;
        crate::durable_fs::sync_directory(&directory).map_err(io_failure)?;
    }
    let result = ResidentPrepareResult::v1(
        vec![SERVE.into(), "--config".into(), path.display().to_string()],
        digest,
    );
    serde_json::to_value(result).map_err(|error| invalid("resident_result", error.to_string()))
}

struct CodexTurns {
    config: Value,
}

impl ResidentTurns for CodexTurns {
    fn implementation(&self) -> (String, String) {
        (
            "agent-runner-codex".into(),
            env!("CARGO_PKG_VERSION").into(),
        )
    }

    fn run_turn(
        &self,
        turn: &TurnRequest,
        stop: &AtomicBool,
        events: &mut dyn Write,
    ) -> Result<i32, TurnFailure> {
        let classify = |failure: ProviderFailure| TurnFailure {
            kind: match failure.code {
                "launch_cancelled" => TurnFailureKind::Cancelled,
                "launch_reconciliation_required" => TurnFailureKind::ReconciliationRequired,
                _ => TurnFailureKind::Failed,
            },
            code: failure.code.into(),
            message: failure.message,
        };
        let request = turn_request(
            &self.config,
            &turn.request_id,
            &turn.cwd,
            &turn.prompt,
            turn.native_session_id.as_deref(),
        )
        .map_err(classify)?;
        let mut events = events;
        launch::run_resident_turn(&request, &turn.state_root, stop, &mut events).map_err(classify)
    }
}

/// `resident.serve --config <path>`: serves one ACP v2 connection on stdio.
pub fn serve(args: &[String]) -> i32 {
    let [_, _, flag, path] = args else {
        eprintln!("usage: agent-runner-codex resident.serve --config <path>");
        return 2;
    };
    if flag != "--config" {
        eprintln!("usage: agent-runner-codex resident.serve --config <path>");
        return 2;
    }
    let path = Path::new(path);
    let config = match load_config(path) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("resident configuration refused: {message}");
            return 2;
        }
    };
    let host: HostContext = match serde_json::from_value(config["host"].clone()) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("resident configuration refused: {error}");
            return 2;
        }
    };
    let state_root = match resident_root(&host) {
        Ok(root) => root,
        Err(failure) => {
            eprintln!("resident state unavailable: {}", failure.message);
            return 2;
        }
    };
    let stdin = std::io::BufReader::new(std::io::stdin());
    match endpoint::serve(
        Arc::new(CodexTurns { config }),
        &state_root,
        stdin,
        std::io::stdout(),
    ) {
        Ok(_) => 0,
        Err(error) => {
            eprintln!("resident endpoint failed: {error}");
            1
        }
    }
}

/// Reads a recorded configuration whose content still matches its name.
fn load_config(path: &Path) -> Result<Value, String> {
    let bytes = crate::durable_fs::read_file_bounded(path, MAX_CONFIG_BYTES)
        .map_err(|error| error.to_string())?;
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if sha256_hex(&bytes) != name {
        return Err("configuration content does not match its digest".into());
    }
    let config: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if config["protocol"] != json!(resident_session::PROTOCOL)
        || config["provider"] != json!("codex")
    {
        return Err("not a Codex resident-session/v1 configuration".into());
    }
    Ok(config)
}
