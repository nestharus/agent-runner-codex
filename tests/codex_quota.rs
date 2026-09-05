use agent_runner_codex::dispatch::write_invocation;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    request: Value,
}
impl Fixture {
    fn new(script: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        fs::create_dir_all(r.join(".local/bin")).unwrap();
        fs::create_dir_all(r.join(".codex4")).unwrap();
        fs::write(r.join(".codex4/auth.json"), "{}").unwrap();
        let helper = r.join(".local/bin/chatgpt-usage");
        fs::write(&helper, script).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let request = json!({"contract":"oulipoly.provider/v1", "request_id":"quota-test", "provider_instance_id":"codex4",
            "host":{"app":"test", "env":{"HOME":r, "SENTINEL":"kept", "CODEX_HOME":"wrong", "CALLS":r.join("calls.json")}},
            "params":{"settings_id":"codex4"}});
        Self { root, request }
    }
    fn invoke(&self, operation: &str) -> (i32, Value) {
        let mut output = Vec::new();
        let code = write_invocation(
            &["provider".into(), operation.into()],
            &serde_json::to_vec(&self.request).unwrap(),
            &mut output,
        );
        (code, serde_json::from_slice(&output).unwrap())
    }
}

#[test]
fn quota_probe_pins_auth_home_preserves_env_and_converts_windows() {
    let fixture = Fixture::new(
        r#"#!/usr/bin/env python3
import json,os,sys
with open(os.environ['CALLS'],'w') as f: json.dump({'auth':sys.argv[1],'home':os.environ['CODEX_HOME'],'sentinel':os.environ['SENTINEL']},f)
print(json.dumps({'windows':[{'used_percent':25,'resets_at':'2026-09-05T00:00:00Z'},{'used_percent':100,'resets_at':'2026-09-06T00:00:00Z'}]}))
"#,
    );
    assert_eq!(
        fixture.invoke("quota.source").1["result"]["has_source"],
        true
    );
    let (code, response) = fixture.invoke("quota.probe");
    assert_eq!(code, 0, "{response}");
    let result = &response["result"];
    assert_eq!(result["available"], true);
    assert_eq!(result["windows"][0]["remaining_ratio"], 0.75);
    assert_eq!(result["windows"][1]["remaining_ratio"], 0.0);
    assert_eq!(result["windows"][0]["resets_at_unix_ms"], 1788566400000_u64);
    let calls: Value =
        serde_json::from_str(&fs::read_to_string(fixture.root.path().join("calls.json")).unwrap())
            .unwrap();
    assert_eq!(calls["home"], json!(fixture.root.path().join(".codex4")));
    assert_eq!(
        calls["auth"],
        json!(fixture.root.path().join(".codex4/auth.json"))
    );
    assert_eq!(calls["sentinel"], "kept");
}

#[test]
fn adapter_errors_are_unavailable_without_leaking_output_or_assuming_full_quota() {
    for script in [
        "#!/bin/sh\necho 'secret-token' >&2\nexit 4\n",
        "#!/bin/sh\necho '{\"windows\":[]}'\n",
        "#!/bin/sh\necho 'secret-token'\n",
    ] {
        let fixture = Fixture::new(script);
        let (code, response) = fixture.invoke("quota.probe");
        assert_eq!(code, 0);
        assert_eq!(response["result"]["available"], false);
        assert_eq!(response["result"]["windows"], json!([]));
        assert!(!response.to_string().contains("secret-token"));
    }
}

#[test]
fn probe_enforces_host_deadline() {
    let mut fixture = Fixture::new("#!/bin/sh\nsleep 30\n");
    fixture.request["host"]["deadline_unix_ms"] =
        json!(chrono::Utc::now().timestamp_millis() + 150);
    let started = Instant::now();
    let (code, response) = fixture.invoke("quota.probe");
    assert_eq!(code, 0);
    assert_eq!(response["result"]["available"], false);
    assert_eq!(response["result"]["detail"], "Quota probe deadline expired");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn missing_auth_and_unknown_account_do_not_run_adapter_and_refresh_is_explicitly_unsupported() {
    let mut fixture = Fixture::new("#!/bin/sh\nexit 9\n");
    fs::remove_file(fixture.root.path().join(".codex4/auth.json")).unwrap();
    assert_eq!(
        fixture.invoke("quota.source").1["result"]["has_source"],
        false
    );
    assert_eq!(
        fixture.invoke("quota.probe").1["result"]["available"],
        false
    );
    let (_, response) = fixture.invoke("quota.refresh_auth");
    assert_eq!(response["error"]["code"], "auth_refresh_unsupported");
    fixture.request["params"]["settings_id"] = json!("codex6");
    let (code, response) = fixture.invoke("quota.probe");
    assert_ne!(code, 0);
    assert_eq!(response["error"]["code"], "unknown_account");
}
