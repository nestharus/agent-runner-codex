//! Stable `codex exec --json` adapter plugged into the SDK's shared one-shot
//! launch lifecycle. The SDK owns custody, replay, reconciliation, admission,
//! the effect gate, draining, cancellation and completion; this module supplies
//! the Codex request digest, session ownership, argv/environment, native event
//! translation and terminal classification.
use crate::{
    account,
    encoding::{now_unix_ms, sha256_hex},
    envelope::{ProviderFailure, RequestEnvelope, CONTRACT},
    native_process,
    policy::{self, Plan, RuntimeConfig},
    terminal,
};
use agent_provider_execution::{
    custody::{self, CustodyError, RequestCustody},
    framing::FramingError,
    lifecycle::{
        self, Channel, EventSink, LaunchAdapter, LaunchSpec, LifecycleError, LifecycleTiming,
        NativeCommand, NativeOutcome, OutputFraming, Preparation, Terminal,
    },
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

const MAX_LINE: u64 = 4 * 1024 * 1024;
pub const HOST_LAUNCH_OUTPUT_ENV: &str = "OULIPOLY_HOST_LAUNCH_OUTPUT_V1";
pub const LAUNCH_OUTPUT_PROTOCOL: &str = "oulipoly.launch_output/v1";
pub const LAUNCH_OUTPUT_COMPLETE_MARKER: &str = "oulipoly.launch_output_complete/v1";

pub fn host_requested_output(host: &crate::envelope::HostContext) -> bool {
    host.env
        .as_ref()
        .and_then(|env| env.get(HOST_LAUNCH_OUTPUT_ENV))
        .map(String::as_str)
        == Some("1")
}

fn output_requested(request: &RequestEnvelope) -> Result<bool, ProviderFailure> {
    let Some(value) = request
        .params
        .get("output_delivery")
        .filter(|value| !value.is_null())
    else {
        return Ok(false);
    };
    if !host_requested_output(&request.host) {
        return Err(ProviderFailure::unsupported(
            &request.request_id,
            "launch_output_not_selected",
            "params.output_delivery requires host.env.OULIPOLY_HOST_LAUNCH_OUTPUT_V1=1",
        ));
    }
    let object = value
        .as_object()
        .filter(|value| value.len() == 1 && value.get("protocol").is_some_and(Value::is_string))
        .ok_or_else(|| {
            ProviderFailure::invalid_request(
                &request.request_id,
                "invalid_launch_output_request",
                "output_delivery must contain only its protocol",
            )
        })?;
    if object["protocol"] != LAUNCH_OUTPUT_PROTOCOL {
        return Err(ProviderFailure::unsupported(
            &request.request_id,
            "unsupported_launch_output_protocol",
            "Unsupported launch output delivery protocol",
        ));
    }
    Ok(true)
}

fn install_cancellation_handlers() {
    #[cfg(unix)]
    agent_provider_execution::cancellation::install_termination_handlers();
}

pub(crate) fn validate_admission(request: &RequestEnvelope) -> Result<(), ProviderFailure> {
    lifecycle::check_admission(request.host.deadline_unix_ms).map_err(ProviderFailure::from)
}

pub(crate) fn valid_thread_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

fn publish_session(path: &Path, id: &str) -> Result<(), ProviderFailure> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(io_failure)?;
    writeln!(file, "{id}").map_err(io_failure)?;
    file.as_file().sync_all().map_err(io_failure)?;
    file.persist(path).map_err(|e| io_failure(e.error))?;
    Ok(())
}

fn failure(code: &'static str, message: impl Into<String>) -> ProviderFailure {
    ProviderFailure::internal("", code, message)
}
fn io_failure(error: std::io::Error) -> ProviderFailure {
    failure("launch_io", error.to_string())
}

fn custody_failure(error: CustodyError) -> ProviderFailure {
    match error {
        CustodyError::Busy => ProviderFailure::retryable_conflict(
            "",
            "launch_busy",
            "This request is already executing",
            json!({}),
        ),
        CustodyError::InvalidState => failure("launch_state_invalid", "Invalid launch state"),
        CustodyError::StateWrite(message) => failure("launch_state_write", message),
        CustodyError::JournalMissing => failure(
            "launch_journal_invalid",
            "Completed launch journal is missing",
        ),
        CustodyError::JournalMismatch => failure(
            "launch_journal_invalid",
            "Completed launch journal does not match its durable receipt",
        ),
        CustodyError::JournalOverflow => framing_failure(FramingError::Overflow),
        CustodyError::Io(error) => io_failure(error),
    }
}

fn framing_failure(error: FramingError) -> ProviderFailure {
    match error {
        FramingError::Overflow => failure(
            "launch_output_accounting",
            "Launch journal byte count overflowed",
        ),
        FramingError::Io(error) => io_failure(error),
    }
}

/// Codex failure codes and messages for the shared lifecycle's outcomes.
impl From<LifecycleError> for ProviderFailure {
    fn from(error: LifecycleError) -> Self {
        match error {
            LifecycleError::Busy => custody_failure(CustodyError::Busy),
            LifecycleError::RequestChanged => ProviderFailure::conflict(
                "",
                "request_changed",
                "Request ID was already used with different inputs",
                json!({}),
            ),
            LifecycleError::ReconciliationRequired => ProviderFailure::conflict("", "launch_reconciliation_required", "Prior invocation ended before terminal custody; inspect the Codex session before issuing a new request", json!({})),
            LifecycleError::Cancelled => failure(
                "launch_cancelled",
                "Launch was cancelled before native admission",
            ),
            LifecycleError::DeadlineElapsed => failure(
                "launch_deadline",
                "Host launch deadline elapsed before native admission",
            ),
            LifecycleError::Custody(error) => custody_failure(error),
            LifecycleError::Framing(error) => framing_failure(error),
            LifecycleError::Io(error) => io_failure(error),
            LifecycleError::NativeStreamInvalid => failure(
                "native_stream_invalid",
                "Codex stream line exceeded 4 MiB or could not be read",
            ),
            LifecycleError::NativeStreamsClosed => failure(
                "native_streams_closed",
                "Codex closed its streams without exiting; native process group terminated",
            ),
            LifecycleError::NativeDrainIncomplete => failure(
                "native_stream_drain_incomplete",
                "Native output pipes remained open after process-group termination",
            ),
            LifecycleError::InputStalled => failure(
                "stdin_failed",
                "Codex input pipe remained open after native termination",
            ),
            LifecycleError::InputWriterFailed => failure("stdin_failed", "Codex input writer failed"),
            LifecycleError::InputIncomplete => failure(
                "stdin_failed",
                "Could not deliver the complete prompt to Codex",
            ),
            LifecycleError::WaitFailed => {
                failure("native_wait_failed", "Could not reap the native process")
            }
            LifecycleError::AccountingOverflow => failure(
                "launch_output_accounting",
                "Launch output byte count overflowed",
            ),
        }
    }
}

/// The environment is inherited wholesale. Only explicit profile and tool
/// bindings are changed; MCP receives names through env_vars, never values in argv.
pub fn native_args(
    config: &RuntimeConfig,
    plan: &Plan,
    env: &BTreeMap<String, String>,
    session: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "exec".into(),
        "--ignore-user-config".into(),
        "--ignore-rules".into(),
    ];
    args.extend(managed_native_args(config, plan, env));
    args.extend([
        "--skip-git-repo-check".into(),
        "--json".into(),
        "--color".into(),
        "never".into(),
    ]);
    if let Some(id) = session {
        args.extend(["resume".into(), id.into()]);
    }
    args.push("-".into());
    args
}

/// Identical model, system instructions and tool inventory for exec and TUI.
pub(crate) fn managed_native_args(
    config: &RuntimeConfig,
    plan: &Plan,
    env: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut args = vec!["--dangerously-bypass-approvals-and-sandbox".into()];
    args.extend(crate::models::args(&plan.model, &plan.effort));
    for pair in [
        format!(
            "model_instructions_file={}",
            json!(config.system_prompt_file)
        ),
        format!(
            "model_catalog_json={}",
            json!(config.bash_mcp_path.parent().unwrap().join("models.json"))
        ),
        "web_search=\"disabled\"".into(),
        "shell_environment_policy.inherit=\"all\"".into(),
        "shell_environment_policy.ignore_default_excludes=true".into(),
        "mcp_servers={}".into(),
    ] {
        args.extend(["-c".into(), pair]);
    }
    for feature in [
        "shell_tool",
        "unified_exec",
        "multi_agent",
        "multi_agent_v2",
        "apps",
        "plugins",
        "remote_plugin",
        "hooks",
        "image_generation",
        "view_image",
        "browser_use",
        "computer_use",
        "code_mode",
        "code_mode_host",
        "goals",
        "sleep_tool",
        "skill_search",
        "personality",
        "memories",
        "workspace_dependencies",
        "tool_suggest",
    ] {
        args.extend(["-c".into(), format!("features.{feature}=false")]);
    }
    let env_vars: std::collections::BTreeSet<String> = std::env::vars_os()
        .filter_map(|(key, _)| key.into_string().ok())
        .chain(env.keys().cloned())
        .collect();
    for (key, value) in [
        ("command", json!(config.bun_bin)),
        ("args", json!(["--no-install", config.bash_mcp_path])),
        ("env_vars", json!(env_vars)),
        ("enabled_tools", json!(["bash"])),
        ("required", json!(true)),
        ("startup_timeout_sec", json!(20)),
        ("tool_timeout_sec", json!(86400)),
    ] {
        args.extend(["-c".into(), format!("mcp_servers.agent_bash.{key}={value}")]);
    }
    if let Some(instructions) = env.get("AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS") {
        args.extend([
            "-c".into(),
            format!("developer_instructions={}", json!(instructions)),
        ]);
    }
    args
}

pub fn run<W: Write>(request: &RequestEnvelope, writer: &mut W) -> Result<i32, ProviderFailure> {
    install_cancellation_handlers();
    let output_requested = output_requested(request)?;
    let plan = policy::plan(request, false)?;
    let config = RuntimeConfig::load(&request.host)?;
    let working_directory = request
        .params
        .get("working_directory")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("missing_working_directory", "working_directory is required"))?;
    if !Path::new(working_directory).is_absolute() {
        return Err(failure(
            "invalid_working_directory",
            "working_directory must be an existing absolute directory",
        ));
    }
    let session = request
        .params
        .pointer("/session/known_provider_session_id")
        .and_then(Value::as_str);
    if session.is_none() && request.params.pointer("/session/start_mode").is_some() {
        return Err(failure(
            "missing_session_id",
            "Session start_mode requires a known Codex session ID",
        ));
    }
    if let Some(id) = session {
        if request
            .params
            .pointer("/session/start_mode")
            .and_then(Value::as_str)
            != Some("resume")
        {
            return Err(ProviderFailure::unsupported(
                "",
                "caller_session_id_unsupported",
                "Codex chooses new thread IDs; only existing Codex threads may be resumed",
            ));
        }
        if !valid_thread_id(id) {
            return Err(failure(
                "invalid_session_id",
                "Codex session ID must be a UUID",
            ));
        }
    }
    let user = account::user_home(&request.host)?;
    let root = request
        .host
        .data_root
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| user.join(".local/share/oulipoly-agent-runner"));
    let state_root = root.join("provider-state/codex/launch");
    crate::durable_fs::create_private_directories(&state_root).map_err(io_failure)?;
    let spec = LaunchSpec {
        contract: CONTRACT,
        request_id: &request.request_id,
        provider_instance_id: request.provider_instance_id.as_deref(),
        deadline_unix_ms: request.host.deadline_unix_ms,
        state_root: &state_root,
        timing: LifecycleTiming::default(),
    };
    let mut adapter = CodexLaunch {
        request,
        plan,
        config,
        working_directory,
        session,
        output_requested,
        state_root: &state_root,
        session_path: None,
        session_lock: None,
        thread_id: session.map(str::to_string),
        completed: false,
        assistant_response: false,
        failed: false,
        native_failure: None,
    };
    lifecycle::run_launch(&spec, &mut adapter, writer)
}

/// Codex plug points for the shared launch lifecycle: request digest, account
/// session ownership and argv/environment preparation, `codex exec --json`
/// translation, and terminal classification.
struct CodexLaunch<'a> {
    request: &'a RequestEnvelope,
    plan: Plan,
    config: RuntimeConfig,
    working_directory: &'a str,
    session: Option<&'a str>,
    output_requested: bool,
    state_root: &'a Path,
    session_path: Option<PathBuf>,
    session_lock: Option<std::fs::File>,
    thread_id: Option<String>,
    completed: bool,
    assistant_response: bool,
    failed: bool,
    native_failure: Option<terminal::NativeFailure>,
}

impl LaunchAdapter for CodexLaunch<'_> {
    type Failure = ProviderFailure;

    fn request_digest(&mut self) -> Result<String, ProviderFailure> {
        let request = self.request;
        Ok(sha256_hex(
            serde_json::to_vec(&json!({"params":request.params,"config":self.config,
            "host_env":request.host.env,"host_working_directory":request.host.working_directory,
            "account_home":account::home(&request.host, &self.plan.settings_id)?}))
            .unwrap()
            .as_slice(),
        ))
    }

    fn prepare(&mut self, launch_custody: &RequestCustody) -> Result<Preparation, ProviderFailure> {
        let request = self.request;
        let plan = &self.plan;
        let config = &self.config;
        let session = self.session;
        config.validate()?;
        if !Path::new(self.working_directory).is_dir() {
            return Err(failure(
                "invalid_working_directory",
                "working_directory must be an existing directory",
            ));
        }
        if let Some(id) = session {
            // Resolve within the selected account before allowing native resume.
            let mut locate = request.clone();
            locate.params = json!({"settings_id":plan.settings_id, "session_id":id});
            let located = crate::session::handle("session.locate_transcript", &locate)?;
            if !located
                .get("located")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(failure(
                    "session_not_found",
                    "The selected Codex account does not own this session",
                ));
            }
        }
        if let Some(id) = session {
            let path = self.state_root.join(format!(
                "session-{}.lock",
                sha256_hex(
                    format!(
                        "{}:{id}",
                        account::home(&request.host, &plan.settings_id)?.display()
                    )
                    .as_bytes()
                )
            ));
            let file = custody::try_lock_exclusive(&path).map_err(|error| match error {
                CustodyError::Busy => ProviderFailure::retryable_conflict(
                    "",
                    "session_busy",
                    "This session already has an active turn",
                    json!({}),
                ),
                error => custody_failure(error),
            })?;
            self.session_lock = Some(file);
        }
        // Unrepresentable inherited values still reach the child through Command's
        // native environment inheritance; only explicit Unicode overrides are added.
        let mut env: BTreeMap<String, String> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        if let Some(host_env) = &request.host.env {
            env.extend(host_env.clone());
        }
        // Only the explicit policy-admitted launch environment may carry account
        // instructions; parent process/host environments are stale for this turn.
        env.remove("AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS");
        env.extend(plan.env.clone());
        if env
            .get("AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS")
            .is_some_and(|value| value.trim().is_empty())
        {
            env.remove("AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS");
        }
        // A headless child can inherit these from a managed TUI parent. Its MCP
        // transport must use the private launch receipt and headless lease policy.
        for key in [
            "AGENT_RUNNER_CODEX_INTERACTIVE",
            "AGENT_RUNNER_CODEX_SESSION_BINDING",
            "AGENT_RUNNER_CODEX_REGISTRATION_CWD",
        ] {
            env.remove(key);
        }
        env.insert(
            "AGENT_BASH_BIN".into(),
            config.agent_bash_bin.display().to_string(),
        );
        env.insert(
            "AGENT_BASH_AGENT_RUNNER_BIN".into(),
            config.agent_runner_bin.display().to_string(),
        );
        let session_path = launch_custody.sibling("session");
        env.insert(
            "AGENT_RUNNER_CODEX_SESSION_FILE".into(),
            session_path.display().to_string(),
        );
        env.remove("AGENT_RUNNER_CODEX_SESSION_ID");
        if let Some(id) = session {
            env.insert("AGENT_RUNNER_CODEX_SESSION_ID".into(), id.into());
        }
        // This file is private and fresh for this request. MCP waits for native identity.
        let mut session_file_options = OpenOptions::new();
        session_file_options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            session_file_options.mode(0o600);
        }
        session_file_options
            .open(&session_path)
            .map_err(io_failure)?
            .sync_all()
            .map_err(io_failure)?;
        self.session_path = Some(session_path.clone());
        if let Some(id) = session {
            publish_session(&session_path, id)?;
        }
        let args = native_args(config, plan, &env, session);
        let mut gate =
            native_process::gated_command(&config.codex_bin, &args).map_err(io_failure)?;
        for key in [
            "AGENT_RUNNER_CODEX_INTERACTIVE",
            "AGENT_RUNNER_CODEX_SESSION_BINDING",
            "AGENT_RUNNER_CODEX_REGISTRATION_CWD",
            "AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS",
        ] {
            gate.command_mut().env_remove(key);
        }
        if session.is_none() {
            gate.command_mut()
                .env_remove("AGENT_RUNNER_CODEX_SESSION_ID");
        }
        gate.command_mut()
            .envs(&env)
            .current_dir(self.working_directory);
        Ok(Preparation::Native(NativeCommand {
            command: gate,
            stdin: Some(plan.prompt.clone().into_bytes()),
            framing: OutputFraming::Lines {
                max_bytes: MAX_LINE,
            },
        }))
    }

    fn discard(&mut self, _custody: &RequestCustody) -> Result<(), ProviderFailure> {
        if let Some(path) = self.session_path.take() {
            std::fs::remove_file(path).map_err(io_failure)?;
        }
        Ok(())
    }

    fn started<W: Write>(&mut self, events: &mut EventSink<'_, W>) -> Result<(), ProviderFailure> {
        let plan = &self.plan;
        events.marker(
            "codex.route",
            json!({"account":plan.settings_id,"model":plan.model,"effort":plan.effort}),
        )?;
        Ok(())
    }

    fn output<W: Write>(
        &mut self,
        channel: Channel,
        bytes: Vec<u8>,
        events: &mut EventSink<'_, W>,
    ) -> Result<(), ProviderFailure> {
        if channel == Channel::Stderr {
            events.data(Channel::Stderr, &bytes)?;
            return Ok(());
        }
        let event: Value = serde_json::from_slice(&bytes)
            .map_err(|_| failure("native_json_invalid", "Codex emitted invalid JSON"))?;
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "thread.started" => {
                if let Some(id) = event.get("thread_id").and_then(Value::as_str) {
                    if !valid_thread_id(id) {
                        return Err(failure(
                            "invalid_thread_identity",
                            "Codex returned an invalid thread ID",
                        ));
                    }
                    if self.thread_id.as_deref().is_some_and(|known| known != id) {
                        return Err(failure(
                            "thread_identity_changed",
                            "Codex returned a different thread ID",
                        ));
                    }
                    publish_session(self.session_path.as_ref().unwrap(), id)?;
                    self.thread_id = Some(id.into());
                    events.marker(
                        "oulipoly.provider_session",
                        json!({"provider_session_id":id,"source":"codex.exec.json"}),
                    )?;
                }
            }
            "turn.started" => {
                if let Some(id) = &self.thread_id {
                    events.marker("oulipoly.submitted_user_turn", json!({"provider_session_id":id,"prompt_sha256":sha256_hex(self.plan.prompt.as_bytes()),"source":"codex.exec.json"}))?;
                }
            }
            "item.completed" => {
                if event.pointer("/item/type").and_then(Value::as_str) == Some("agent_message") {
                    if let Some(text) = event.pointer("/item/text").and_then(Value::as_str) {
                        self.assistant_response |= !text.trim().is_empty();
                        events.data(Channel::Stdout, format!("{text}\n").as_bytes())?;
                    }
                }
            }
            "turn.completed" => {
                self.completed = true;
            }
            "turn.failed" => {
                self.failed = true;
                self.native_failure = terminal::NativeFailure::from_event(&event);
                events.data(Channel::Stderr, &bytes)?;
            }
            "error" => {
                self.native_failure = terminal::NativeFailure::from_event(&event);
                events.data(Channel::Stderr, &bytes)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn finish<W: Write>(
        &mut self,
        outcome: NativeOutcome,
        events: &mut EventSink<'_, W>,
    ) -> Result<Terminal, ProviderFailure> {
        let status = outcome.status;
        let code = status.code().unwrap_or(1);
        let code = if code == 0 && (!self.completed || self.failed) {
            1
        } else {
            code
        };
        // Host cancellation and the host deadline both end the turn as cancelled.
        let terminal_status = match outcome.stopped {
            Some(_) => terminal::ProcessStatus::Cancelled,
            None => {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    match status.signal() {
                        Some(signal) => terminal::ProcessStatus::SignalTerminated { signal },
                        None => terminal::ProcessStatus::Exited { code },
                    }
                }
                #[cfg(not(unix))]
                terminal::ProcessStatus::Exited { code }
            }
        };
        let code = terminal::exit_code_for_status(&terminal_status);
        if self.completed && !self.failed && self.assistant_response && code == 0 {
            events.marker("oulipoly.produced_assistant_response", json!(true))?;
        }
        if self.output_requested {
            let mut summary = events.accounting().to_json();
            summary["protocol"] = json!(LAUNCH_OUTPUT_PROTOCOL);
            events.marker(LAUNCH_OUTPUT_COMPLETE_MARKER, summary)?;
        }
        Ok(Terminal {
            status: terminal::process_status_json(&terminal_status),
            terminal_signal: terminal::classify_with_failure(
                &terminal_status,
                now_unix_ms(),
                self.native_failure.take(),
                terminal::host_supports_unavailable(&self.request.host),
            ),
            session: Some(json!({"provider_session_id":self.thread_id})),
            exit_code: code,
        })
    }
}
