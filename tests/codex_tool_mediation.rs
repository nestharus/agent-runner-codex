//! `oulipoly.tool_mediation/v1` through the real provider binary: a fake
//! `codex exec --json` that starts its managed `agent_bash` MCP server from
//! the `-c mcp_servers.agent_bash.*` overrides (forwarding only the
//! environment variables `env_vars` names, as Codex does) and calls its
//! `bash` tool, a requester stand-in speaking the root Bash ingress wire, and
//! a stand-in root ingress socket that records each request and its peer.
//! Fakes establish plumbing only: whether a real Codex honours the
//! constructed configuration is not shown here.
#![cfg(target_os = "linux")]

use agent_runner_codex::dispatch::write_invocation;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
const INGRESS_ENV: &str = "OULIPOLY_ROOT_BASH_V1";

/// Fake `codex exec --json`. A prompt `bash <command>` starts the managed
/// `agent_bash` server and calls its `bash` tool with that command.
const FAKE_CODEX: &str = r#"#!/usr/bin/env python3
import json, os, subprocess, sys, uuid
args = sys.argv[1:]
prompt = sys.stdin.read().strip()
overrides = {}
for i, arg in enumerate(args):
    if arg == '-c' and i + 1 < len(args):
        key, _, value = args[i + 1].partition('=')
        overrides[key] = value
thread = args[args.index('resume') + 1] if 'resume' in args else str(uuid.uuid4())
if 'resume' not in args:
    sessions = os.path.join(os.environ['CODEX_HOME'], 'sessions')
    os.makedirs(sessions, exist_ok=True)
    with open(os.path.join(sessions, 'rollout-%s.jsonl' % thread), 'a') as rollout:
        rollout.write(json.dumps({'timestamp': '2026-10-06T00:00:00Z', 'type': 'session_meta', 'payload': {'id': thread, 'cwd': os.getcwd()}}) + '\n')
def emit(event):
    print(json.dumps(event), flush=True)
emit({'type': 'thread.started', 'thread_id': thread})
emit({'type': 'turn.started'})
record = {'pid': os.getpid(), 'shell_tool': overrides.get('features.shell_tool'), 'servers': sorted({k.split('.')[1] for k in overrides if k.startswith('mcp_servers.')})}
said = 'no tool'
words = prompt.split(' ', 1)
if words[0] == 'bash':
    command = json.loads(overrides['mcp_servers.agent_bash.command'])
    server_args = json.loads(overrides['mcp_servers.agent_bash.args'])
    names = json.loads(overrides['mcp_servers.agent_bash.env_vars'])
    record['command'] = [command] + server_args
    record['enabled_tools'] = json.loads(overrides['mcp_servers.agent_bash.enabled_tools'])
    env = {name: os.environ[name] for name in names if name in os.environ}
    mcp = subprocess.Popen([command] + server_args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, text=True)
    def rpc(id, method, params=None):
        mcp.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': id, 'method': method, 'params': params or {}}) + '\n'); mcp.stdin.flush()
        return json.loads(mcp.stdout.readline())
    rpc(1, 'initialize', {'protocolVersion': '2025-06-18'})
    record['tools'] = [t['name'] for t in rpc(2, 'tools/list')['result']['tools']]
    record['mcp_pid'] = mcp.pid
    with open(os.environ['CALLS'], 'a') as f:
        f.write(json.dumps(record) + '\n')
    said = rpc(3, 'tools/call', {'name': 'bash', 'arguments': {'command': words[1]}})['result']['content'][0]['text']
    mcp.stdin.close(); mcp.wait()
else:
    with open(os.environ['CALLS'], 'a') as f:
        f.write(json.dumps(record) + '\n')
emit({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': said}})
emit({'type': 'turn.completed', 'usage': {}})
"#;

/// Requester stand-in: the root ingress wire of `agent-bash run`, reduced to
/// what the bridge reads (one `agent-bash-root-v1` result object).
const REQUESTER: &str = r#"#!/usr/bin/env python3
import base64, json, os, socket, sys
args = sys.argv[1:]
assert args[:2] == ['run', '--delivery'] and args[3] == '--', args
argv = args[4:]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(os.environ['OULIPOLY_ROOT_BASH_V1'])
s.sendall((json.dumps({'v': 1, 'op': 'run', 'argv': argv, 'cwd': os.getcwd()}) + '\n').encode())
stages, out, end = [], b'', None
for line in s.makefile('rb'):
    event = json.loads(line)
    stages.append({'event': event['event']})
    if event['event'] == 'output':
        out += base64.b64decode(event['b64'])
    if event['event'] in ('end', 'refused'):
        end = event; break
if end is None:
    print(json.dumps({'result_surface': 'agent-bash-root-v1', 'version': 1, 'delivery_mode': 'sync', 'outcome': 'unknown', 'meaning': 'accepted-end-unknown', 'effects_possible': True, 'stages': stages, 'faults': [], 'wait': None, 'output': {'base64': '', 'bytes': 0, 'delivery': 'unproven'}}))
elif end['event'] == 'refused':
    print(json.dumps({'result_surface': 'agent-bash-root-v1', 'version': 1, 'delivery_mode': 'sync', 'outcome': 'refused', 'effects_possible': False, 'refusal': {'by': 'owner', 'reason': end['reason']}, 'stages': stages, 'faults': [], 'wait': None, 'output': {'base64': '', 'bytes': 0, 'delivery': 'none'}}))
else:
    print(json.dumps({'result_surface': 'agent-bash-root-v1', 'version': 1, 'delivery_mode': 'sync', 'outcome': 'ended', 'effects_possible': True, 'stages': stages, 'faults': [], 'wait': {'status': end['status'], 'observer': 'work-pid1-wait', 'exit': {'code': int(end['status'].split(':')[1])}}, 'output': {'base64': base64.b64encode(out).decode(), 'bytes': len(out), 'delivery': 'complete'}}))
"#;

/// One request the stand-in ingress received.
#[derive(Debug, Clone)]
struct Seen {
    request: Value,
    peer: i32,
    /// The peer's ancestors when it connected, nearest first.
    ancestry: Vec<i32>,
    eof_while_running: bool,
}

/// Stand-in root Bash ingress: answers `hang` with accepted/started and then
/// waits for its requester to go away; anything else ends with code 0 and
/// output naming the command and directory.
struct Ingress {
    path: PathBuf,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn peer_pid(stream: &UnixStream) -> i32 {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid socket descriptor and an out-buffer of the stated size.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    assert_eq!(rc, 0);
    cred.pid
}

impl Ingress {
    fn start(dir: &Path) -> Self {
        let path = dir.join("bash.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let record = Arc::clone(&record);
                std::thread::spawn(move || {
                    let peer = peer_pid(&stream);
                    let ancestry = ancestors(peer);
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let index = {
                        let mut seen = record.lock().unwrap();
                        seen.push(Seen {
                            request: request.clone(),
                            peer,
                            ancestry,
                            eof_while_running: false,
                        });
                        seen.len() - 1
                    };
                    let command = request["argv"][2].as_str().unwrap_or_default().to_owned();
                    let mut say = |event: Value| {
                        let _ = writeln!(stream, "{event}");
                    };
                    say(
                        json!({"event":"accepted","root_id":"stand-in","work":index + 1,"durable":true}),
                    );
                    say(json!({"event":"started","work":index + 1}));
                    if command == "hang" {
                        let mut rest = String::new();
                        let _ = reader.read_line(&mut rest);
                        record.lock().unwrap()[index].eof_while_running = true;
                        return;
                    }
                    let output = format!("ran {command} in {}\n", request["cwd"].as_str().unwrap());
                    say(
                        json!({"event":"output","b64":agent_provider_execution::encoding::encode_base64(output.as_bytes())}),
                    );
                    say(json!({"event":"output-closed","bytes":output.len()}));
                    say(
                        json!({"event":"end","status":"code:0","observer":"work-pid1-wait","output":{"state":"closed"}}),
                    );
                });
            }
        });
        Self { path, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// The parent chain of `pid`, nearest first.
fn ancestors(pid: i32) -> Vec<i32> {
    let mut chain = Vec::new();
    let mut pid = pid;
    while pid > 1 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let Some((_, after)) = stat.rsplit_once(')') else {
            break;
        };
        pid = after
            .split_whitespace()
            .nth(1)
            .and_then(|ppid| ppid.parse().ok())
            .unwrap_or(0);
        chain.push(pid);
    }
    chain
}

fn alive(pid: i64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z "))
}

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
        let root = tempfile::Builder::new()
            .prefix("u94-correction-codex-mediation-")
            .tempdir_in("/tmp")
            .unwrap();
        let r = root.path();
        let codex = r.join("codex");
        executable(&codex, FAKE_CODEX);
        executable(&r.join("requester"), REQUESTER);
        let bash = r.join("bash");
        executable(&bash, "#!/bin/sh\nexit 0\n");
        let mcp = r.join("mcp.ts");
        file(&mcp, "// fixture: never started under mediation\n");
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

    fn policy(&self, bash: Value) -> String {
        json!({"protocol":"oulipoly.tool_mediation/v1","bash":bash,
            "requester":self.path().join("requester"),"ingress_env":INGRESS_ENV})
        .to_string()
    }

    fn host(&self) -> Value {
        let r = self.path();
        json!({"app":"test","config_root":r.join("config"),"data_root":r.join("data"),
            "env":{"HOME": r, "CALLS": r.join("calls.jsonl"),
                "OULIPOLY_HOST_RESIDENT_SESSION_V1":"1","OULIPOLY_HOST_TOOL_MEDIATION_V1":"1"}})
    }

    fn template(&self, policy: Option<String>) -> Value {
        let mut env = json!({});
        if let Some(policy) = policy {
            env["OULIPOLY_TOOL_MEDIATION_V1"] = json!(policy);
        }
        json!({"settings_id":"codex2","mode":"stdin",
            "model":{"name":"gpt-astra-high","provider_args":["-m","gpt-6-astra","-c","model_reasoning_effort=\"high\""],"inputs":{"named":{}}},
            "argv":["codex2","exec","--dangerously-bypass-approvals-and-sandbox","-m","gpt-6-astra","-c","model_reasoning_effort=\"high\""],
            "env":env})
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

    fn prepare(&self, template: Value) -> Value {
        self.invoke(
            "resident.prepare",
            self.host(),
            json!({"protocol":"oulipoly.resident_session/v1","launch":template}),
        )
    }

    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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
    fn serve(prepared: &Value, ingress: Option<&Path>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"));
        for arg in prepared["invocation"]["args"].as_array().unwrap() {
            command.arg(arg.as_str().unwrap());
        }
        command.env_remove(INGRESS_ENV);
        if let Some(ingress) = ingress {
            command.env(INGRESS_ENV, ingress);
        }
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

    fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        stdin.flush().unwrap();
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

    fn notify(&mut self, method: &str, params: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":method,"params":params})
        )
        .unwrap();
        stdin.flush().unwrap();
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

    fn open(&mut self, cwd: &Path) -> String {
        self.call(
            "initialize",
            json!({"protocolVersion":2,"info":{"name":"t","version":"0"}}),
        );
        self.call("session/new", json!({"cwd":cwd}))["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn prompt(&mut self, session: &str, text: &str) -> u64 {
        self.request(
            "session/prompt",
            json!({"sessionId":session,"prompt":[{"type":"text","text":text}]}),
        )
    }

    /// The agent's text for one prompt, after its ACK.
    fn said(&mut self, session: &str, text: &str) -> String {
        let id = self.prompt(session, text);
        let ack = self.response(id);
        let input = ack["result"]["messageId"]
            .as_str()
            .unwrap_or_else(|| panic!("no ACK: {ack}"))
            .to_owned();
        let message = self.update("agent message", move |u| {
            u["sessionUpdate"] == json!("agent_message")
                && u["_meta"]["oulipoly.ai/parentMessageId"] == json!(input)
        });
        message["content"][0]["text"].as_str().unwrap().to_owned()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn describe_and_policy_report_the_mediated_codex_tool() {
    let f = Fixture::new();
    let described = f.invoke("describe", f.host(), json!({}));
    assert_eq!(
        described["result"]["capabilities"]["tool_mediation_v1"],
        json!(true)
    );
    let plain = f.invoke("describe", json!({"app":"t"}), json!({}));
    assert!(plain["result"]["capabilities"]
        .get("tool_mediation_v1")
        .is_none());
    let evaluate = |env: Value| {
        let mut template = f.template(None);
        template["env"] = env;
        let mut params = template.clone();
        params.as_object_mut().unwrap().remove("argv");
        params.as_object_mut().unwrap().remove("env");
        params["model"]["inputs"]["prompt"] = json!("hi");
        params["launch"] = json!({"argv": template["argv"], "env": template["env"]});
        f.invoke("policy.evaluate", f.host(), params)["result"].clone()
    };
    let policy = f.policy(json!({"allow":["cargo test"]}));
    let result = evaluate(json!({"OULIPOLY_TOOL_MEDIATION_V1": policy}));
    assert_eq!(result["accepted"], json!(true), "{result}");
    assert_eq!(result["env"]["OULIPOLY_TOOL_MEDIATION_V1"], json!(policy));
    let marker = result["markers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == json!("oulipoly.tool_mediation/v1"))
        .unwrap_or_else(|| panic!("{result}"))
        .clone();
    agent_provider_contract::tool_mediation::validate("EffectiveMediation", &marker["value"])
        .unwrap();
    assert_eq!(
        marker["value"]["native_tools"],
        json!(["mcp__agent_bash__bash"])
    );
    for env in [
        json!({}),
        json!({"OULIPOLY_TOOL_MEDIATION_V1": "{\"protocol\":\"oulipoly.tool_mediation/v1\",\"bash\":{\"authority\":\"root\"}}"}),
    ] {
        let result = evaluate(env.clone());
        assert_eq!(result["accepted"], json!(false), "{env} {result}");
    }
}

#[test]
fn prepare_and_launch_refuse_what_they_cannot_honour() {
    let f = Fixture::new();
    for (template, code) in [
        (f.template(None), "tool_mediation_invalid"),
        (
            f.template(Some(f.policy(json!({"allow":[]})))),
            "tool_mediation_invalid",
        ),
    ] {
        let response = f.prepare(template);
        assert_eq!(response["ok"], json!(false), "{response}");
        assert_eq!(response["error"]["code"], json!(code), "{response}");
    }
    // A one-shot exec launch is mediated the same way; without the ingress
    // it is refused before Codex starts.
    let mut params = f.template(Some(f.policy(json!({"authority":"trusted-task"}))));
    params["model"]["inputs"]["prompt"] = json!("bash true");
    params["working_directory"] = json!(f.path().join("work"));
    let mut host = f.host();
    host["env"].as_object_mut().unwrap().remove(INGRESS_ENV);
    let mut output = Vec::new();
    write_invocation(
        &["agent-runner-codex".into(), "launch".into()],
        &serde_json::to_vec(
            &json!({"contract":"oulipoly.provider/v1","request_id":"req-launch",
            "host":host,"params":params}),
        )
        .unwrap(),
        &mut output,
    );
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("tool_mediation_ingress_unavailable"),
        "{text}"
    );
    assert!(f.calls().is_empty(), "Codex never started");
}

#[test]
fn mediated_bash_reaches_the_root_ingress_with_the_turn_context() {
    let f = Fixture::new();
    let ingress = Ingress::start(f.path());
    let prepared = f.prepare(f.template(Some(f.policy(json!({"authority":"trusted-task"})))));
    assert_eq!(prepared["ok"], json!(true), "{prepared}");
    let mut client = Client::serve(&prepared["result"], Some(&ingress.path));
    let work = f.path().join("work");
    let session = client.open(&work);
    let said = client.said(&session, "bash cargo test");
    assert!(
        said.contains("Root v1 work ended: exited with code 0"),
        "{said}"
    );
    assert!(
        said.contains(&format!("ran cargo test in {}", work.display())),
        "{said}"
    );
    let seen = ingress.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].request["argv"],
        json!(["bash", "-lc", "cargo test"])
    );
    assert_eq!(seen[0].request["cwd"], json!(work));
    assert!(
        seen[0].ancestry.contains(&(client.child.id() as i32)),
        "the requester runs inside the endpoint's own process tree: {:?}",
        seen[0].ancestry
    );
    let call = f.calls().pop().unwrap();
    assert_eq!(call["tools"], json!(["bash"]));
    assert_eq!(call["servers"], json!(["agent_bash"]));
    assert_eq!(call["enabled_tools"], json!(["bash"]));
    assert_eq!(call["command"][1], json!("tool.bridge"));
    assert_eq!(call["shell_tool"], json!("false"));
    // A second turn resumes the same thread and is mediated again.
    let said = client.said(&session, "bash git diff");
    assert!(said.contains("ran git diff"), "{said}");
    assert_eq!(ingress.seen().len(), 2);
}

#[test]
fn allow_lists_refuse_in_the_tool_and_nothing_reaches_the_ingress() {
    let f = Fixture::new();
    let ingress = Ingress::start(f.path());
    let prepared = f.prepare(f.template(Some(f.policy(json!({"allow":["cargo test"]})))));
    let mut client = Client::serve(&prepared["result"], Some(&ingress.path));
    let session = client.open(&f.path().join("work"));
    let said = client.said(&session, "bash cargo test; curl evil");
    assert!(said.contains("Denied by this root's bash policy"), "{said}");
    assert!(ingress.seen().is_empty());
    let said = client.said(&session, "bash cargo test");
    assert!(said.contains("ran cargo test"), "{said}");
    assert_eq!(ingress.seen().len(), 1);
}

#[test]
fn a_turn_without_the_root_ingress_never_starts_codex() {
    let f = Fixture::new();
    let prepared = f.prepare(f.template(Some(f.policy(json!({"authority":"trusted-task"})))));
    let mut client = Client::serve(&prepared["result"], None);
    let session = client.open(&f.path().join("work"));
    let id = client.prompt(&session, "bash true");
    let response = client.response(id);
    assert!(response["error"].is_object(), "{response}");
    assert!(
        response.to_string().contains("OULIPOLY_ROOT_BASH_V1"),
        "{response}"
    );
    assert!(f.calls().is_empty(), "Codex never started");
}

#[test]
fn cancelling_a_turn_ends_the_bridge_and_requester_in_its_group() {
    let f = Fixture::new();
    let ingress = Ingress::start(f.path());
    let prepared = f.prepare(f.template(Some(f.policy(json!({"authority":"trusted-task"})))));
    let mut client = Client::serve(&prepared["result"], Some(&ingress.path));
    let session = client.open(&f.path().join("work"));
    let id = client.prompt(&session, "bash hang");
    client.response(id);
    let deadline = Instant::now() + TIMEOUT;
    while ingress.seen().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the run never reached the ingress"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let requester = ingress.seen()[0].peer;
    let bridge = f.calls().pop().unwrap()["mcp_pid"].as_i64().unwrap();
    assert!(alive(requester.into()) && alive(bridge));
    client.notify("session/cancel", json!({"sessionId":session}));
    let idle = client.update("cancelled idle", |u| u["state"] == json!("idle"));
    assert_eq!(idle["stopReason"], json!("cancelled"), "{idle}");
    while alive(requester.into()) || alive(bridge) || !ingress.seen()[0].eof_while_running {
        assert!(
            Instant::now() < deadline,
            "requester or bridge survived cancel"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn one_shot_request(f: &Fixture, id: &str, mediated: bool) -> Value {
    let mut params = f.template(mediated.then(|| f.policy(json!({"authority":"trusted-task"}))));
    params["prompt"] = json!("hi");
    params["model"]["inputs"]["prompt"] = json!("hi");
    params["working_directory"] = json!(f.path().join("work"));
    let mut host = f.host();
    if !mediated {
        host["env"]
            .as_object_mut()
            .unwrap()
            .remove("OULIPOLY_HOST_TOOL_MEDIATION_V1");
    }
    json!({"contract":"oulipoly.provider/v1", "request_id":id, "host":host, "params":params})
}

fn launch(f: &Fixture, request: &Value, ingress: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"));
    command
        .arg("launch")
        .env_remove(INGRESS_ENV)
        .current_dir(f.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if ingress {
        command.env(INGRESS_ENV, "unused-fake-ingress");
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn mediated_native_starts_drop_old_install_prerequisites_but_keep_real_dependencies() {
    let f = Fixture::new();
    for name in ["bash", "mcp.ts"] {
        std::fs::remove_file(f.path().join(name)).unwrap();
    }
    let prepared = f.prepare(f.template(Some(f.policy(json!({"authority":"trusted-task"})))));
    assert_eq!(prepared["ok"], json!(true), "{prepared}");
    let first = launch(&f, &one_shot_request(&f, "minimal", true), true);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    assert_eq!(f.calls().len(), 1);
    let plain = launch(&f, &one_shot_request(&f, "unselected", false), true);
    assert!(
        !plain.status.success()
            && String::from_utf8_lossy(&plain.stdout).contains("runtime_dependency_missing")
    );
    std::fs::remove_file(f.path().join("models.json")).unwrap();
    let missing_catalog = launch(&f, &one_shot_request(&f, "missing-catalog", true), true);
    assert!(
        !missing_catalog.status.success()
            && String::from_utf8_lossy(&missing_catalog.stdout)
                .contains("model_catalog_unreadable")
    );
    assert_eq!(f.calls().len(), 1);
}

#[test]
fn completed_one_shot_replays_without_ingress_or_current_native_tools() {
    let f = Fixture::new();
    let request = one_shot_request(&f, "tool-free-replay", true);
    let first = launch(&f, &request, true);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    for name in [
        "codex",
        "bash",
        "requester",
        "mcp.ts",
        "models.json",
        "ai/AGENTS.md",
    ] {
        std::fs::remove_file(f.path().join(name)).unwrap();
    }
    let replay = launch(&f, &request, false);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stdout)
    );
    assert_eq!(first.stdout, replay.stdout);
    assert_eq!(f.calls().len(), 1);
    let mut fresh = request.clone();
    fresh["request_id"] = json!("fresh-no-ingress");
    let no_ingress = launch(&f, &fresh, false);
    assert!(
        !no_ingress.status.success()
            && String::from_utf8_lossy(&no_ingress.stdout)
                .contains("tool_mediation_ingress_unavailable")
    );
    fresh["request_id"] = json!("fresh-no-native");
    let no_native = launch(&f, &fresh, true);
    assert!(
        !no_native.status.success()
            && String::from_utf8_lossy(&no_native.stdout).contains("runtime_dependency_missing")
    );
    assert_eq!(f.calls().len(), 1);
}
