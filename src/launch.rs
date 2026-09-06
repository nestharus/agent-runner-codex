//! Stable `codex exec --json` adapter, using the copied native process gate and
//! process-group custody. Requests are journaled before output is published.
use crate::{
    account,
    encoding::{encode_base64, now_unix_ms, sha256_hex},
    envelope::{ProviderFailure, RequestEnvelope, CONTRACT},
    native_process,
    policy::{self, Plan, RuntimeConfig},
    terminal,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
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

fn replay_journal<W: Write>(
    path: &Path,
    state: &State,
    writer: &mut W,
) -> Result<(), ProviderFailure> {
    let mut file = File::open(path).map_err(|_| {
        failure(
            "launch_journal_invalid",
            "Completed launch journal is missing",
        )
    })?;
    if Some(file.metadata().map_err(io_failure)?.len()) != state.journal_len {
        return Err(failure(
            "launch_journal_invalid",
            "Completed launch journal does not match its durable receipt",
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(io_failure)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if state.journal_sha256.as_deref() != Some(format!("{:x}", hash.finalize()).as_str()) {
        return Err(failure(
            "launch_journal_invalid",
            "Completed launch journal does not match its durable receipt",
        ));
    }
    file.seek(SeekFrom::Start(0)).map_err(io_failure)?;
    std::io::copy(&mut file, writer).map_err(io_failure)?;
    writer.flush().map_err(io_failure)
}
const PINNED_VERSION: &str = "codex-cli 0.153.4";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CLOSE_GRACE: Duration = Duration::from_secs(2);
const MAX_VERSION_BYTES: u64 = 64 * 1024;
static CANCEL_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn record_cancel_signal(signal: i32) {
    CANCEL_SIGNAL.store(signal, std::sync::atomic::Ordering::Relaxed);
}

fn install_cancellation_handlers() {
    #[cfg(unix)]
    {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = record_cancel_signal as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
            libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        });
    }
}

fn cancel_requested() -> bool {
    CANCEL_SIGNAL.load(std::sync::atomic::Ordering::Relaxed) != 0
}

fn deadline_elapsed(request: &RequestEnvelope) -> bool {
    request
        .host
        .deadline_unix_ms
        .is_some_and(|deadline| now_unix_ms() >= deadline)
}

fn validate_admission(request: &RequestEnvelope) -> Result<(), ProviderFailure> {
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

pub(crate) fn verify_version(
    config: &RuntimeConfig,
    env: &BTreeMap<String, String>,
    request: &RequestEnvelope,
) -> Result<(), ProviderFailure> {
    validate_admission(request)?;
    let mut output = tempfile::tempfile().map_err(io_failure)?;
    let mut command = std::process::Command::new(&config.codex_bin);
    command
        .arg("--version")
        .envs(env)
        .stdin(Stdio::null())
        .stdout(output.try_clone().map_err(io_failure)?)
        .stderr(Stdio::null());
    native_process::configure_process_group(&mut command);
    let mut child = ChildGuard(command.spawn().map_err(io_failure)?, true);
    let started = Instant::now();
    let status = loop {
        validate_admission(request)?;
        if output.metadata().map_err(io_failure)?.len() > MAX_VERSION_BYTES {
            return Err(failure(
                "codex_version_output_limit",
                "Codex version output exceeded 64 KiB",
            ));
        }
        if let Some(status) = child.0.try_wait().map_err(io_failure)? {
            break status;
        }
        if started.elapsed() >= VERSION_TIMEOUT {
            return Err(failure(
                "codex_version_timeout",
                "Codex version probe timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    native_process::terminate_process_group_child(&mut child.0);
    child.1 = false;
    output.seek(SeekFrom::Start(0)).map_err(io_failure)?;
    let mut bytes = Vec::new();
    output
        .take(MAX_VERSION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_failure)?;
    if bytes.len() as u64 > MAX_VERSION_BYTES
        || !status.success()
        || String::from_utf8_lossy(&bytes).trim() != PINNED_VERSION
    {
        return Err(failure("codex_version_unverified", format!("This adapter's tool inventory is verified against {PINNED_VERSION}; validate an upgrade before launch")));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct State {
    digest: String,
    phase: String,
    actor_id: Option<u32>,
    incarnation: Option<String>,
    exit_code: Option<i32>,
    journal_sha256: Option<String>,
    journal_len: Option<u64>,
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

fn write_state(path: &Path, state: &State) -> Result<(), ProviderFailure> {
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(io_failure)?;
    serde_json::to_writer(&mut temporary, state)
        .map_err(|e| failure("launch_state_write", e.to_string()))?;
    temporary.as_file().sync_all().map_err(io_failure)?;
    temporary.persist(path).map_err(|e| io_failure(e.error))?;
    File::open(path.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(io_failure)
}

struct Stream<'a, W: Write> {
    writer: &'a mut W,
    journal: File,
    request_id: &'a str,
    seq: u64,
    bytes: u64,
    journal_sha256: Sha256,
    output: OutputSummary,
}
impl<W: Write> Stream<'_, W> {
    fn event(&mut self, mut event: Value) -> Result<(), ProviderFailure> {
        self.seq += 1;
        event["contract"] = json!(CONTRACT);
        event["request_id"] = json!(self.request_id);
        event["seq"] = json!(self.seq);
        event["time_unix_ms"] = json!(now_unix_ms());
        let mut bytes = serde_json::to_vec(&event).unwrap();
        bytes.push(b'\n');
        self.bytes = self.bytes.checked_add(bytes.len() as u64).ok_or_else(|| {
            failure(
                "launch_output_accounting",
                "Launch journal byte count overflowed",
            )
        })?;
        self.journal.write_all(&bytes).map_err(io_failure)?;
        self.journal_sha256.update(&bytes);
        self.writer.write_all(&bytes).map_err(io_failure)?;
        self.writer.flush().map_err(io_failure)
    }
    fn marker(&mut self, name: &str, value: Value) -> Result<(), ProviderFailure> {
        self.event(json!({"kind":"marker", "name":name, "value":value}))
    }
    fn bytes(&mut self, kind: &str, bytes: &[u8]) -> Result<(), ProviderFailure> {
        self.event(json!({"kind":kind,"data_base64":encode_base64(bytes)}))?;
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
    let key = sha256_hex(
        &serde_json::to_vec(&json!([request.provider_instance_id, request.request_id])).unwrap(),
    );
    let state_path = state_root.join(format!("{key}.json"));
    let journal_path = state_root.join(format!("{key}.jsonl"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_root.join(format!("{key}.lock")))
        .map_err(io_failure)?;
    lock.try_lock_exclusive().map_err(|_| {
        ProviderFailure::retryable_conflict(
            "",
            "launch_busy",
            "This request is already executing",
            json!({}),
        )
    })?;
    let digest = sha256_hex(
        serde_json::to_vec(&json!({"params":request.params,"config":config,
        "host_env":request.host.env,"host_working_directory":request.host.working_directory,
        "account_home":account::home(&request.host, &plan.settings_id)?}))
        .unwrap()
        .as_slice(),
    );
    if state_path.is_file() {
        let state: State = serde_json::from_slice(
            &crate::durable_fs::read_file_bounded(&state_path, 64 * 1024).map_err(io_failure)?,
        )
        .map_err(|_| failure("launch_state_invalid", "Invalid launch state"))?;
        if state.digest != digest {
            return Err(ProviderFailure::conflict(
                "",
                "request_changed",
                "Request ID was already used with different inputs",
                json!({}),
            ));
        }
        if state.phase == "complete" {
            replay_journal(&journal_path, &state, writer)?;
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
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(io_failure)?;
        file.try_lock_exclusive().map_err(|_| {
            ProviderFailure::retryable_conflict(
                "",
                "session_busy",
                "This session already has an active turn",
                json!({}),
            )
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
    let session_path = state_root.join(format!("{key}.session"));
    env.insert(
        "AGENT_RUNNER_CODEX_SESSION_FILE".into(),
        session_path.display().to_string(),
    );
    env.remove("AGENT_RUNNER_CODEX_SESSION_ID");
    if let Some(id) = session {
        env.insert("AGENT_RUNNER_CODEX_SESSION_ID".into(), id.into());
    }
    crate::request_control::bind(request, &mut env, &root)?;
    verify_version(&config, &env, request)?;
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
    let mut gate =
        native_process::GatedCommand::new(&config.codex_bin, &args).map_err(io_failure)?;
    for key in [
        "AGENT_RUNNER_CODEX_INTERACTIVE",
        "AGENT_RUNNER_CODEX_SESSION_BINDING",
        "AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS",
        crate::request_control::BINDING_ENV,
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
    let mut state = State {
        digest,
        phase: "prepared".into(),
        actor_id: None,
        incarnation: None,
        exit_code: None,
        journal_sha256: None,
        journal_len: None,
    };
    write_state(&state_path, &state)?;
    let (child, release) = gate.spawn().map_err(io_failure)?;
    let mut child = ChildGuard(child, true);
    let actor = native_process::actor_for_child(&child.0).map_err(io_failure)?;
    state.actor_id = Some(actor.process_group_id);
    state.incarnation = Some(actor.incarnation);
    state.phase = "running".into();
    write_state(&state_path, &state)?;
    let journal = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&journal_path)
        .map_err(io_failure)?;
    let mut stream = Stream {
        writer,
        journal,
        request_id: &request.request_id,
        seq: 0,
        bytes: 0,
        journal_sha256: Sha256::new(),
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
    stream.journal.sync_all().map_err(io_failure)?;
    state.journal_sha256 = Some(format!("{:x}", stream.journal_sha256.clone().finalize()));
    state.journal_len = Some(stream.bytes);
    state.phase = "complete".into();
    state.exit_code = Some(code);
    state.actor_id = None;
    state.incarnation = None;
    write_state(&state_path, &state)?;
    Ok(code)
}
