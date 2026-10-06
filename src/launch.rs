//! Stable `codex exec --json` adapter over the SDK's native effect gate,
//! process-group custody, request custody, and launch-event framing. Requests
//! are journaled before output is published.
use crate::{
    account,
    encoding::{now_unix_ms, sha256_hex},
    envelope::{ProviderFailure, RequestEnvelope, CONTRACT},
    native_process,
    policy::{self, Plan, RuntimeConfig},
    terminal,
};
use agent_provider_execution::{
    custody::{self, CustodyError, LaunchState, RequestCustody},
    framing::{FramingError, LaunchEventWriter},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
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

#[derive(Default)]
struct ChannelSummary {
    bytes: u64,
    sha256: Sha256,
}
impl ChannelSummary {
    fn accept(&mut self, bytes: &[u8]) -> Result<(), ProviderFailure> {
        self.bytes = self.bytes.checked_add(bytes.len() as u64).ok_or_else(|| {
            failure(
                "launch_output_accounting",
                "Launch output byte count overflowed",
            )
        })?;
        self.sha256.update(bytes);
        Ok(())
    }
    fn value(&self) -> Value {
        json!({"bytes":self.bytes,"sha256":format!("{:x}",self.sha256.clone().finalize())})
    }
}

#[derive(Default)]
struct OutputSummary {
    stdout: ChannelSummary,
    stderr: ChannelSummary,
    data_event_count: u64,
}
impl OutputSummary {
    fn accept(&mut self, kind: &str, bytes: &[u8]) -> Result<(), ProviderFailure> {
        self.data_event_count = self.data_event_count.checked_add(1).ok_or_else(|| {
            failure(
                "launch_output_accounting",
                "Launch output event count overflowed",
            )
        })?;
        if kind == "stdout" {
            self.stdout.accept(bytes)
        } else {
            self.stderr.accept(bytes)
        }
    }
    fn value(&self) -> Value {
        json!({"protocol":LAUNCH_OUTPUT_PROTOCOL,"stdout":self.stdout.value(),"stderr":self.stderr.value(),"data_event_count":self.data_event_count})
    }
}

const STREAM_CLOSE_GRACE: Duration = Duration::from_secs(2);

fn install_cancellation_handlers() {
    #[cfg(unix)]
    agent_provider_execution::cancellation::install_termination_handlers();
}

fn cancel_requested() -> bool {
    #[cfg(unix)]
    return agent_provider_execution::cancellation::termination_requested();
    #[cfg(not(unix))]
    false
}

fn deadline_elapsed(request: &RequestEnvelope) -> bool {
    request
        .host
        .deadline_unix_ms
        .is_some_and(|deadline| now_unix_ms() >= deadline)
}

pub(crate) fn validate_admission(request: &RequestEnvelope) -> Result<(), ProviderFailure> {
    if cancel_requested() {
        return Err(failure(
            "launch_cancelled",
            "Launch was cancelled before native admission",
        ));
    }
    if deadline_elapsed(request) {
        return Err(failure(
            "launch_deadline",
            "Host launch deadline elapsed before native admission",
        ));
    }
    Ok(())
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

struct ChildGuard(Child, bool);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.1 {
            native_process::terminate_process_group_child(&mut self.0);
        }
    }
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

struct Stream<'a, W: Write> {
    events: LaunchEventWriter<'a, W>,
    output: OutputSummary,
}
impl<W: Write> Stream<'_, W> {
    fn event(&mut self, event: Value) -> Result<(), ProviderFailure> {
        self.events.event(event).map_err(framing_failure)
    }
    fn marker(&mut self, name: &str, value: Value) -> Result<(), ProviderFailure> {
        self.events.marker(name, value).map_err(framing_failure)
    }
    fn bytes(&mut self, kind: &str, bytes: &[u8]) -> Result<(), ProviderFailure> {
        self.events.data(kind, bytes).map_err(framing_failure)?;
        self.output.accept(kind, bytes)
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
    let key = custody::request_key(request.provider_instance_id.as_deref(), &request.request_id);
    let launch_custody = RequestCustody::acquire(&state_root, &key).map_err(custody_failure)?;
    let state_path = launch_custody.state_path();
    let journal_path = launch_custody.journal_path();
    let digest = sha256_hex(
        serde_json::to_vec(&json!({"params":request.params,"config":config,
        "host_env":request.host.env,"host_working_directory":request.host.working_directory,
        "account_home":account::home(&request.host, &plan.settings_id)?}))
        .unwrap()
        .as_slice(),
    );
    if let Some(state) = launch_custody.load_state().map_err(custody_failure)? {
        if state.digest != digest {
            return Err(ProviderFailure::conflict(
                "",
                "request_changed",
                "Request ID was already used with different inputs",
                json!({}),
            ));
        }
        if state.is_complete() {
            launch_custody
                .replay(&state, writer)
                .map_err(custody_failure)?;
            return Ok(state.exit_code.unwrap_or(1));
        }
        if let (Some(id), Some(incarnation)) = (state.actor_id, state.incarnation) {
            native_process::terminate_process_group_actor(&native_process::ProcessGroupActor {
                process_group_id: id,
                incarnation,
            })
            .map_err(io_failure)?;
        }
        return Err(ProviderFailure::conflict("", "launch_reconciliation_required", "Prior invocation ended before terminal custody; inspect the Codex session before issuing a new request", json!({})));
    }
    validate_admission(request)?;
    config.validate()?;
    if !Path::new(working_directory).is_dir() {
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
    let _session_lock = if let Some(id) = session {
        let path = state_root.join(format!(
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
        Some(file)
    } else {
        None
    };
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
    if let Some(id) = session {
        publish_session(&session_path, id)?;
    }
    let args = native_args(&config, &plan, &env, session);
    let mut gate = native_process::gated_command(&config.codex_bin, &args).map_err(io_failure)?;
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
        .current_dir(working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut state = LaunchState::prepared(digest);
    if let Err(error) = validate_admission(request) {
        std::fs::remove_file(&session_path).map_err(io_failure)?;
        return Err(error);
    }
    launch_custody
        .write_state(&state)
        .map_err(custody_failure)?;
    if let Err(error) = validate_admission(request) {
        std::fs::remove_file(&state_path).map_err(io_failure)?;
        std::fs::remove_file(&session_path).map_err(io_failure)?;
        return Err(error);
    }
    let (child, release) = gate.spawn().map_err(io_failure)?;
    let mut child = ChildGuard(child, true);
    let actor = native_process::actor_for_child(&child.0).map_err(io_failure)?;
    state.actor_id = Some(actor.process_group_id);
    state.incarnation = Some(actor.incarnation);
    state.phase = custody::PHASE_RUNNING.into();
    launch_custody
        .write_state(&state)
        .map_err(custody_failure)?;
    let journal = launch_custody.create_journal().map_err(io_failure)?;
    let mut stream = Stream {
        events: LaunchEventWriter::new(writer, journal, CONTRACT, &request.request_id),
        output: OutputSummary::default(),
    };
    let (send, receive) = mpsc::sync_channel(32);
    for (kind, reader) in [
        (
            "stdout",
            Box::new(child.0.stdout.take().unwrap()) as Box<dyn Read + Send>,
        ),
        (
            "stderr",
            Box::new(child.0.stderr.take().unwrap()) as Box<dyn Read + Send>,
        ),
    ] {
        let send = send.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let mut bytes = Vec::new();
                match reader
                    .by_ref()
                    .take(MAX_LINE + 1)
                    .read_until(b'\n', &mut bytes)
                {
                    Ok(0) => break,
                    Ok(_) if bytes.len() as u64 <= MAX_LINE => {
                        if send.send((kind, Ok(bytes))).is_err() {
                            return;
                        }
                    }
                    _ => {
                        let _ = send.send((
                            kind,
                            Err("Codex stream line exceeded 4 MiB or could not be read"),
                        ));
                        break;
                    }
                }
            }
        });
    }
    drop(send);
    if let Err(error) = validate_admission(request) {
        drop(release);
        native_process::terminate_process_group_child(&mut child.0);
        child.1 = false;
        std::fs::remove_file(&journal_path).map_err(io_failure)?;
        std::fs::remove_file(&state_path).map_err(io_failure)?;
        std::fs::remove_file(&session_path).map_err(io_failure)?;
        return Err(error);
    }
    release.release().map_err(io_failure)?;
    let mut stdin = child.0.stdin.take().unwrap();
    let prompt = plan.prompt.clone();
    let input = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
    stream.marker(
        "codex.route",
        json!({"account":plan.settings_id,"model":plan.model,"effort":plan.effort}),
    )?;
    let mut thread_id = session.map(str::to_string);
    let mut completed = false;
    let mut assistant_response = false;
    let mut failed = false;
    let mut native_failure = None;
    let mut native_status = None;
    let mut last_heartbeat = Instant::now();
    let mut last_native_event = Instant::now();
    let mut exited_at = None;
    let mut streams_closed_at = None;
    let mut forced_status = None;
    loop {
        if forced_status.is_none() && (cancel_requested() || deadline_elapsed(request)) {
            forced_status = Some(terminal::ProcessStatus::Cancelled);
            native_status = native_process::terminate_process_group_child(&mut child.0);
            child.1 = false;
            exited_at = Some(Instant::now());
        }
        match receive.recv_timeout(Duration::from_millis(100)) {
            Ok((kind, result)) => {
                last_native_event = Instant::now();
                let bytes = result.map_err(|message| failure("native_stream_invalid", message))?;
                if kind == "stderr" {
                    stream.bytes(kind, &bytes)?;
                } else {
                    let event: Value = serde_json::from_slice(&bytes).map_err(|_| {
                        failure("native_json_invalid", "Codex emitted invalid JSON")
                    })?;
                    match event.get("type").and_then(Value::as_str).unwrap_or("") {
                        "thread.started" => {
                            if let Some(id) = event.get("thread_id").and_then(Value::as_str) {
                                if !valid_thread_id(id) {
                                    return Err(failure(
                                        "invalid_thread_identity",
                                        "Codex returned an invalid thread ID",
                                    ));
                                }
                                if thread_id.as_deref().is_some_and(|known| known != id) {
                                    return Err(failure(
                                        "thread_identity_changed",
                                        "Codex returned a different thread ID",
                                    ));
                                }
                                publish_session(&session_path, id)?;
                                thread_id = Some(id.into());
                                stream.marker(
                                    "oulipoly.provider_session",
                                    json!({"provider_session_id":id,"source":"codex.exec.json"}),
                                )?;
                            }
                        }
                        "turn.started" => {
                            if let Some(id) = &thread_id {
                                stream.marker("oulipoly.submitted_user_turn", json!({"provider_session_id":id,"prompt_sha256":sha256_hex(plan.prompt.as_bytes()),"source":"codex.exec.json"}))?;
                            }
                        }
                        "item.completed" => {
                            if event.pointer("/item/type").and_then(Value::as_str)
                                == Some("agent_message")
                            {
                                if let Some(text) =
                                    event.pointer("/item/text").and_then(Value::as_str)
                                {
                                    assistant_response |= !text.trim().is_empty();
                                    stream.bytes("stdout", format!("{text}\n").as_bytes())?;
                                }
                            }
                        }
                        "turn.completed" => {
                            completed = true;
                        }
                        "turn.failed" => {
                            failed = true;
                            native_failure = terminal::NativeFailure::from_event(&event);
                            stream.bytes("stderr", &bytes)?;
                        }
                        "error" => {
                            native_failure = terminal::NativeFailure::from_event(&event);
                            stream.bytes("stderr", &bytes)?;
                        }
                        _ => {}
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                streams_closed_at.get_or_insert_with(Instant::now);
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if native_status.is_none() {
            native_status = child.0.try_wait().map_err(io_failure)?;
            if native_status.is_some() {
                // Once the native leader exits, stop descendants but continue
                // draining buffered output; a busy drain has no lifetime cap.
                if child.1 {
                    native_process::terminate_process_group_child(&mut child.0);
                    child.1 = false;
                }
                exited_at = Some(Instant::now());
            }
        }
        if streams_closed_at.is_some() && native_status.is_some() {
            break;
        }
        if streams_closed_at.is_some_and(|time| time.elapsed() >= STREAM_CLOSE_GRACE) {
            return Err(failure(
                "native_streams_closed",
                "Codex closed its streams without exiting; native process group terminated",
            ));
        }
        if exited_at.is_some_and(|time| time.elapsed() > STREAM_CLOSE_GRACE)
            && last_native_event.elapsed() > STREAM_CLOSE_GRACE
        {
            return Err(failure(
                "native_stream_drain_incomplete",
                "Native output pipes remained open after process-group termination",
            ));
        }
        if last_heartbeat.elapsed() >= Duration::from_secs(1) {
            stream.event(json!({"kind":"heartbeat"}))?;
            last_heartbeat = Instant::now();
        }
    }
    let status = match native_status {
        Some(status) => status,
        None => native_process::terminate_process_group_child(&mut child.0)
            .ok_or_else(|| failure("native_wait_failed", "Could not reap the native process"))?,
    };
    // Finish process-group custody even if a native child inherited stream fds.
    if child.1 {
        native_process::terminate_process_group_child(&mut child.0);
        child.1 = false;
    }
    let input_done = Instant::now();
    while !input.is_finished() && input_done.elapsed() < STREAM_CLOSE_GRACE {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !input.is_finished() {
        return Err(failure(
            "stdin_failed",
            "Codex input pipe remained open after native termination",
        ));
    }
    if input
        .join()
        .map_err(|_| failure("stdin_failed", "Codex input writer failed"))?
        .is_err()
        && status.success()
        && forced_status.is_none()
    {
        return Err(failure(
            "stdin_failed",
            "Could not deliver the complete prompt to Codex",
        ));
    }
    let code = status.code().unwrap_or(1);
    let code = if code == 0 && (!completed || failed) {
        1
    } else {
        code
    };
    let terminal_status = forced_status.unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return terminal::ProcessStatus::SignalTerminated { signal };
            }
        }
        terminal::ProcessStatus::Exited { code }
    });
    let code = terminal::exit_code_for_status(&terminal_status);
    if completed && !failed && assistant_response && code == 0 {
        stream.marker("oulipoly.produced_assistant_response", json!(true))?;
    }
    if output_requested {
        stream.marker(LAUNCH_OUTPUT_COMPLETE_MARKER, stream.output.value())?;
    }
    stream.event(json!({"kind":"exit", "status":terminal::process_status_json(&terminal_status),
        "terminal_signal":terminal::classify_with_failure(&terminal_status,now_unix_ms(),native_failure,terminal::host_supports_unavailable(&request.host)), "session":{"provider_session_id":thread_id}}))?;
    let receipt = stream.events.seal().map_err(io_failure)?;
    state.journal_sha256 = Some(receipt.sha256);
    state.journal_len = Some(receipt.len);
    state.phase = custody::PHASE_COMPLETE.into();
    state.exit_code = Some(code);
    state.actor_id = None;
    state.incarnation = None;
    launch_custody
        .write_state(&state)
        .map_err(custody_failure)?;
    Ok(code)
}
