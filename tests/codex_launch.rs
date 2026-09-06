mod support;
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
impl Drop for Fixture {
    fn drop(&mut self) {
        support::preserve_fixture(self.root.path());
    }
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
fn astra_request(f: &Fixture, label: &str, effort: &str) -> Value {
    let args = json!([
        "-m",
        "gpt-6-astra",
        "-c",
        format!("model_reasoning_effort=\"{effort}\"")
    ]);
    let mut request = f.request.clone();
    request["request_id"] = json!(label);
    request["params"]["model"]["name"] = json!(label);
    request["params"]["model"]["provider_args"] = args.clone();
    let mut argv = vec![
        json!("codex2"),
        json!("exec"),
        json!("--dangerously-bypass-approvals-and-sandbox"),
    ];
    argv.extend(args.as_array().unwrap().iter().cloned());
    request["params"]["argv"] = json!(argv);
    request["params"]["launch"] = json!({"argv": argv, "env": {}});
    request
}

#[test]
fn astra_standard_and_compatibility_native_argv_are_independently_checked() {
    for (label, effort) in [
        ("gpt-low", "low"),
        ("gpt-medium", "medium"),
        ("gpt-high", "medium"),
        ("gpt-xhigh", "medium"),
        ("gpt-max", "medium"),
        ("codex-gpt-low", "low"),
        ("codex-gpt-medium", "medium"),
        ("codex-gpt-high", "high"),
        ("codex-gpt-xhigh", "xhigh"),
        ("codex-gpt-max", "max"),
    ] {
        assert_astra_launch(label, effort);
    }
}

fn assert_astra_launch(label: &str, effort: &str) {
    let f = Fixture::new();
    let request = astra_request(&f, label, effort);
    let (code, response) = f.invoke("policy.evaluate", &request);
    assert_eq!(code, 0);
    assert_eq!(
        response[0]["result"]["accepted"], true,
        "{label}: {response:?}"
    );
    assert_eq!(
        response[0]["result"]["markers"][0]["value"]["effort"],
        effort
    );
    let (code, events) = f.invoke("launch", &request);
    assert_eq!(code, 0, "{label}: {events:?}");
    let calls: Vec<Value> = fs::read_to_string(f.root.path().join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(calls.len(), 1);
    let argv = calls[0]["argv"].as_array().unwrap();
    assert!(argv
        .windows(2)
        .any(|pair| pair == [json!("-m"), json!("gpt-6-astra")]));
    let native_efforts: Vec<_> = argv
        .iter()
        .filter(|arg| {
            arg.as_str()
                .is_some_and(|s| s.starts_with("model_reasoning_effort="))
        })
        .collect();
    assert_eq!(
        native_efforts,
        [&json!(format!("model_reasoning_effort=\"{effort}\""))],
        "{label}"
    );
}

#[test]
fn astra_stale_standard_efforts_are_rejected_before_spawn() {
    for effort in ["high", "xhigh", "max"] {
        assert_stale_astra_rejected(effort);
    }
}

fn assert_stale_astra_rejected(effort: &str) {
    let f = Fixture::new();
    let label = format!("gpt-{effort}");
    let mut request = astra_request(&f, &label, effort);
    let (_, response) = f.invoke("policy.evaluate", &request);
    assert_eq!(
        response[0]["result"]["accepted"], false,
        "{label}: {response:?}"
    );
    assert_eq!(
        response[0]["result"]["diagnostics"][0]["code"],
        "model_args_mismatch"
    );
    let (code, response) = f.invoke("launch", &request);
    assert_ne!(code, 0);
    assert_eq!(response[0]["error"]["code"], "model_args_mismatch");
    // Matching provider_args must not launder a stale (or appended) argv override.
    request["params"]["model"]["provider_args"] = json!([
        "-m",
        "gpt-6-astra",
        "-c",
        "model_reasoning_effort=\"medium\""
    ]);
    let (_, response) = f.invoke("policy.evaluate", &request);
    assert_eq!(response[0]["result"]["accepted"], false);
    assert_eq!(
        response[0]["result"]["diagnostics"][0]["code"],
        "unmanaged_argv"
    );
    let (code, response) = f.invoke("launch", &request);
    assert_ne!(code, 0);
    assert_eq!(response[0]["error"]["code"], "unmanaged_argv");
    assert!(!f.root.path().join("calls.jsonl").exists());
}

#[test]
fn luna_terra_and_sol_labels_pass_policy_and_launch_with_managed_tools_in_every_account() {
    for (family, model) in [
        ("luna", "gpt-5.6-luna"),
        ("terra", "gpt-5.6-terra"),
        ("sol", "gpt-5.6-sol"),
    ] {
        for account in ["codex", "codex2", "codex3", "codex4", "codex5"] {
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                let f = Fixture::new();
                let mut request = f.request.clone();
                let model_args = json!([
                    "-m",
                    model,
                    "-c",
                    format!("model_reasoning_effort=\"{effort}\"")
                ]);
                request["provider_instance_id"] = json!(account);
                request["params"]["settings_id"] = json!(account);
                request["params"]["model"]["name"] = json!(format!("gpt-{family}-{effort}"));
                request["params"]["model"]["provider_args"] = model_args.clone();
                let mut argv = vec![
                    json!(account),
                    json!("exec"),
                    json!("--dangerously-bypass-approvals-and-sandbox"),
                ];
                argv.extend(model_args.as_array().unwrap().iter().cloned());
                request["params"]["argv"] = json!(argv);
                let mut admission = request.clone();
                admission["params"]["launch"] = json!({"argv": argv, "env": {}});
                let (code, response) = f.invoke("policy.evaluate", &admission);
                assert_eq!(code, 0, "{account}/gpt-{family}-{effort}: {response:?}");
                assert_eq!(response[0]["result"]["accepted"], true, "{response:?}");
                assert_eq!(
                    response[0]["result"]["markers"][0]["value"],
                    json!({"account": account, "model": model, "effort": effort})
                );
                let (code, events) = f.invoke("launch", &request);
                assert_eq!(code, 0, "{account}/gpt-{family}-{effort}: {events:?}");
                let call: Value = serde_json::from_str(
                    fs::read_to_string(f.root.path().join("calls.jsonl"))
                        .unwrap()
                        .trim(),
                )
                .unwrap();
                assert_eq!(
                    call["home"],
                    json!(f.root.path().join(format!(".{account}")))
                );
                let argv = call["argv"].as_array().unwrap();
                assert!(argv
                    .windows(2)
                    .any(|pair| pair == [json!("-m"), json!(model)]));
                assert!(argv.contains(&json!(format!("model_reasoning_effort=\"{effort}\""))));
                assert!(argv.contains(&json!("features.shell_tool=false")));
                assert!(argv.contains(&json!("features.multi_agent=false")));
                assert!(argv.contains(&json!("features.plugins=false")));
                assert!(argv.contains(&json!("features.remote_plugin=false")));
                assert!(argv.iter().any(|value| value
                    .as_str()
                    .is_some_and(|value| value.starts_with("model_instructions_file="))));
                assert!(argv.contains(&json!("mcp_servers.agent_bash.enabled_tools=[\"bash\"]")));
            }
        }
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
fn luna_terra_and_sol_reject_ultra_and_cross_model_or_effort_arguments() {
    for (family, model) in [
        ("luna", "gpt-5.6-luna"),
        ("terra", "gpt-5.6-terra"),
        ("sol", "gpt-5.6-sol"),
    ] {
        for (label_effort, argument_model, argument_effort, expected) in [
            ("ultra", model, "ultra", "unknown_model"),
            ("high", "gpt-6-astra", "high", "model_args_mismatch"),
            ("high", model, "max", "model_args_mismatch"),
        ] {
            let f = Fixture::new();
            let mut request = f.request.clone();
            let args = json!([
                "-m",
                argument_model,
                "-c",
                format!("model_reasoning_effort=\"{argument_effort}\"")
            ]);
            request["params"]["model"]["name"] = json!(format!("gpt-{family}-{label_effort}"));
            request["params"]["model"]["provider_args"] = args.clone();
            let mut argv = vec![
                json!("codex2"),
                json!("exec"),
                json!("--dangerously-bypass-approvals-and-sandbox"),
            ];
            argv.extend(args.as_array().unwrap().iter().cloned());
            request["params"]["argv"] = json!(argv);
            let mut admission = request.clone();
            admission["params"]["launch"] = json!({"argv": argv, "env": {}});
            let (code, response) = f.invoke("policy.evaluate", &admission);
            assert_eq!(code, 0);
            assert_eq!(response[0]["result"]["accepted"], false);
            assert_eq!(response[0]["result"]["diagnostics"][0]["code"], expected);
            let (code, events) = f.invoke("launch", &request);
            assert_ne!(code, 0);
            assert_eq!(events[0]["error"]["code"], expected);
            assert!(!f.root.path().join("calls.jsonl").exists());
        }
    }
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
fn headless_child_drops_inherited_tui_mode_and_uses_its_private_identity_file() {
    use std::io::Write;
    let f = Fixture::new();
    fake_native(
        &f,
        r#"sys.stdin.read()
assert 'AGENT_RUNNER_CODEX_INTERACTIVE' not in os.environ
assert 'AGENT_RUNNER_CODEX_SESSION_BINDING' not in os.environ
assert 'AGENT_RUNNER_CODEX_SESSION_ID' not in os.environ
assert os.environ['AGENT_RUNNER_CODEX_SESSION_FILE'] != '/stale/parent/session'
print(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)
print(json.dumps({'type':'turn.completed'}),flush=True)"#,
    );
    let mut request = f.request.clone();
    // Exercise both the native inherited environment and request overlay.
    for key in [
        "AGENT_RUNNER_CODEX_INTERACTIVE",
        "AGENT_RUNNER_CODEX_SESSION_BINDING",
    ] {
        request["host"]["env"][key] = json!("stale-tui-mode");
    }
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .arg("launch")
        .env("AGENT_RUNNER_CODEX_INTERACTIVE", "1")
        .env("AGENT_RUNNER_CODEX_SESSION_BINDING", "tool_metadata")
        .env(
            "AGENT_RUNNER_CODEX_SESSION_ID",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        )
        .env("AGENT_RUNNER_CODEX_SESSION_FILE", "/stale/parent/session")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn cross_account_child_rebinds_inherited_sqlite_home_to_selected_account() {
    use std::io::Write;
    let f = Fixture::new();
    fake_native(
        &f,
        r#"sys.stdin.read()
expected=os.path.join(os.environ['HOME'],'.codex4')
assert os.environ['CODEX_HOME'] == expected
assert os.environ['CODEX_SQLITE_HOME'] == expected
print(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)
print(json.dumps({'type':'turn.completed'}),flush=True)"#,
    );
    let mut request = f.request.clone();
    request["provider_instance_id"] = json!("codex4");
    request["params"]["settings_id"] = json!("codex4");
    request["params"]["argv"][0] = json!("codex4");
    let parent_home = f.root.path().join(".codex3");
    request["host"]["env"]["CODEX_SQLITE_HOME"] = json!(parent_home);
    request["params"]["env"]["CODEX_SQLITE_HOME"] = json!(parent_home);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .arg("launch")
        .env("CODEX_HOME", &parent_home)
        .env("CODEX_SQLITE_HOME", &parent_home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn child_instructions_reset_survives_runner_policy_environment_merge() {
    use std::io::Write;
    const CARRIER: &str = "AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS";
    for selected in [
        None,
        Some(""),
        Some("  "),
        Some("selected child instructions"),
    ] {
        let f = Fixture::new();
        let expected = selected.filter(|value| !value.trim().is_empty());
        fake_native(
            &f,
            r#"sys.stdin.read()
expected=os.environ.get('EXPECTED_DEVELOPER')
assert os.environ.get('AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS') == expected
pairs=[value for value in sys.argv if value.startswith('developer_instructions=')]
assert pairs == ([] if expected is None else ['developer_instructions='+json.dumps(expected)])
print(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)
print(json.dumps({'type':'turn.completed'}),flush=True)"#,
        );
        let mut request = f.request.clone();
        request["host"]["env"][CARRIER] = json!("stale host instructions");
        let mut admission = request.clone();
        admission["params"]["launch"] =
            json!({"argv":request["params"]["argv"],"env":{CARRIER:"stale incoming carrier"}});
        if let Some(value) = selected {
            admission["params"]["launch"]["system_prompt_override"] = json!(value);
        }
        let (code, response) = f.invoke("policy.evaluate", &admission);
        assert_eq!(code, 0);
        let admitted = &response[0]["result"];
        assert_eq!(admitted["accepted"], true, "{admitted}");
        assert_eq!(admitted["env"][CARRIER], json!(expected.unwrap_or("")));
        // Runner policy_transform merges admitted entries into the original
        // candidate environment. A missing result key must not pass this test.
        let mut merged = admission["params"]["launch"]["env"]
            .as_object()
            .unwrap()
            .clone();
        merged.extend(admitted["env"].as_object().unwrap().clone());
        request["params"]["env"] = json!(merged);
        if let Some(value) = expected {
            request["host"]["env"]["EXPECTED_DEVELOPER"] = json!(value);
        }
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
            .arg("launch")
            .env(CARRIER, "stale process instructions")
            .env_remove("EXPECTED_DEVELOPER")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "selected={selected:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
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
    for slug in [
        "gpt-6-astra",
        "gpt-5.6-luna",
        "gpt-5.6-terra",
        "gpt-5.6-sol",
    ] {
        let f = Fixture::new();
        let path = f.root.path().join("models.json");
        let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let entry = catalog["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry["slug"] == slug)
            .unwrap();
        entry["apply_patch_tool_type"] = json!("freeform");
        fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        let result = f.invoke("launch", &f.request);
        assert_eq!(
            result.1[0]["error"]["code"],
            "model_catalog_tools_unrestricted"
        );
        assert!(!f.root.path().join("calls.jsonl").exists());
    }
}

#[test]
fn missing_or_duplicate_model_metadata_rejects_launch_before_spawn() {
    for slug in [
        "gpt-6-astra",
        "gpt-5.6-luna",
        "gpt-5.6-terra",
        "gpt-5.6-sol",
    ] {
        for duplicate in [false, true] {
            let f = Fixture::new();
            let path = f.root.path().join("models.json");
            let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let entries = catalog["models"].as_array_mut().unwrap();
            if duplicate {
                entries.push(
                    entries
                        .iter()
                        .find(|entry| entry["slug"] == slug)
                        .unwrap()
                        .clone(),
                );
            } else {
                entries.retain(|entry| entry["slug"] != slug);
            }
            fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
            let result = f.invoke("launch", &f.request);
            assert_eq!(result.1[0]["error"]["code"], "model_catalog_invalid");
            assert!(!f.root.path().join("calls.jsonl").exists());
        }
    }
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

#[test]
fn native_error_classification_survives_launch_output_custody_and_replay() {
    for (message, expected) in [
        (
            "You've hit your usage limit. Try again later.",
            "quota_exhausted_inband",
        ),
        ("rate limit exceeded: request limit reached", "rate_limited"),
        ("Selected model is at capacity. Please try a different model.", "provider_unavailable"),
        ("Error running remote compact task: Selected model is at capacity. Please try a different model.", "provider_unavailable"),
        (
            "unexpected status 429 Too Many Requests: synthetic test",
            "rate_limited",
        ),
    ] {
        let f = Fixture::new();
        let event = json!({"type":"turn.failed","error":{"message":message}});
        fake_native(
            &f,
            &format!(
                "sys.stdin.read()\nprint({},flush=True)\nsys.exit(1)",
                json!(event.to_string())
            ),
        );
        let mut request = output_request(&f);
        request["host"]["env"]["OULIPOLY_HOST_TERMINAL_UNAVAILABLE_V1"] = json!("1");
        let first = f.invoke("launch", &request);
        assert_eq!(first.0, 1);
        assert_eq!(first.1.last().unwrap()["terminal_signal"]["kind"], expected);
        assert_complete_output(&first.1);
        assert_eq!(first, f.invoke("launch", &request));
        let stderr: Vec<u8> = first
            .1
            .iter()
            .filter(|e| e["kind"] == "stderr")
            .flat_map(|e| {
                agent_runner_codex::encoding::decode_base64(e["data_base64"].as_str().unwrap())
                    .unwrap()
            })
            .collect();
        let mut classify = request.clone();
        classify["params"] = json!({"stdout_base64":"","stderr_base64":encode_base64(&stderr),
            "status":{"kind":"exited","code":1},"observed_at_unix_ms":1});
        assert_eq!(
            f.invoke("terminal.classify", &classify).1[0]["result"]["terminal_signal"]["kind"],
            expected
        );
    }
}

#[test]
fn recovered_rate_limit_and_assistant_prose_do_not_poison_successful_launch() {
    let f = Fixture::new();
    let event = json!({"type":"error","message":"rate limit exceeded: temporary retry"});
    let path = f.root.path().join("codex");
    let body = fs::read_to_string(&path)
        .unwrap()
        .replace(
            "for event in [",
            &format!(
                "print({},flush=True)\nfor event in [",
                json!(event.to_string())
            ),
        )
        .replace("'native-ok'", "\"You've hit your usage limit.\"");
    file(&path, &body);
    let result = f.invoke("launch", &output_request(&f));
    assert_eq!(result.0, 0);
    assert_eq!(
        result.1.last().unwrap()["terminal_signal"]["kind"],
        "clean_exit"
    );
    assert_complete_output(&result.1);
}

#[test]
fn signal_and_host_cancellation_override_native_rate_error() {
    for cancelled in [false, true] {
        let f = Fixture::new();
        let end = if cancelled {
            "time.sleep(30)"
        } else {
            "os.kill(os.getpid(),9)"
        };
        fake_native(&f, &format!("sys.stdin.read()\nprint(json.dumps({{'type':'error','message':'rate limit exceeded: synthetic'}}),flush=True)\n{end}"));
        let mut request = output_request(&f);
        if cancelled {
            request["host"]["deadline_unix_ms"] = json!(now_ms() + 600);
        }
        let result = f.invoke("launch", &request);
        assert_eq!(result.0, if cancelled { 130 } else { 137 });
        let exit = result.1.last().unwrap();
        assert_eq!(
            exit["terminal_signal"]["kind"],
            if cancelled {
                "cancelled"
            } else {
                "signal_exit"
            }
        );
        assert_eq!(
            exit["status"]["kind"],
            if cancelled {
                "cancelled"
            } else {
                "signal_terminated"
            }
        );
        assert_complete_output(&result.1);
    }
}

#[test]
fn unavailable_launch_remains_compatible_with_unselected_hosts() {
    let f = Fixture::new();
    let event = json!({"type":"turn.failed","error":{"message":"Error running remote compact task: Selected model is at capacity. Please try a different model."}});
    fake_native(
        &f,
        &format!(
            "sys.stdin.read()\nprint({},flush=True)\nsys.exit(1)",
            json!(event.to_string())
        ),
    );
    let result = f.invoke("launch", &output_request(&f));
    assert_eq!(result.0, 1);
    let signal = &result.1.last().unwrap()["terminal_signal"];
    assert_eq!(signal["kind"], "nonzero_exit");
    assert_eq!(signal["evidence"], "codex.exec: server_overloaded");
}

// Real CLI pipe tests: an open read end is deliberately not drained until the
// provider exits. A small pipe makes the blocking relationship independent of
// machine defaults; the journal proves a data event reached delivery.
#[cfg(target_os = "linux")]
fn backpressured_cli(f: &Fixture, request: &Value) -> std::process::Child {
    use std::{
        io::Write,
        os::fd::AsRawFd,
        process::{Command, Stdio},
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .arg("launch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(
        unsafe {
            libc::fcntl(
                child.stdout.as_ref().unwrap().as_raw_fd(),
                libc::F_SETPIPE_SZ,
                4096,
            )
        },
        4096
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(request).unwrap())
        .unwrap();
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(3) {
        let pending = pending_output(&child);
        let journal = launch_journal(f);
        if pending > 0 && journal.len() > 128 * 1024 {
            let info = fs::read_to_string(format!("/proc/{}/fdinfo/1", child.id())).unwrap();
            let flags = info
                .lines()
                .find_map(|line| line.strip_prefix("flags:"))
                .unwrap();
            let flags = i32::from_str_radix(flags.trim(), 8).unwrap();
            assert_eq!(
                flags & libc::O_NONBLOCK,
                0,
                "inherited stdout flags were changed"
            );
            return child;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cleanup_blocked_cli(f, &mut child);
    panic!("native output did not reach the held-open host pipe");
}

#[cfg(target_os = "linux")]
fn pending_output(child: &std::process::Child) -> i32 {
    use std::os::fd::AsRawFd;
    let mut pending = 0;
    assert_eq!(
        unsafe {
            libc::ioctl(
                child.stdout.as_ref().unwrap().as_raw_fd(),
                libc::FIONREAD,
                &mut pending,
            )
        },
        0
    );
    pending
}

fn launch_journal(f: &Fixture) -> Vec<u8> {
    let root = f.root.path().join("data/provider-state/codex/launch");
    let Ok(files) = fs::read_dir(root) else {
        return Vec::new();
    };
    files
        .flatten()
        .find(|file| file.path().extension().is_some_and(|e| e == "jsonl"))
        .map(|file| fs::read(file.path()).unwrap())
        .unwrap_or_default()
}

fn native_group(f: &Fixture) -> i32 {
    fs::read_to_string(f.root.path().join("calls.jsonl"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn cleanup_blocked_cli(f: &Fixture, child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
    if f.root.path().join("calls.jsonl").exists() {
        unsafe {
            libc::kill(-native_group(f), libc::SIGKILL);
        }
    }
}

fn wait_cli(child: &mut std::process::Child, seconds: u64) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(seconds) {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

#[cfg(target_os = "linux")]
fn assert_blocked_delivery_fails_safely(trigger: &str, channel: &str) {
    let f = Fixture::new();
    fake_native(
        &f,
        &format!(
            r#"import subprocess
sys.stdin.read()
subprocess.Popen(['sleep','30'])
open(os.environ['CALLS'],'w').write(str(os.getpid()))
print(json.dumps({{'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}}),flush=True)
if {channel:?} == 'stderr':
 sys.stderr.write('x'*(256*1024)+'\n');sys.stderr.flush()
else:
 print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'x'*(256*1024)}}}}),flush=True)
time.sleep(30)"#
        ),
    );
    let mut request = output_request(&f);
    if trigger == "deadline" {
        request["host"]["deadline_unix_ms"] = json!(now_ms() + 1500);
    }
    let mut child = backpressured_cli(&f, &request);
    if trigger == "sigterm" {
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    }
    // Never drain stdout during this wait; keep the reader open and non-consuming.
    let stopped = wait_cli(&mut child, 5);
    if !stopped {
        cleanup_blocked_cli(&f, &mut child);
    }
    assert!(
        stopped,
        "provider stalled with open non-consuming reader: {trigger}/{channel}"
    );
    assert!(pending_output(&child) > 0);
    assert_eq!(
        unsafe { libc::kill(-native_group(&f), 0) },
        -1,
        "native group survived {trigger}/{channel}"
    );
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("oulipoly.launch_output_complete/v1"));
    let journal = launch_journal(&f);
    assert!(
        journal.starts_with(&output.stdout),
        "failed output was not a journal prefix"
    );
    let events: Vec<Value> = journal
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect();
    assert!(events.iter().any(|e| e["kind"] == channel));
    assert!(!events
        .iter()
        .any(|e| e["kind"] == "exit" || e["name"] == "oulipoly.launch_output_complete/v1"));
    let state_dir = f.root.path().join("data/provider-state/codex/launch");
    let state_file = fs::read_dir(state_dir)
        .unwrap()
        .flatten()
        .find(|e| e.path().extension().is_some_and(|e| e == "json"))
        .unwrap();
    let state: Value = serde_json::from_slice(&fs::read(state_file.path()).unwrap()).unwrap();
    assert_eq!(state["phase"], "running");
    assert!(
        state["journal_sha256"].is_null()
            && state["journal_len"].is_null()
            && state["exit_code"].is_null()
    );
    let before_calls = fs::read(f.root.path().join("calls.jsonl")).unwrap();
    let retry = f.invoke("launch", &request);
    assert_ne!(retry.0, 0);
    assert_eq!(
        retry.1[0]["error"]["code"],
        "launch_reconciliation_required"
    );
    assert_eq!(
        before_calls,
        fs::read(f.root.path().join("calls.jsonl")).unwrap()
    );
    assert_eq!(journal, launch_journal(&f));
    if let Some(destination) = std::env::var_os("CODEX_TEST_EVIDENCE_DIR") {
        let destination = Path::new(&destination).join(format!("blocked-{trigger}-{channel}"));
        fs::create_dir_all(&destination).unwrap();
        fs::write(
            destination.join("request.json"),
            serde_json::to_vec_pretty(&request).unwrap(),
        )
        .unwrap();
        fs::write(destination.join("journal.jsonl"), journal).unwrap();
        fs::write(
            destination.join("state.json"),
            serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
        fs::write(destination.join("stdout.raw"), output.stdout).unwrap();
        fs::write(destination.join("stderr.raw"), output.stderr).unwrap();
        fs::write(destination.join("result.json"), json!({"trigger":trigger,"channel":channel,"exit_code":output.status.code(),"native_group_gone":true,"retry":retry.1}).to_string()).unwrap();
    }
}

#[test]
#[cfg(target_os = "linux")]
fn held_open_output_pipe_cannot_strand_cancellation_deadline_or_cleanup() {
    for trigger in ["sigterm", "deadline", "delivery_timeout"] {
        for channel in ["stdout", "stderr"] {
            assert_blocked_delivery_fails_safely(trigger, channel);
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn temporarily_backpressured_cli_delivers_truthful_receipt_and_exact_replay() {
    let f = Fixture::new();
    fake_native(
        &f,
        r#"sys.stdin.read()
open(os.environ['CALLS'],'a').write('one-turn\n')
print(json.dumps({'type':'thread.started','thread_id':'11111111-2222-3333-4444-555555555555'}),flush=True)
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'x'*(256*1024)}}),flush=True)
print(json.dumps({'type':'turn.completed'}),flush=True)"#,
    );
    let request = output_request(&f);
    let child = backpressured_cli(&f, &request);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect();
    assert_complete_output(&events);
    assert_eq!(output.stdout, launch_journal(&f));
    // A blocked replay must fail delivery, not alter the durable completion or
    // execute a second native turn. A subsequent draining retry remains exact.
    let mut replay = backpressured_cli(&f, &request);
    let stopped = wait_cli(&mut replay, 5);
    if !stopped {
        let _ = replay.kill();
        let _ = replay.wait();
    }
    assert!(stopped, "completed replay blocked indefinitely");
    assert!(!replay.wait_with_output().unwrap().status.success());
    let retry = f.invoke("launch", &request);
    assert_eq!(retry, (0, events));
    assert_eq!(
        fs::read_to_string(f.root.path().join("calls.jsonl")).unwrap(),
        "one-turn\n"
    );
    assert_eq!(output.stdout, launch_journal(&f));
}

#[test]
fn cli_launch_preserves_file_offset_and_draining_socket_output() {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        process::{Command, Stdio},
    };
    let f = Fixture::new();
    let path = f.root.path().join("file-output");
    let mut destination = fs::File::create(&path).unwrap();
    destination.write_all(b"existing-prefix\n").unwrap();
    let mut file_child = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .arg("launch")
        .stdin(Stdio::piped())
        .stdout(destination)
        .spawn()
        .unwrap();
    file_child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&output_request(&f)).unwrap())
        .unwrap();
    assert!(wait_cli(&mut file_child, 5));
    assert!(file_child.wait().unwrap().success());
    let bytes = fs::read(path).unwrap();
    assert!(bytes.starts_with(b"existing-prefix\n"));
    assert_eq!(&bytes[b"existing-prefix\n".len()..], launch_journal(&f));

    // Replay the same result over a socket: no new native turn, no alteration
    // of the inherited socket's file status flags.
    let (mut reader, writer) = UnixStream::pair().unwrap();
    let retained = writer.try_clone().unwrap();
    use std::os::fd::{AsRawFd, OwnedFd};
    let original_flags = unsafe { libc::fcntl(retained.as_raw_fd(), libc::F_GETFL) };
    let fd: OwnedFd = writer.into();
    let mut socket_child = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .arg("launch")
        .stdin(Stdio::piped())
        .stdout(fd)
        .spawn()
        .unwrap();
    socket_child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&output_request(&f)).unwrap())
        .unwrap();
    assert!(wait_cli(&mut socket_child, 5));
    assert!(socket_child.wait().unwrap().success());
    assert_eq!(
        unsafe { libc::fcntl(retained.as_raw_fd(), libc::F_GETFL) },
        original_flags
    );
    drop(retained);
    let mut output = Vec::new();
    reader.read_to_end(&mut output).unwrap();
    let events: Vec<Value> = output
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect();
    assert_complete_output(&events);
    assert_eq!(output, launch_journal(&f));
}
