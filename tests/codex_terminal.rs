use agent_runner_codex::{encoding::encode_base64, write_invocation};
use serde_json::{json, Value};

fn classify(stdout: &[u8], stderr: &[u8], status: Value) -> Value {
    let request = json!({"contract":"oulipoly.provider/v1","request_id":"terminal-audit",
        "host":{"app":"contract-test"},"params":{"stdout_base64":encode_base64(stdout),
        "stderr_base64":encode_base64(stderr),"status":status,"observed_at_unix_ms":123}});
    let mut output = Vec::new();
    assert_eq!(
        write_invocation(
            &["provider".into(), "terminal.classify".into()],
            &serde_json::to_vec(&request).unwrap(),
            &mut output
        ),
        0
    );
    serde_json::from_slice::<Value>(&output).unwrap()["result"]["terminal_signal"].clone()
}

#[test]
fn pinned_native_error_events_distinguish_quota_and_rate_limits() {
    // Messages are from openai/codex rust-v0.153.4 protocol/src/error.rs;
    // exec/src/exec_events.rs exposes only message, without the API code.
    for (message, expected) in [
        (
            "You've hit your usage limit. Try again tomorrow.",
            "quota_exhausted_inband",
        ),
        (
            "You've hit your usage limit for GPT. Switch to another model now, or try again later.",
            "quota_exhausted_inband",
        ),
        (
            "Quota exceeded. Check your plan and billing details.",
            "quota_exhausted_inband",
        ),
        (
            "Your workspace is out of credits. Add credits to continue.",
            "quota_exhausted_inband",
        ),
        (
            "You hit your spend cap set in your workspace. Increase your spend cap to continue.",
            "quota_exhausted_inband",
        ),
        ("rate limit exceeded: request limit reached", "rate_limited"),
        (
            "unexpected status 429 Too Many Requests: server response, url: private-url",
            "rate_limited",
        ),
        (
            "exceeded retry limit, last status: 429 Too Many Requests, request id: private-id",
            "rate_limited",
        ),
    ] {
        for event in [
            json!({"type":"error","message":message}),
            json!({"type":"turn.failed","error":{"message":message}}),
        ] {
            let signal = classify(
                b"",
                &serde_json::to_vec(&event).unwrap(),
                json!({"kind":"exited","code":1}),
            );
            assert_eq!(signal["kind"], expected, "{event}");
            assert_eq!(signal["observed_at_unix_ms"], 123);
            assert!(!signal["evidence"].as_str().unwrap().contains("private"));
        }
    }
}

#[test]
fn assistant_json_tool_errors_and_unstructured_diagnostics_are_not_quota_evidence() {
    let error = json!({"type":"turn.failed","error":{"message":"You've hit your usage limit. Try again later."}});
    assert_eq!(
        classify(
            &serde_json::to_vec(&error).unwrap(),
            b"",
            json!({"kind":"exited","code":1})
        )["kind"],
        "nonzero_exit"
    );
    for stderr in [
        b"You've hit your usage limit. Try again later.".to_vec(),
        b"OpenAI Codex v0.153.4 HTTP 429 rate_limit_exceeded".to_vec(),
        b"\xff\x00{\"type\":\"error\",\"message\":\"rate limit exceeded: x\"}".to_vec(),
        serde_json::to_vec(&json!({"type":"item.completed","item":{"type":"agent_message","text":error.to_string()}})).unwrap(),
        serde_json::to_vec(&json!({"type":"item.completed","item":{"type":"mcp_tool_call","error":{"message":"rate limit exceeded: x"}}})).unwrap(),
        serde_json::to_vec(&json!({"type":"turn.failed","error":{"message":"Example error: You've hit your usage limit."}})).unwrap(),
        serde_json::to_vec(&json!({"type":"error","message":"unexpected status 500 Internal Server Error: HTTP 429"})).unwrap(),
    ] {
        assert_eq!(classify(b"", &stderr, json!({"kind":"exited","code":1}))["kind"], "nonzero_exit");
    }
}

#[test]
fn final_error_and_explicit_process_outcome_override_earlier_rate_limit() {
    let rate = b"{\"type\":\"error\",\"message\":\"rate limit exceeded: request limit\"}\n";
    for (status, kind) in [
        (json!({"kind":"exited","code":0}), "clean_exit"),
        (
            json!({"kind":"signal_terminated","signal":9}),
            "signal_exit",
        ),
        (json!({"kind":"cancelled"}), "cancelled"),
        (
            json!({"kind":"spawn_error","reason":"missing"}),
            "spawn_error",
        ),
        (
            json!({"kind":"prolonged_silence","reason":"timeout"}),
            "prolonged_silence",
        ),
    ] {
        assert_eq!(classify(b"", rate, status)["kind"], kind);
    }
    let mut stderr = rate.to_vec();
    stderr.extend_from_slice(
        b"{\"type\":\"turn.failed\",\"error\":{\"message\":\"authentication failed\"}}\n",
    );
    assert_eq!(
        classify(b"", &stderr, json!({"kind":"exited","code":1}))["kind"],
        "nonzero_exit"
    );
}
