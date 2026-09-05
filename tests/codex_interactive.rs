use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
};

struct Fixture {
    root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        fs::create_dir_all(r.join("config/agent-runner-codex")).unwrap();
        fs::create_dir_all(r.join(".codex3/sessions")).unwrap();
        let native = r.join("native");
        fs::write(&native, r#"#!/usr/bin/env python3
import os,sys,json
if sys.argv[1:] == ['--version']:
 print('codex-cli 0.153.4');sys.exit(0)
with open(os.environ['CALLS'],'w') as f:
 json.dump({'pid':os.getpid(),'argv':sys.argv[1:],'home':os.environ['CODEX_HOME'],'mode':os.environ.get('AGENT_RUNNER_CODEX_INTERACTIVE'),'binding':os.environ.get('AGENT_RUNNER_CODEX_SESSION_BINDING'),'id':os.environ.get('AGENT_RUNNER_CODEX_SESSION_ID'),'file':os.environ.get('AGENT_RUNNER_CODEX_SESSION_FILE'),'parent_thread':os.environ.get('CODEX_THREAD_ID'),'instructions':os.environ.get('AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS'),'sentinel':os.environ.get('SENTINEL')},f)
"#).unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o755)).unwrap();
        for file in ["bun", "agent-bash", "agents", "mcp.ts"] {
            fs::write(r.join(file), "fixture").unwrap();
        }
        fs::write(
            r.join("models.json"),
            include_str!("../integrations/codex/models.json"),
        )
        .unwrap();
        fs::write(r.join("prompt.md"), "managed system prompt").unwrap();
        let paths = [
            ("codex_bin", "native"),
            ("bun_bin", "bun"),
            ("agent_bash_bin", "agent-bash"),
            ("agent_runner_bin", "agents"),
            ("bash_mcp_path", "mcp.ts"),
            ("system_prompt_file", "prompt.md"),
        ];
        fs::write(
            r.join("config/agent-runner-codex/config.toml"),
            paths
                .map(|(key, path)| format!("{key}={}\n", json!(r.join(path))))
                .join(""),
        )
        .unwrap();
        fs::write(r.join("config/providers.toml"), "[codex3]\nsystem_prompt_override='account extra instructions'\ntool_restrictions={kind='codex'}\n").unwrap();
        Self { root }
    }
    fn command(&self) -> Command {
        self.command_for("codex3")
    }
    fn command_for(&self, account: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"));
        command
            .args(["interactive", "--settings-id", account, "--config-root"])
            .arg(self.root.path().join("config"))
            .env("HOME", self.root.path())
            .env("CALLS", self.root.path().join("calls.json"))
            .env("SENTINEL", "inherited")
            .env("CODEX_THREAD_ID", "wrong-parent")
            .env("AGENT_RUNNER_CODEX_SESSION_ID", "wrong-parent")
            .env("AGENT_RUNNER_CODEX_SESSION_FILE", "wrong-parent-file")
            .current_dir(self.root.path())
            .stdin(Stdio::null());
        command
    }
    fn call(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.path().join("calls.json")).unwrap()).unwrap()
    }
}

#[test]
fn luna_and_terra_interactive_routes_preserve_every_effort_and_account() {
    for (family, model) in [("luna", "gpt-5.6-luna"), ("terra", "gpt-5.6-terra")] {
        for account in ["codex", "codex2", "codex3", "codex4", "codex5"] {
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                let f = Fixture::new();
                fs::create_dir_all(f.root.path().join(format!(".{account}/sessions"))).unwrap();
                fs::write(
                    f.root.path().join("config/providers.toml"),
                    format!("[{account}]\ntool_restrictions={{kind='codex'}}\n"),
                )
                .unwrap();
                // Labels and installed legacy argument pairs must produce identical native argv.
                let mut calls = Vec::new();
                for args in [
                    vec!["--model".to_string(), format!("gpt-{family}-{effort}")],
                    vec![
                        "-m".to_string(),
                        model.to_string(),
                        "-c".to_string(),
                        format!("model_reasoning_effort=\"{effort}\""),
                    ],
                ] {
                    let output = f.command_for(account).args(args).output().unwrap();
                    assert!(
                        output.status.success(),
                        "{account}/gpt-{family}-{effort}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let call = f.call();
                    assert!(Path::new(call["home"].as_str().unwrap()).starts_with(
                        f.root
                            .path()
                            .join(format!(".{account}/agent-runner-managed/tui"))
                    ));
                    let argv = call["argv"].as_array().unwrap();
                    for expected in [
                        model.to_string(),
                        format!("model_reasoning_effort=\"{effort}\""),
                        "features.shell_tool=false".to_string(),
                        "features.multi_agent=false".to_string(),
                        "features.plugins=false".to_string(),
                        "features.remote_plugin=false".to_string(),
                        "mcp_servers.agent_bash.enabled_tools=[\"bash\"]".to_string(),
                    ] {
                        assert!(argv.contains(&json!(expected)), "{expected}");
                    }
                    assert!(argv.iter().any(|value| value
                        .as_str()
                        .is_some_and(|value| value.starts_with("model_instructions_file="))));
                    calls.push(argv.clone());
                }
                assert_eq!(calls[0], calls[1]);
            }
        }
    }
}

#[test]
fn managed_tui_preserves_pty_process_identity_and_account_policy() {
    let f = Fixture::new();
    let mut child = f
        .command()
        .args(["--model", "gpt-luna-low", "--prompt", "first turn"])
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(child.wait().unwrap().success());
    let call = f.call();
    assert_eq!(call["pid"], pid);
    let home = Path::new(call["home"].as_str().unwrap());
    assert!(home.starts_with(f.root.path().join(".codex3/agent-runner-managed/tui")));
    for name in [
        "auth.json",
        "sessions",
        "archived_sessions",
        "thread-writer-locks",
    ] {
        assert_eq!(
            fs::read_link(home.join(name)).unwrap(),
            f.root.path().join(".codex3").join(name)
        );
    }
    assert_eq!(call["mode"], "1");
    assert_eq!(call["binding"], "tool_metadata");
    assert_eq!(call["id"], Value::Null);
    assert_eq!(call["file"], Value::Null);
    assert_eq!(call["parent_thread"], Value::Null);
    assert_eq!(call["sentinel"], "inherited");
    let argv = call["argv"].as_array().unwrap();
    for value in [
        "features.shell_tool=false",
        "features.multi_agent=false",
        "mcp_servers.agent_bash.enabled_tools=[\"bash\"]",
        "developer_instructions=\"account extra instructions\"",
        "gpt-5.6-luna",
        "model_reasoning_effort=\"low\"",
    ] {
        assert!(argv.contains(&json!(value)), "{value}");
    }
    for value in [
        "exec",
        "--json",
        "--skip-git-repo-check",
        "--ignore-user-config",
        "--ignore-rules",
    ] {
        assert!(!argv.contains(&json!(value)));
    }
    assert_eq!(&argv[argv.len() - 2..], &[json!("--"), json!("first turn")]);
}

#[test]
fn default_and_exact_legacy_model_routes_are_managed() {
    for (args, model, effort) in [
        (vec![], "gpt-6-astra", "xhigh"),
        (
            vec!["-m", "gpt-5.6-luna", "-c", "model_reasoning_effort=\"max\""],
            "gpt-5.6-luna",
            "max",
        ),
    ] {
        let f = Fixture::new();
        assert!(f.command().args(args).status().unwrap().success());
        let call = f.call();
        let argv = call["argv"].as_array().unwrap();
        assert!(argv.contains(&json!(model)));
        assert!(argv.contains(&json!(format!("model_reasoning_effort=\"{effort}\""))));
    }
}

#[test]
fn tui_clears_parent_instructions_unless_selected_account_declares_override() {
    for selected in [
        None,
        Some(""),
        Some("  "),
        Some("selected account instructions"),
    ] {
        let f = Fixture::new();
        let mut account = "[codex3]\ntool_restrictions={kind='codex'}\n".to_string();
        if let Some(value) = selected {
            account.push_str(&format!("system_prompt_override={}\n", json!(value)));
        }
        fs::write(f.root.path().join("config/providers.toml"), account).unwrap();
        assert!(f
            .command()
            .env(
                "AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS",
                "stale parent instructions"
            )
            .status()
            .unwrap()
            .success());
        let call = f.call();
        let pairs: Vec<_> = call["argv"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|value| {
                value
                    .as_str()
                    .is_some_and(|value| value.starts_with("developer_instructions="))
            })
            .cloned()
            .collect();
        let expected: Vec<_> = selected
            .filter(|value| !value.trim().is_empty())
            .map(|value| json!(format!("developer_instructions={}", json!(value))))
            .into_iter()
            .collect();
        assert_eq!(pairs, expected);
        assert_eq!(
            call["instructions"],
            json!(selected.filter(|value| !value.trim().is_empty()))
        );
    }
}

#[test]
fn tui_rejects_overrides_missing_dependencies_and_wrong_account_resume() {
    for args in [
        vec!["--model", "gpt-low", "-c", "features.shell_tool=true"],
        vec!["-m", "gpt-6-astra", "-c", "features.shell_tool=true"],
        vec!["--model", "gpt-low", "--model", "gpt-high"],
        vec!["--resume", "11111111-2222-3333-4444-555555555555"],
        vec!["--remote", "ws://example"],
        vec!["--model", "gpt-luna-ultra"],
        vec!["--model", "gpt-terra-ultra"],
        vec![
            "-m",
            "gpt-5.6-terra",
            "-c",
            "model_reasoning_effort=\"ultra\"",
        ],
    ] {
        let f = Fixture::new();
        assert!(!f.command().args(args).output().unwrap().status.success());
        assert!(!f.root.path().join("calls.json").exists());
    }
    let f = Fixture::new();
    fs::remove_file(f.root.path().join("prompt.md")).unwrap();
    assert!(!f.command().output().unwrap().status.success());
    assert!(!f.root.path().join("calls.json").exists());
    let f = Fixture::new();
    fs::write(
        f.root.path().join("config/providers.toml"),
        "[codex3]\ntool_restrictions={kind='codex',codex={disabled_features=['x']}}\n",
    )
    .unwrap();
    assert!(!f.command().output().unwrap().status.success());
    assert!(!f.root.path().join("calls.json").exists());
}

#[test]
fn tui_resume_requires_native_ownership_and_binds_the_known_thread() {
    let f = Fixture::new();
    let id = "11111111-2222-3333-4444-555555555555";
    let rollout = json!({"type":"session_meta","payload":{"id":id,"cwd":f.root.path(),"timestamp":"2026-09-04T00:00:00Z"}});
    fs::write(
        f.root.path().join(".codex3/sessions/rollout-fixture.jsonl"),
        format!("{rollout}\n"),
    )
    .unwrap();
    assert!(f
        .command()
        .args(["--resume", id])
        .status()
        .unwrap()
        .success());
    let call = f.call();
    assert_eq!(call["id"], id);
    let argv = call["argv"].as_array().unwrap();
    assert_eq!(&argv[argv.len() - 2..], &[json!("resume"), json!(id)]);
}

#[test]
fn help_never_requires_configuration_or_reads_terminal_input() {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-runner-codex"))
        .args(["interactive", "--help"])
        .env("HOME", "/nonexistent")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("agent-runner-codex interactive"));
    assert!(text.contains("--settings-id") && text.contains("--resume"));
}

#[test]
fn headless_policy_accepts_only_its_exact_managed_executable_carrier() {
    use std::io::Write;
    let f = Fixture::new();
    let binary = Path::new(env!("CARGO_BIN_EXE_agent-runner-codex"));
    for (carrier, accepted) in [(binary, true), (f.root.path(), false)] {
        let args = ["-m", "gpt-5.6-luna", "-c", "model_reasoning_effort=\"low\""];
        let mut argv = vec![
            json!(carrier),
            json!("exec"),
            json!("--dangerously-bypass-approvals-and-sandbox"),
        ];
        argv.extend(args.map(|arg| json!(arg)));
        let request = json!({"contract":"oulipoly.provider/v1","request_id":"carrier-test","host":{"app":"test","config_root":f.root.path().join("config"),"env":{"HOME":f.root.path()}},
            "params":{"settings_id":"codex3","mode":"arg","model":{"name":"gpt-luna-low","provider_args":args,"inputs":{"prompt":"test","named":{}}},"launch":{"argv":argv,"env":{}}}});
        let mut child = Command::new(binary)
            .arg("policy.evaluate")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_string(&request).unwrap().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["result"]["accepted"], accepted, "{response}");
    }
}
