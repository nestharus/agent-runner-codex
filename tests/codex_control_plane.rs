use agent_runner_codex::write_invocation;
use serde_json::{json, Value};

#[test]
fn discovery_registers_exact_efforts_models_and_accounts() {
    let request = json!({"contract":"oulipoly.provider/v1","request_id":"model-routes","host":{"app":"contract-test"},"params":{}});
    let mut output = Vec::new();
    assert_eq!(
        write_invocation(
            &["agent-runner-codex".into(), "discovery.models".into()],
            &serde_json::to_vec(&request).unwrap(),
            &mut output
        ),
        0
    );
    let response: Value = serde_json::from_slice(&output).unwrap();
    let entries = response["result"]["models"].as_array().unwrap();
    assert_eq!(entries.len(), 26);
    let gpt: Vec<_> = entries
        .iter()
        .filter(|entry| entry["name"] == "gpt")
        .collect();
    assert_eq!(gpt.len(), 1);
    assert_eq!(gpt[0]["provider_model"], "gpt-6.1-sol");
    assert_eq!(
        gpt[0]["provider_args"],
        json!(["-m", "gpt-6.1-sol", "-c", "model_reasoning_effort=\"high\""])
    );
    assert_eq!(
        gpt[0]["eligible_accounts"],
        json!(["codex", "codex2", "codex3", "codex4", "codex5"])
    );
    assert!(!entries.iter().any(|entry| entry["name"] == "default"));
    let metadata: Value =
        serde_json::from_str(include_str!("../integrations/codex/models.json")).unwrap();
    for (prefix, model) in [
        ("gpt-", "gpt-6.1-sol"),
        ("gpt-astra-", "gpt-6-astra"),
        ("gpt-luna-", "gpt-6-luna"),
        ("gpt-terra-", "gpt-5.6-terra"),
        ("gpt-sol-", "gpt-6.1-sol"),
    ] {
        let native = metadata["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["slug"] == model)
            .unwrap();
        let levels: Vec<_> = native["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|level| level["effort"].as_str().unwrap())
            .collect();
        assert_eq!(levels, ["low", "medium", "high", "xhigh", "max"]);
        for effort in levels {
            let name = format!("{prefix}{effort}");
            let matches: Vec<_> = entries
                .iter()
                .filter(|entry| entry["name"] == name)
                .collect();
            assert_eq!(matches.len(), 1, "{name}");
            assert_eq!(matches[0]["provider_model"], model);
            assert_eq!(
                matches[0]["provider_args"],
                json!([
                    "-m",
                    model,
                    "-c",
                    format!("model_reasoning_effort=\"{effort}\"")
                ])
            );
            assert_eq!(
                matches[0]["eligible_accounts"],
                json!(["codex", "codex2", "codex3", "codex4", "codex5"])
            );
        }
    }
    assert!(!entries
        .iter()
        .any(|entry| entry["name"] == "codex-exec-bench"));
    assert!(!entries.iter().any(|entry| entry["name"]
        .as_str()
        .is_some_and(|name| name.starts_with("codex-gpt-"))));
}

#[test]
fn control_plane_responses_match_sdk_operation_contracts() {
    let temporary = tempfile::tempdir().unwrap();
    let registry = agent_provider_contract::SchemaRegistry::new();
    for (operation, params) in [
        ("describe", json!({})),
        ("discovery.models", json!({})),
        ("discovery.accounts", json!({})),
        ("setup.detect", json!({})),
        ("setup.install_plan", json!({})),
        ("setup.sync_plan", json!({})),
        ("quota.source", json!({"settings_id":"codex"})),
        ("quota.probe", json!({"settings_id":"codex"})),
    ] {
        for selected in [false, true] {
            let env = if selected {
                json!({"HOME":temporary.path(),"OULIPOLY_HOST_LAUNCH_OUTPUT_V1":"1","OULIPOLY_HOST_SESSION_TURN_PAGES_V1":"1"})
            } else {
                json!({"HOME":temporary.path()})
            };
            let request = json!({"contract":"oulipoly.provider/v1","request_id":"control-test","host":{"app":"contract-test","config_root":temporary.path(),"env":env},"params":params});
            let mut output = Vec::new();
            let code = write_invocation(
                &["agent-runner-codex".into(), operation.into()],
                &serde_json::to_vec(&request).unwrap(),
                &mut output,
            );
            let response: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(code, 0, "{operation}: {response}");
            registry.validate_response(operation, &response).unwrap();
        }
    }
}

fn invoke_contract(operation: &str, request: &Value) -> (i32, Value) {
    let mut output = Vec::new();
    let code = write_invocation(
        &["agent-runner-codex".into(), operation.into()],
        &serde_json::to_vec(request).unwrap(),
        &mut output,
    );
    (code, serde_json::from_slice(&output).unwrap())
}

#[test]
fn base_admission_enforces_describe_intent_and_preserves_request_identity() {
    // Independent provider/v1 intent: describe is an empty-object query; host
    // app and request_id need characters, not non-whitespace content. Optional
    // string fields are omitted, not null, and unknown properties are refused.
    let request = json!({"contract":"oulipoly.provider/v1","request_id":"admission-id",
        "host":{"app":"contract-test"},"params":{}});
    for (app, id) in [("contract-test", "admission-id"), (" ", " "), ("test", "é")] {
        let mut accepted = request.clone();
        accepted["host"]["app"] = json!(app);
        accepted["request_id"] = json!(id);
        let (code, response) = invoke_contract("describe", &accepted);
        assert_eq!(code, 0, "{response}");
        assert_eq!(response["request_id"], id);
        assert_eq!(response["ok"], true);
    }
    let mut refused = Vec::new();
    for params in [Value::Null, json!(1), json!([]), json!({"unused":true})] {
        let mut candidate = request.clone();
        candidate["params"] = params;
        refused.push(candidate);
    }
    for (pointer, value) in [
        ("/host/app", json!("")),
        ("/host/app_version", Value::Null),
        ("/host/env", Value::Null),
        ("/host/unrecognized", json!(true)),
        ("/provider_instance_id", Value::Null),
        ("/contract", json!("oulipoly.provider/v2")),
    ] {
        let (parent, name) = pointer.rsplit_once('/').unwrap();
        let mut candidate = request.clone();
        candidate
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(name.into(), value);
        refused.push(candidate);
    }
    for candidate in refused {
        let (code, response) = invoke_contract("describe", &candidate);
        assert_eq!(code, 2, "{candidate}: {response}");
        assert_eq!(response["ok"], false);
        assert_eq!(response["request_id"], "admission-id");
        assert_eq!(response["error"]["category"], "invalid_request");
    }
}

#[test]
fn sdk_operation_params_are_admitted_before_adapter_work() {
    // A scalar is not a query/launch/policy/session params object. These
    // requests must not reach configuration, account, filesystem or native work.
    for operation in [
        "describe",
        "discovery.models",
        "discovery.accounts",
        "policy.evaluate",
        "launch",
        "terminal.classify",
        "quota.source",
        "session.read_turns",
        "setup.detect",
    ] {
        let request = json!({"contract":"oulipoly.provider/v1","request_id":"before-adapter",
            "host":{"app":"contract-test"},"params":42});
        let (code, response) = invoke_contract(operation, &request);
        assert_eq!(code, 2, "{operation}: {response}");
        assert_eq!(response["request_id"], "before-adapter");
        assert_eq!(response["error"]["code"], "invalid_request");
    }
}

#[test]
fn resident_extension_keeps_shared_envelope_admission() {
    for missing_params in [false, true] {
        let mut request = json!({"contract":"oulipoly.provider/v1","request_id":"resident-envelope",
            "host":{"app":"contract-test"},"params":{}});
        if missing_params {
            request.as_object_mut().unwrap().remove("params");
        } else {
            request["host"]["app"] = json!("");
        }
        let (code, response) = invoke_contract("resident.prepare", &request);
        assert_eq!(code, 2, "{response}");
        assert_eq!(response["request_id"], "resident-envelope");
        assert_eq!(response["error"]["code"], "invalid_request");
    }
}
