//! Resident Codex sessions through the real provider binary and a fake
//! `codex` executable: describe selection, `resident.prepare`, and the
//! `resident.serve` ACP v2 endpoint running `codex exec --json` turns.
#![cfg(target_os = "linux")]

use agent_provider_contract::resident_session as extension;
use agent_runner_codex::dispatch::write_invocation;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Fake `codex exec --json`: a new thread writes its rollout in the account
/// home; `resume <id>` continues it. The prompt (stdin) selects behaviour.
const FAKE_CODEX: &str = r#"#!/usr/bin/env python3
import json, os, subprocess, sys, uuid
args = sys.argv[1:]
prompt = sys.stdin.read()
with open(os.environ['CALLS'], 'a') as f:
    f.write(json.dumps({'argv': args, 'prompt': prompt, 'home': os.environ.get('CODEX_HOME')}) + '\n')
if prompt.split()[:1] == ['preident']:
    # Fails before it reports any thread, as a native failure ahead of its first event would.
    print(json.dumps({'type': 'error', 'message': 'fixture failure before identity'}), flush=True)
    sys.exit(1)
if 'resume' in args:
    thread = args[args.index('resume') + 1]
else:
    thread = str(uuid.uuid4())
    sessions = os.path.join(os.environ['CODEX_HOME'], 'sessions')
    os.makedirs(sessions, exist_ok=True)
    with open(os.path.join(sessions, 'rollout-%s.jsonl' % thread), 'a') as rollout:
        rollout.write(json.dumps({'timestamp': '2026-10-06T00:00:00Z', 'type': 'session_meta', 'payload': {'id': thread, 'cwd': os.getcwd()}}) + '\n')
def emit(event):
    print(json.dumps(event), flush=True)
emit({'type': 'thread.started', 'thread_id': thread})
words = prompt.split()
if words and words[0] == 'noconsume':
    sys.exit(4)
emit({'type': 'turn.started'})
if words and words[0] == 'hang':
    child = subprocess.Popen(['sleep', '300'])
    open(os.path.join(words[1], 'descendant.pid'), 'w').write(str(child.pid))
    emit({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': 'waiting'}})
    child.wait()
    sys.exit(0)
if words and words[0] == 'fail':
    emit({'type': 'turn.failed', 'error': {'message': 'fixture failure'}})
    sys.exit(1)
emit({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': 'reply to %s on %s' % (prompt.strip(), thread)}})
emit({'type': 'turn.completed', 'usage': {}})
"#;

fn file(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn executable(path: &Path, text: &str) {
    file(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self::with_codex(FAKE_CODEX)
    }

    fn with_codex(script: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("u92-correction-codex-resident-")
            .tempdir_in(std::env::temp_dir())
            .unwrap();
        let r = root.path();
        let codex = r.join("codex");
        executable(&codex, script);
        let bash = r.join("bash");
        executable(&bash, "#!/bin/sh\nexit 0\n");
        let mcp = r.join("mcp.ts");
        file(&mcp, "// fixture\n");
        file(
            &r.join("models.json"),
            include_str!("../integrations/codex/models.json"),
        );
        let prompt = r.join("ai/AGENTS.md");
        file(&prompt, "system sentinel\n");
        file(
            &r.join("config/agent-runner-codex/config.toml"),
            &format!(
                "codex_bin = {codex:?}\nbun_bin = {bash:?}\nbash_mcp_path = {mcp:?}\nsystem_prompt_file = {prompt:?}\nagent_bash_bin = {bash:?}\nagent_runner_bin = {bash:?}\n"
            ),
        );
        std::fs::create_dir_all(r.join("work")).unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn host(&self, resident: bool) -> Value {
        let r = self.path();
        let mut env = json!({"HOME": r, "CALLS": r.join("calls.jsonl")});
        if resident {
            env["OULIPOLY_HOST_RESIDENT_SESSION_V1"] = json!("1");
        }
        json!({"app":"test","config_root":r.join("config"),"data_root":r.join("data"),"env":env})
    }

    fn template(&self) -> Value {
        json!({"settings_id":"codex2","mode":"stdin",
            "model":{"name":"gpt-astra-high","provider_args":["-m","gpt-6-astra","-c","model_reasoning_effort=\"high\""],"inputs":{"named":{}}},
            "argv":["codex2","exec","--dangerously-bypass-approvals-and-sandbox","-m","gpt-6-astra","-c","model_reasoning_effort=\"high\""],
            "env":{}})
    }

    fn invoke(&self, operation: &str, host: Value, params: Value) -> Value {
        let request = json!({"contract":"oulipoly.provider/v1","request_id":format!("req-{operation}"),
            "host":host,"params":params});
        let mut output = Vec::new();
        write_invocation(
            &["agent-runner-codex".into(), operation.into()],
            &serde_json::to_vec(&request).unwrap(),
            &mut output,
        );
        serde_json::from_slice(&output).unwrap()
    }

    fn prepare(&self) -> Value {
        let response = self.invoke(
            "resident.prepare",
            self.host(true),
            json!({"protocol":"oulipoly.resident_session/v1","launch":self.template()}),
        );
        assert_eq!(response["ok"], json!(true), "{response}");
        response["result"].clone()
    }

    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn serve(&self, prepared: &Value) -> Client {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"));
        for arg in prepared["invocation"]["args"].as_array().unwrap() {
            command.arg(arg.as_str().unwrap());
        }
        Client::start(command)
    }
}

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: mpsc::Receiver<Value>,
    seen: Vec<Value>,
    next_id: u64,
}

impl Client {
    fn start(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if send
                    .send(serde_json::from_str::<Value>(&line).unwrap())
                    .is_err()
                {
                    return;
                }
            }
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            messages,
            seen: Vec::new(),
            next_id: 0,
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        id
    }

    fn wait(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(index) = self.seen.iter().position(&pred) {
            return self.seen.remove(index);
        }
        let start = Instant::now();
        loop {
            let left = TIMEOUT
                .checked_sub(start.elapsed())
                .unwrap_or_else(|| panic!("timed out waiting for {what}; seen {:?}", self.seen));
            let message = self
                .messages
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no {what}; seen {:?}", self.seen));
            if pred(&message) {
                return message;
            }
            self.seen.push(message);
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.request(method, params);
        self.response(id)
    }

    fn response(&mut self, id: u64) -> Value {
        self.wait(&format!("response {id}"), |m| {
            m.get("method").is_none() && m["id"] == json!(id)
        })
    }

    fn update(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        self.wait(what, |m| {
            m["method"] == json!("session/update") && pred(&m["params"]["update"])
        })["params"]["update"]
            .clone()
    }

    fn idle_for(&mut self, id: &str) -> Value {
        let id = id.to_owned();
        let idle = self.update("tagged idle", move |u| {
            u["state"] == json!("idle") && u["_meta"]["oulipoly.ai/lastUserMessageId"] == json!(id)
        });
        extension::validate("TurnStopReason", &idle["stopReason"]).unwrap();
        extension::validate("NativeTurnMeta", &idle["_meta"]["oulipoly.ai/nativeTurn"]).unwrap();
        idle
    }

    fn prompt(&mut self, session: &str, text: &str, key: Option<&str>) -> u64 {
        let mut params = json!({"sessionId":session,"prompt":[{"type":"text","text":text}]});
        if let Some(key) = key {
            params["_meta"] = json!({"oulipoly.ai/messageKey": key});
        }
        self.request("session/prompt", params)
    }

    fn end(mut self) -> std::process::ExitStatus {
        self.stdin.take();
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(start.elapsed() < TIMEOUT, "provider did not end after EOF");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn message_id(response: &Value) -> String {
    response["result"]["messageId"]
        .as_str()
        .unwrap_or_else(|| panic!("no messageId in {response}"))
        .to_owned()
}

fn wait_pid(path: &PathBuf) -> i32 {
    let start = Instant::now();
    loop {
        if let Some(pid) = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            return pid;
        }
        assert!(
            start.elapsed() < TIMEOUT,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_dies(pid: i32) {
    let start = Instant::now();
    loop {
        let alive = std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            !stat
                .rsplit_once(") ")
                .is_some_and(|(_, r)| r.starts_with('Z'))
        });
        if !alive {
            return;
        }
        assert!(
            start.elapsed() < TIMEOUT,
            "process {pid} survived settlement"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn describe_advertises_resident_session_only_when_selected() {
    let registry = agent_provider_contract::SchemaRegistry::new();
    let f = Fixture::new();
    let selected = f.invoke("describe", f.host(true), json!({}));
    assert_eq!(
        selected["result"]["capabilities"]["resident_session_v1"],
        json!(true)
    );
    let unselected = f.invoke("describe", f.host(false), json!({}));
    assert!(unselected["result"]["capabilities"]
        .get("resident_session_v1")
        .is_none());
    // A newer host offer alone selects nothing this provider supports.
    let mut newer = f.host(false);
    newer["env"]["OULIPOLY_HOST_RESIDENT_SESSION_V2"] = json!("1");
    let newer = f.invoke("describe", newer, json!({}));
    assert!(newer["result"]["capabilities"]
        .get("resident_session_v2")
        .is_none());
    assert!(newer["result"]["capabilities"]
        .get("resident_session_v1")
        .is_none());
    // Host side: the selected capability yields version 1.
    let capabilities = selected["result"]["capabilities"].as_object().unwrap();
    assert_eq!(extension::select(&[1], capabilities), Ok(1));
    let mut future = selected.clone();
    future["result"]["contract_versions"] = json!(["oulipoly.provider/v2", "oulipoly.provider/v1"]);
    future["result"]["preferred_contract"] = json!("oulipoly.provider/v2");
    future["result"]["capabilities"]["resident_session_v2"] = json!(true);
    future["result"]["capabilities"]["future_capability"] = json!({"new_shape":42});
    let admitted = registry
        .decode_response::<agent_provider_contract::operations::Describe>(
            &serde_json::to_vec(&future).unwrap(),
        )
        .unwrap();
    let advertised = &admitted.value().result;
    assert_eq!(
        agent_provider_contract::negotiation::select_contract_version(
            &["oulipoly.provider/v1"],
            &advertised.contract_versions,
            &advertised.preferred_contract
        ),
        Ok("oulipoly.provider/v1".into())
    );
    let caps = serde_json::to_value(&advertised.capabilities).unwrap();
    assert_eq!(extension::select(&[1], caps.as_object().unwrap()), Ok(1));
    future["result"]["capabilities"]["resident_session_v1"] = json!("true");
    assert!(registry
        .decode_response::<agent_provider_contract::operations::Describe>(
            &serde_json::to_vec(&future).unwrap()
        )
        .is_err());
    future["result"]["capabilities"]["resident_session_v1"] = json!(true);
    future["result"]["preferred_contract"] = json!("oulipoly.provider/v3");
    assert!(registry
        .decode_response::<agent_provider_contract::operations::Describe>(
            &serde_json::to_vec(&future).unwrap()
        )
        .is_err());
}

#[test]
fn prepare_records_a_checked_template_and_names_only_arguments() {
    let f = Fixture::new();
    let prepared = f.prepare();
    let result = extension::decode_prepare_result(&prepared).unwrap();
    assert_eq!(result.invocation.args[0], "resident.serve");
    assert_eq!(result.invocation.endpoint, "stdio");
    let config = PathBuf::from(&result.invocation.args[2]);
    assert!(config.starts_with(f.path().join("data/provider-state/codex/resident/configs")));
    let bytes = std::fs::read(&config).unwrap();
    assert_eq!(
        agent_runner_codex::encoding::sha256_hex(&bytes),
        result.config_sha256
    );
    // Idempotent: the same template names the same record.
    assert_eq!(f.prepare(), prepared);
    // Unselected hosts, unmanaged argv and private fields are refused.
    let unselected = f.invoke(
        "resident.prepare",
        f.host(false),
        json!({"protocol":"oulipoly.resident_session/v1","launch":f.template()}),
    );
    assert_eq!(
        unselected["error"]["code"],
        json!("resident_session_not_selected")
    );
    let mut template = f.template();
    template["argv"] = json!(["codex2", "exec", "--yolo"]);
    let unmanaged = f.invoke(
        "resident.prepare",
        f.host(true),
        json!({"protocol":"oulipoly.resident_session/v1","launch":template}),
    );
    assert_eq!(
        unmanaged["error"]["code"],
        json!("unmanaged_argv"),
        "{unmanaged}"
    );
    let mut template = f.template();
    template["working_directory"] = json!("/tmp");
    let extra = f.invoke(
        "resident.prepare",
        f.host(true),
        json!({"protocol":"oulipoly.resident_session/v1","launch":template}),
    );
    assert_eq!(extra["error"]["code"], json!("invalid_resident_prepare"));
    assert!(f.calls().is_empty(), "prepare runs no native command");
}

#[test]
fn resident_turns_run_codex_exec_and_resume_the_thread() {
    let f = Fixture::new();
    let prepared = f.prepare();
    let mut client = f.serve(&prepared);
    let init = client.call(
        "initialize",
        json!({"protocolVersion":2,"info":{"name":"t","version":"0"}}),
    );
    assert_eq!(init["result"]["protocolVersion"], json!(2));
    assert_eq!(init["result"]["info"]["name"], json!("agent-runner-codex"));
    let cwd = f.path().join("work");
    let session = client.call("session/new", json!({"cwd":cwd}))["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = client.prompt(&session, "hello", Some("k1"));
    let first = message_id(&client.response(first));
    let text = client.update("agent message", |u| {
        u["sessionUpdate"] == json!("agent_message")
    });
    assert_eq!(text["_meta"]["oulipoly.ai/parentMessageId"], json!(first));
    let said = text["content"][0]["text"].as_str().unwrap().to_owned();
    assert!(said.starts_with("reply to hello on "), "{said}");
    let thread = said.rsplit(' ').next().unwrap().to_owned();
    let idle = client.idle_for(&first);
    assert_eq!(idle["stopReason"], json!("end_turn"));
    let native = &idle["_meta"]["oulipoly.ai/nativeTurn"];
    assert_eq!(native["status"], json!({"kind":"exited","code":0}));
    assert_eq!(native["launch_output"]["data_event_count"], json!(1));

    let second = client.prompt(&session, "again", Some("k2"));
    let second = message_id(&client.response(second));
    assert!(second > first);
    let text = client.update("second message", |u| {
        u["sessionUpdate"] == json!("agent_message")
    });
    assert_eq!(
        text["content"][0]["text"],
        json!(format!("reply to again on {thread}"))
    );
    client.idle_for(&second);

    // A resent key inserts nothing and runs no native turn.
    let again = client.prompt(&session, "hello", Some("k1"));
    let again = client.response(again);
    assert_eq!(message_id(&again), first);
    assert_eq!(
        again["result"]["_meta"]["oulipoly.ai/duplicate"],
        json!(true)
    );
    client.idle_for(&first);

    let calls = f.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    let argv =
        |call: &Value| -> Vec<String> { serde_json::from_value(call["argv"].clone()).unwrap() };
    assert!(!argv(&calls[0]).contains(&"resume".to_owned()));
    assert_eq!(argv(&calls[0])[0], "exec");
    assert!(argv(&calls[0]).contains(&"--json".to_owned()));
    let resumed = argv(&calls[1]);
    let at = resumed
        .iter()
        .position(|a| a == "resume")
        .expect("second turn resumes");
    assert_eq!(resumed[at + 1], thread);
    assert_eq!(calls[0]["prompt"], json!("hello"));
    assert!(calls[0]["home"].as_str().unwrap().ends_with(".codex2"));
    assert!(client.end().success());
}

#[test]
fn cancel_and_unconsumed_turns_keep_their_meaning() {
    let f = Fixture::new();
    let prepared = f.prepare();
    let mut client = f.serve(&prepared);
    client.call(
        "initialize",
        json!({"protocolVersion":2,"info":{"name":"t","version":"0"}}),
    );
    let cwd = f.path().join("work");
    let session = client.call("session/new", json!({"cwd":cwd}))["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let refused = client.prompt(&session, "noconsume", None);
    let refused = client.response(refused);
    assert_eq!(refused["error"]["code"], json!(-32010), "{refused}");

    let marks = f.path().join("marks");
    std::fs::create_dir(&marks).unwrap();
    let hang = client.prompt(&session, &format!("hang {}", marks.display()), None);
    let hang = message_id(&client.response(hang));
    let descendant = wait_pid(&marks.join("descendant.pid"));
    client.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}));
    let idle = client.idle_for(&hang);
    assert_eq!(idle["stopReason"], json!("cancelled"));
    assert_eq!(
        idle["_meta"]["oulipoly.ai/nativeTurn"]["status"]["kind"],
        json!("cancelled")
    );
    assert_dies(descendant);

    let failed = client.prompt(&session, "fail", None);
    let failed = message_id(&client.response(failed));
    let idle = client.idle_for(&failed);
    assert_eq!(idle["stopReason"], json!("_oulipoly_native_failed"));
}

#[test]
fn a_changed_configuration_record_is_refused() {
    let f = Fixture::new();
    let prepared = f.prepare();
    let path = PathBuf::from(prepared["invocation"]["args"][2].as_str().unwrap());
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(b' ');
    std::fs::write(&path, bytes).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .args(["resident.serve", "--config"])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not match its digest"));
}

/// A client with one new session over `f`'s resident template.
fn open_session(f: &Fixture) -> (Client, String) {
    let prepared = f.prepare();
    let mut client = f.serve(&prepared);
    client.call(
        "initialize",
        json!({"protocolVersion":2,"info":{"name":"t","version":"0"}}),
    );
    let cwd = f.path().join("work");
    let session = client.call("session/new", json!({"cwd":cwd}))["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    (client, session)
}

/// Native work that ran and failed before reporting any thread leaves its
/// session identity unknown. The provider must not invent one or let the
/// session run another turn, because the first one may have had effects.
#[test]
fn a_native_failure_before_any_thread_identity_blocks_later_input() {
    let f = Fixture::new();
    let (mut client, session) = open_session(&f);
    let first = client.prompt(&session, "preident", Some("k1"));
    let first = client.response(first);
    assert_eq!(first["error"]["code"], json!(-32010), "{first}");
    let native = &first["error"]["data"]["nativeTurn"];
    // The native program ran and exited; the result must not say it never started.
    assert_eq!(
        native["status"],
        json!({"kind":"exited","code":1}),
        "{first}"
    );
    assert_eq!(native["terminal_signal"]["kind"], json!("nonzero_exit"));
    assert_eq!(f.calls().len(), 1);

    let second = client.prompt(&session, "hello", Some("k2"));
    let second = client.response(second);
    assert_eq!(second["error"]["code"], json!(-32012), "{second}");
    assert!(
        second["error"]["message"]
            .as_str()
            .unwrap()
            .contains("native session identity is uncertain"),
        "{second}"
    );
    assert_eq!(
        f.calls().len(),
        1,
        "a blocked input must not rerun native work"
    );
}

/// A native program the lifecycle observed never starting is not unknown work:
/// the result says so in the contract's `spawn_error` terms, and the session
/// stays usable.
#[test]
fn a_native_program_that_never_starts_is_reported_so_and_does_not_block() {
    let f = Fixture::with_codex("#!/nonexistent/interpreter\n");
    let (mut client, session) = open_session(&f);
    for (text, key) in [("hello", "k1"), ("again", "k2")] {
        let turn = client.prompt(&session, text, Some(key));
        let turn = client.response(turn);
        // The second input is accepted for a new attempt, not blocked (-32012).
        assert_eq!(turn["error"]["code"], json!(-32010), "{turn}");
        let native = &turn["error"]["data"]["nativeTurn"];
        assert_eq!(native["status"]["kind"], json!("spawn_error"), "{turn}");
        assert_eq!(native["terminal_signal"]["kind"], json!("spawn_error"));
        assert_eq!(native["custody"], json!("complete"));
    }
    assert!(f.calls().is_empty(), "the fake program never ran");
}
