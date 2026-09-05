use agent_runner_codex::{encoding::encode_base64, write_invocation};
use serde_json::{json, Value};

fn classify(stdout: &[u8], stderr: &[u8], status: Value) -> Value {
    classify_for_host(
        stdout,
        stderr,
        status,
        json!({"OULIPOLY_HOST_TERMINAL_UNAVAILABLE_V1":"1"}),
    )
}

fn classify_for_host(stdout: &[u8], stderr: &[u8], status: Value, env: Value) -> Value {
    let request = json!({"contract":"oulipoly.provider/v1","request_id":"terminal-audit",
        "host":{"app":"contract-test","env":env},"params":{"stdout_base64":encode_base64(stdout),
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
fn pinned_native_error_events_distinguish_quota_rate_limits_and_unavailability() {
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
        ("Selected model is at capacity. Please try a different model.", "provider_unavailable"),
        ("Error running remote compact task: Selected model is at capacity. Please try a different model.", "provider_unavailable"),
        ("Error running remote compact task: You've hit your usage limit.", "quota_exhausted_inband"),
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

#[test]
fn unavailable_requires_explicit_host_selection_and_preserves_fixed_evidence() {
    let event = serde_json::to_vec(&json!({"type":"turn.failed","error":{"message":"Error running remote compact task: Selected model is at capacity. Please try a different model."}})).unwrap();
    for env in [
        json!({}),
        json!({"OULIPOLY_HOST_TERMINAL_UNAVAILABLE_V1":"true"}),
        json!({"OULIPOLY_HOST_TERMINAL_UNAVAILABLE_V1":"0"}),
    ] {
        let signal = classify_for_host(b"", &event, json!({"kind":"exited","code":1}), env);
        assert_eq!(signal["kind"], "nonzero_exit");
        assert_eq!(signal["evidence"], "codex.exec: server_overloaded");
    }
    for (status, expected) in [
        (json!({"kind":"exited","code":0}), "clean_exit"),
        (json!({"kind":"cancelled"}), "cancelled"),
        (
            json!({"kind":"signal_terminated","signal":9}),
            "signal_exit",
        ),
        (
            json!({"kind":"spawn_error","reason":"missing"}),
            "spawn_error",
        ),
        (
            json!({"kind":"prolonged_silence","reason":"timeout"}),
            "prolonged_silence",
        ),
        (json!({"kind":"unknown"}), "provider_unavailable"),
    ] {
        assert_eq!(classify(b"", &event, status)["kind"], expected);
    }
    let mut recovered = event.clone();
    recovered.extend_from_slice(
        b"\n{\"type\":\"turn.failed\",\"error\":{\"message\":\"authentication failed\"}}\n",
    );
    assert_eq!(
        classify(b"", &recovered, json!({"kind":"exited","code":1}))["kind"],
        "nonzero_exit"
    );
}

#[test]
fn overload_words_in_prose_tool_errors_or_other_native_errors_are_not_classification() {
    let message = "Selected model is at capacity. Please try a different model.";
    let event = serde_json::to_vec(&json!({"type":"error","message":message})).unwrap();
    assert_eq!(
        classify(&event, b"", json!({"kind":"exited","code":1}))["kind"],
        "nonzero_exit"
    );
    for event in [
        json!({"type":"item.completed","item":{"type":"mcp_tool_call","error":{"message":message}}}),
        json!({"type":"error","message":format!("Example error: {message}")}),
        json!({"type":"error","message":format!("unexpected status 500 Internal Server Error: {message}")}),
        json!({"type":"error","message":format!("{message} private-url")}),
    ] {
        assert_eq!(
            classify(
                b"",
                &serde_json::to_vec(&event).unwrap(),
                json!({"kind":"exited","code":1})
            )["kind"],
            "nonzero_exit"
        );
    }
    assert_eq!(
        classify(b"", message.as_bytes(), json!({"kind":"exited","code":1}))["kind"],
        "nonzero_exit"
    );
}

#[test]
fn selected_and_legacy_failure_signals_match_their_wire_schemas() {
    let common: Value =
        serde_json::from_str(include_str!("../contract/v1/common.schema.json")).unwrap();
    let mut legacy = common.clone();
    legacy["$defs"]["TerminalSignalKind"]["enum"]
        .as_array_mut()
        .unwrap()
        .retain(|kind| kind != "provider_unavailable");
    let event = serde_json::to_vec(&json!({"type":"error","message":"Selected model is at capacity. Please try a different model."})).unwrap();
    let selected = classify(b"", &event, json!({"kind":"exited","code":1}));
    let fallback = classify_for_host(b"", &event, json!({"kind":"exited","code":1}), json!({}));
    let extension: Value = serde_json::from_str(include_str!(
        "../contract/extensions/terminal-unavailable/v1.schema.json"
    ))
    .unwrap();
    let extension_validator = jsonschema::JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .compile(&extension)
        .unwrap();
    assert!(extension_validator.is_valid(&selected));
    assert!(!extension_validator.is_valid(&fallback));
    for (schema, supports_unavailable) in [(common, true), (legacy, false)] {
        let root = json!({"$ref":"https://contract.test/common.schema.json#/$defs/TerminalSignal"});
        let validator = jsonschema::JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_document("https://contract.test/common.schema.json".into(), schema)
            .compile(&root)
            .unwrap();
        assert_eq!(validator.is_valid(&selected), supports_unavailable);
        assert!(validator.is_valid(&fallback));
    }
}
