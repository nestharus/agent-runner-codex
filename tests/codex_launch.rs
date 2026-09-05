use agent_runner_codex::{dispatch::write_invocation, encoding::encode_base64};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn file(path: &Path, text: &str) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, text).unwrap();
}
fn executable(path: &Path, text: &str) {
    file(path, text);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
struct Fixture {
    root: tempfile::TempDir,
    request: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        let config = r.join("config/agent-runner-codex");
        fs::create_dir_all(&config).unwrap();
        let codex = r.join("codex");
        executable(
            &codex,
            r#"#!/usr/bin/env python3
import os,sys,json
if sys.argv[1:] == ['--version']:
 print('codex-cli 0.153.4');sys.exit(0)
with open(os.environ['CALLS'],'a') as f: f.write(json.dumps({'argv':sys.argv[1:],'home':os.environ.get('CODEX_HOME'),'kept':os.environ.get('CUSTOM_SENTINEL'),'stdin':sys.stdin.read()})+'\n')
for event in [{'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'},{'type':'turn.started'},{'type':'item.completed','item':{'type':'agent_message','text':'native-ok'}},{'type':'turn.completed','usage':{}}]: print(json.dumps(event),flush=True)
"#,
        );
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
        file(&config.join("config.toml"),&format!("codex_bin = {:?}\nbun_bin = {:?}\nbash_mcp_path = {:?}\nsystem_prompt_file = {:?}\nagent_bash_bin = {:?}\nagent_runner_bin = {:?}\n",codex,bash,mcp,prompt,bash,bash));
        let request = json!({"contract":"oulipoly.provider/v1","request_id":"launch-fixture","provider_instance_id":"codex2",
            "host":{"app":"test","config_root":r.join("config"),"data_root":r.join("data"),"env":{"HOME":r,"CUSTOM_SENTINEL":"inherited","CALLS":r.join("calls.jsonl")}},
            "params":{"settings_id":"codex2","mode":"arg","model":{"name":"codex-gpt-high","provider_args":["-m","gpt-6-astra","-c","model_reasoning_effort=\"high\""],"inputs":{"prompt":"test prompt","named":{}}},
                "argv":["codex2","exec","--dangerously-bypass-approvals-and-sandbox","-m","gpt-6-astra","-c","model_reasoning_effort=\"high\""],"working_directory":r,"env":{}}});
        Self { root, request }
    }
    fn invoke(&self, operation: &str, request: &Value) -> (i32, Vec<Value>) {
        let mut output = Vec::new();
        let code = write_invocation(
            &["agent-runner-codex".into(), operation.into()],
            &serde_json::to_vec(request).unwrap(),
            &mut output,
        );
        (
            code,
            String::from_utf8(output)
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
        )
    }
}
#[test]
fn standard_labels_launch_the_same_astra_configuration_as_temporary_aliases() {
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        let f = Fixture::new();
        let model_args = json!([
            "-m",
            "gpt-6-astra",
            "-c",
            format!("model_reasoning_effort=\"{effort}\"")
        ]);
        for prefix in ["codex-gpt-", "gpt-"] {
            let mut request = f.request.clone();
            request["request_id"] = json!(format!("{prefix}{effort}"));
            request["params"]["model"]["name"] = json!(format!("{prefix}{effort}"));
            request["params"]["model"]["provider_args"] = model_args.clone();
            let mut argv = vec![
                json!("codex2"),
                json!("exec"),
                json!("--dangerously-bypass-approvals-and-sandbox"),
            ];
            argv.extend(model_args.as_array().unwrap().iter().cloned());
            request["params"]["argv"] = json!(argv);
            let (code, events) = f.invoke("launch", &request);
            assert_eq!(code, 0, "{prefix}{effort}: {events:?}");
        }
        let calls: Vec<Value> = fs::read_to_string(f.root.path().join("calls.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0], calls[1],
            "Native configuration changed between aliases for {effort}"
        );
    }
}

#[test]
fn native_launch_preserves_environment_pins_account_and_publishes_real_completion() {
    let f = Fixture::new();
    let (code, events) = f.invoke("launch", &f.request);
    assert_eq!(code, 0, "{events:?}");
    let calls: Value = serde_json::from_str(
        fs::read_to_string(f.root.path().join("calls.jsonl"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(calls["home"], json!(f.root.path().join(".codex2")));
    assert_eq!(calls["kept"], "inherited");
    assert_eq!(calls["stdin"], "test prompt");
    let argv = calls["argv"].as_array().unwrap();
    assert!(argv.iter().any(|v| v == "features.shell_tool=false"));
    assert!(argv.iter().any(|v| v == "features.multi_agent=false"));
    assert!(argv.iter().any(|v| v == "features.plugins=false"));
    assert!(argv.iter().any(|v| v == "features.remote_plugin=false"));
    assert!(argv.iter().any(|v| v
        .as_str()
        .is_some_and(|v| v.starts_with("model_instructions_file="))));
    assert!(events
        .iter()
        .any(|e| e["name"] == "oulipoly.provider_session"));
    assert!(events
        .iter()
        .any(|e| e["data_base64"] == encode_base64(b"native-ok\n")));
    assert!(events
        .iter()
        .any(|e| e["name"] == "oulipoly.produced_assistant_response"));
    assert_eq!(events.last().unwrap()["kind"], "exit");
}
#[test]
fn exact_retry_replays_without_a_second_native_turn_and_changed_retry_fails() {
    let f = Fixture::new();
    let first = f.invoke("launch", &f.request);
    let second = f.invoke("launch", &f.request);
    assert_eq!(first, second);
    assert_eq!(
        fs::read_to_string(f.root.path().join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let mut changed = f.request.clone();
    changed["params"]["model"]["inputs"]["prompt"] = json!("changed");
    let (code, events) = f.invoke("launch", &changed);
    assert_ne!(code, 0);
    assert_eq!(events[0]["error"]["code"], "request_changed");
}
#[test]
fn policy_rejects_route_overrides_before_spawn() {
    let f = Fixture::new();
    let mut request = f.request.clone();
    request["params"]["argv"]
        .as_array_mut()
        .unwrap()
        .extend([json!("-c"), json!("features.shell_tool=true")]);
    let (code, events) = f.invoke("launch", &request);
    assert_ne!(code, 0);
    assert_eq!(events[0]["error"]["code"], "unmanaged_argv");
    assert!(!f.root.path().join("calls.jsonl").exists());
}
#[test]
fn missing_instruction_file_is_rejected_before_spawn() {
    let f = Fixture::new();
    fs::remove_file(f.root.path().join("ai/AGENTS.md")).unwrap();
    let (code, events) = f.invoke("launch", &f.request);
    assert_ne!(code, 0);
    assert_eq!(events[0]["error"]["code"], "runtime_dependency_missing");
}
#[test]
fn native_success_without_turn_completed_does_not_report_assistant_completion() {
    let f = Fixture::new();
    let path = f.root.path().join("codex");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace(",{'type':'turn.completed','usage':{}}", "");
    file(&path, &text);
    let (code, events) = f.invoke("launch", &f.request);
    assert_ne!(code, 0);
    assert!(!events
        .iter()
        .any(|e| e["name"] == "oulipoly.produced_assistant_response"));
}

fn fake_native(f: &Fixture, body: &str) {
    executable(&f.root.path().join("codex"), &format!("#!/usr/bin/env python3\nimport os,sys,json,time\nif sys.argv[1:] == ['--version']:\n print('codex-cli 0.153.4');sys.exit(0)\n{body}\n"));
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[test]
fn replay_is_bound_to_explicit_environment_and_survives_missing_runtime_files() {
    let f = Fixture::new();
    let first = f.invoke("launch", &f.request);
    assert_eq!(first.0, 0);
    let mut changed = f.request.clone();
    changed["host"]["env"]["HOME"] = json!(f.root.path().join("other-user"));
    let rejected = f.invoke("launch", &changed);
    assert_eq!(rejected.1[0]["error"]["code"], "request_changed");
    fs::remove_file(f.root.path().join("ai/AGENTS.md")).unwrap();
    assert_eq!(first, f.invoke("launch", &f.request));
}

#[test]
fn corrupted_completed_journal_is_rejected_without_rerunning_native_work() {
    let f = Fixture::new();
    assert_eq!(f.invoke("launch", &f.request).0, 0);
    let journal = fs::read_dir(f.root.path().join("data/provider-state/codex/launch"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|s| s == "jsonl"))
        .unwrap();
    fs::write(journal, b"corrupt receipt\n").unwrap();
    let rejected = f.invoke("launch", &f.request);
    assert_eq!(rejected.1[0]["error"]["code"], "launch_journal_invalid");
    assert_eq!(
        fs::read_to_string(f.root.path().join("calls.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn version_probe_honors_deadline_without_launching_a_turn() {
    let f = Fixture::new();
    executable(
        &f.root.path().join("codex"),
        "#!/usr/bin/env python3\nimport time\ntime.sleep(30)\n",
    );
    let mut request = f.request.clone();
    request["host"]["deadline_unix_ms"] = json!(now_ms() + 300);
    let started = std::time::Instant::now();
    let result = f.invoke("launch", &request);
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(result.1[0]["error"]["code"], "launch_deadline");
    assert!(!f.root.path().join("calls.jsonl").exists());
}

#[test]
fn closed_native_streams_do_not_bypass_lifecycle_bounds() {
    let f = Fixture::new();
    fake_native(
        &f,
        "sys.stdin.read()\nos.close(1)\nos.close(2)\ntime.sleep(30)",
    );
    let started = std::time::Instant::now();
    let result = f.invoke("launch", &f.request);
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert_ne!(result.0, 0);
    assert_eq!(
        result.1.last().unwrap()["error"]["code"],
        "native_streams_closed"
    );
}

#[test]
fn deadline_emits_replayable_cancelled_exit_and_reaps_native_process() {
    let f = Fixture::new();
    fake_native(&f,"sys.stdin.read()\nopen(os.environ['CALLS'],'w').write(str(os.getpid()))\nprint(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)\ntime.sleep(30)");
    let mut request = f.request.clone();
    request["host"]["deadline_unix_ms"] = json!(now_ms() + 600);
    let result = f.invoke("launch", &request);
    assert_eq!(result.0, 130);
    assert_eq!(result.1.last().unwrap()["status"]["kind"], "cancelled");
    let pid: i32 = fs::read_to_string(f.root.path().join("calls.jsonl"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(result, f.invoke("launch", &request));
}

#[test]
fn fresh_launch_publishes_actual_private_session_identity_to_agent_bash() {
    let f = Fixture::new();
    fake_native(
        &f,
        r#"sys.stdin.read()
assert 'AGENT_RUNNER_CODEX_SESSION_ID' not in os.environ
print(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)
p=os.environ['AGENT_RUNNER_CODEX_SESSION_FILE']
for _ in range(100):
 if open(p).read().strip(): break
 time.sleep(.02)
assert open(p).read().strip()=='11111111-2222-3333-4444-555555555555'
assert os.stat(p).st_mode & 0o077 == 0
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'session-bound'}}),flush=True)
print(json.dumps({'type':'turn.completed'}),flush=True)"#,
    );
    let mut request = f.request.clone();
    request["host"]["env"]["AGENT_RUNNER_CODEX_SESSION_ID"] = json!("stale-parent-session");
    let result = f.invoke("launch", &request);
    assert_eq!(result.0, 0, "{:?}", result.1);
    assert!(result
        .1
        .iter()
        .any(|e| e["data_base64"] == encode_base64(b"session-bound\n")));
}

#[test]
fn non_utf8_inherited_environment_reaches_native_without_provider_panic() {
    use std::io::Write;
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    fake_native(&f,"sys.stdin.read()\nassert os.environb[b'NON_UTF8_SENTINEL']==b'\\xff\\xfe'\nprint(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)\nprint(json.dumps({'type':'turn.completed'}),flush=True)");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .arg("launch")
        .env(
            "NON_UTF8_SENTINEL",
            std::ffi::OsString::from_vec(vec![255, 254]),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&f.request).unwrap())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn sigterm_cancels_cli_and_reaps_native_process_group() {
    use std::io::Write;
    let f = Fixture::new();
    fake_native(&f,"sys.stdin.read()\nopen(os.environ['CALLS'],'w').write(str(os.getpid()))\nprint(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)\ntime.sleep(30)");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .arg("launch")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&f.request).unwrap())
        .unwrap();
    let started = std::time::Instant::now();
    while !f.root.path().join("calls.jsonl").exists()
        && started.elapsed() < std::time::Duration::from_secs(3)
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(f.root.path().join("calls.jsonl").exists());
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let stopped = std::time::Instant::now();
    while child.try_wait().unwrap().is_none()
        && stopped.elapsed() < std::time::Duration::from_secs(3)
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
        panic!("provider did not terminate after SIGTERM");
    }
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(130));
    let events: Vec<Value> = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.last().unwrap()["status"]["kind"], "cancelled");
    let pid: i32 = fs::read_to_string(f.root.path().join("calls.jsonl"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
}

#[test]
fn model_catalog_cannot_reenable_native_tools() {
    let f = Fixture::new();
    let path = f.root.path().join("models.json");
    let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    catalog["models"][0]["apply_patch_tool_type"] = json!("freeform");
    fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let result = f.invoke("launch", &f.request);
    assert_eq!(
        result.1[0]["error"]["code"],
        "model_catalog_tools_unrestricted"
    );
    assert!(!f.root.path().join("calls.jsonl").exists());
}

fn output_request(f: &Fixture) -> Value {
    let mut request = f.request.clone();
    request["host"]["env"]["OULIPOLY_HOST_LAUNCH_OUTPUT_V1"] = json!("1");
    request["params"]["output_delivery"] = json!({"protocol":"oulipoly.launch_output/v1"});
    request
}

fn assert_complete_output(events: &[Value]) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut count = 0;
    for event in events {
        if let Some(encoded) = event.get("data_base64").and_then(Value::as_str) {
            let bytes = agent_runner_codex::encoding::decode_base64(encoded).unwrap();
            if event["kind"] == "stdout" {
                stdout.extend(bytes)
            } else {
                stderr.extend(bytes)
            };
            count += 1;
        }
    }
    let completions: Vec<_> = events
        .iter()
        .filter(|e| e["name"] == "oulipoly.launch_output_complete/v1")
        .collect();
    assert_eq!(completions.len(), 1);
    let expected = json!({"protocol":"oulipoly.launch_output/v1",
        "stdout":{"bytes":stdout.len(),"sha256":agent_runner_codex::encoding::sha256_hex(&stdout)},
        "stderr":{"bytes":stderr.len(),"sha256":agent_runner_codex::encoding::sha256_hex(&stderr)},"data_event_count":count});
    assert_eq!(completions[0]["value"], expected);
    assert_eq!(
        events[events.len() - 2]["name"],
        "oulipoly.launch_output_complete/v1"
    );
    assert_eq!(events.last().unwrap()["kind"], "exit");
    let schema: Value =
        serde_json::from_str(include_str!("../contract/v1/launch.schema.json")).unwrap();
    let value_schema = json!({"$ref":"https://contract.test/launch.schema.json#/$defs/LaunchOutputCompleteMarkerValueV1"});
    let validator = jsonschema::JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_document("https://contract.test/launch.schema.json".into(), schema)
        .compile(&value_schema)
        .unwrap();
    assert!(validator.is_valid(&completions[0]["value"]));
}

#[test]
fn extension_capabilities_require_exact_host_selection() {
    let f = Fixture::new();
    let legacy = f.invoke("describe", &f.request);
    assert!(legacy.1[0]["result"]["capabilities"]
        .get("launch_output_v1")
        .is_none());
    assert!(legacy.1[0]["result"]["capabilities"]
        .get("session_turn_pages_v1")
        .is_none());
    let mut selected = output_request(&f);
    selected["host"]["env"]["OULIPOLY_HOST_SESSION_TURN_PAGES_V1"] = json!("1");
    let capabilities = f.invoke("describe", &selected).1[0]["result"]["capabilities"].clone();
    assert_eq!(capabilities["launch_output_v1"], true);
    assert_eq!(capabilities["session_turn_pages_v1"], true);
    selected["host"]["env"]["OULIPOLY_HOST_LAUNCH_OUTPUT_V1"] = json!("true");
    assert!(
        f.invoke("describe", &selected).1[0]["result"]["capabilities"]
            .get("launch_output_v1")
            .is_none()
    );
}

#[test]
fn output_protocol_is_validated_before_native_admission() {
    let f = Fixture::new();
    let mut request = output_request(&f);
    request["host"]["env"]
        .as_object_mut()
        .unwrap()
        .remove("OULIPOLY_HOST_LAUNCH_OUTPUT_V1");
    assert_eq!(
        f.invoke("launch", &request).1[0]["error"]["code"],
        "launch_output_not_selected"
    );
    request["host"]["env"]["OULIPOLY_HOST_LAUNCH_OUTPUT_V1"] = json!("1");
    request["params"]["output_delivery"]["protocol"] = json!("oulipoly.launch_output/v2");
    assert_eq!(
        f.invoke("launch", &request).1[0]["error"]["code"],
        "unsupported_launch_output_protocol"
    );
    request["params"]["output_delivery"] =
        json!({"protocol":"oulipoly.launch_output/v1","extra":true});
    assert_eq!(
        f.invoke("launch", &request).1[0]["error"]["code"],
        "invalid_launch_output_request"
    );
    assert!(!f.root.path().join("calls.jsonl").exists());
}

#[test]
fn selected_output_attests_binary_stderr_utf8_stdout_and_exact_replay() {
    let f = Fixture::new();
    let native = f.root.path().join("codex");
    let body = fs::read_to_string(&native).unwrap().replace(
        "for event in [",
        "sys.stderr.buffer.write(b'\\xff\\x00native-warning\\n')\nfor event in [",
    );
    file(&native, &body);
    let request = output_request(&f);
    let first = f.invoke("launch", &request);
    assert_eq!(first.0, 0, "{:?}", first.1);
    assert_complete_output(&first.1);
    assert_eq!(first, f.invoke("launch", &request));
    let marker = first
        .1
        .iter()
        .find(|e| e["name"] == "oulipoly.launch_output_complete/v1")
        .unwrap();
    assert_eq!(marker["value"]["stdout"]["bytes"], 10);
    assert_eq!(marker["value"]["stderr"]["bytes"], 17);
}

#[test]
fn nonzero_and_cancelled_exits_attest_delivered_output_after_pipe_drain() {
    let f = Fixture::new();
    fake_native(
        &f,
        "sys.stdin.read()\nprint('failed',file=sys.stderr,flush=True)\nsys.exit(17)",
    );
    let result = f.invoke("launch", &output_request(&f));
    assert_eq!(result.0, 17);
    assert_complete_output(&result.1);
    let g = Fixture::new();
    fake_native(
        &g,
        r#"import signal
sys.stdin.read()
def stop(signum,frame):
 sys.stderr.write('terminated\n');sys.stderr.flush();sys.exit(130)
signal.signal(signal.SIGTERM,stop)
print(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)
time.sleep(30)"#,
    );
    let mut request = output_request(&g);
    request["host"]["deadline_unix_ms"] = json!(now_ms() + 600);
    let result = g.invoke("launch", &request);
    assert_eq!(result.0, 130);
    assert_complete_output(&result.1);
    let marker = result
        .1
        .iter()
        .find(|e| e["name"] == "oulipoly.launch_output_complete/v1")
        .unwrap();
    assert_eq!(marker["value"]["stderr"]["bytes"], 11);
}

#[test]
fn output_exceeding_previous_lifetime_journal_limit_remains_complete() {
    use std::io::Write;
    struct Sink {
        buffer: Vec<u8>,
        summary: Option<Value>,
        exit: Option<Value>,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.buffer.extend_from_slice(bytes);
            while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let event: Value = serde_json::from_slice(&self.buffer[..end]).unwrap();
                if event["name"] == "oulipoly.launch_output_complete/v1" {
                    self.summary = Some(event["value"].clone());
                }
                if event["kind"] == "exit" {
                    self.exit = Some(event);
                }
                self.buffer.drain(..=end);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new();
    fake_native(
        &f,
        r#"sys.stdin.read()
print(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)
for _ in range(49):
 print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'x'*(1024*1024)}}),flush=True)
print(json.dumps({'type':'turn.completed'}),flush=True)"#,
    );
    let mut sink = Sink {
        buffer: Vec::new(),
        summary: None,
        exit: None,
    };
    let code = write_invocation(
        &["agent-runner-codex".into(), "launch".into()],
        &serde_json::to_vec(&output_request(&f)).unwrap(),
        &mut sink,
    );
    assert_eq!(code, 0);
    let summary = sink.summary.unwrap();
    assert_eq!(summary["stdout"]["bytes"], 49 * (1024 * 1024 + 1));
    assert_eq!(summary["data_event_count"], 49);
    assert!(sink.exit.is_some());
}
