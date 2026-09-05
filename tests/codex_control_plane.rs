use agent_runner_codex::write_invocation;
use serde_json::{json, Value};
use std::{fs, path::Path};

#[test]
fn control_plane_responses_match_the_copied_host_contracts() {
    let temporary = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("contract/v1");
    let common: Value =
        serde_json::from_slice(&fs::read(root.join("common.schema.json")).unwrap()).unwrap();
    for (operation, schema_file, definition, params) in [
        (
            "describe",
            "describe.schema.json",
            "DescribeResult",
            json!({}),
        ),
        (
            "discovery.models",
            "discovery.schema.json",
            "DiscoveryModelsResult",
            json!({}),
        ),
        (
            "discovery.accounts",
            "discovery.schema.json",
            "DiscoveryAccountsResult",
            json!({}),
        ),
        (
            "setup.detect",
            "setup.schema.json",
            "SetupDetectResult",
            json!({}),
        ),
        (
            "setup.install_plan",
            "setup.schema.json",
            "SetupInstallPlanResult",
            json!({}),
        ),
        (
            "setup.sync_plan",
            "setup.schema.json",
            "SetupSyncPlanResult",
            json!({}),
        ),
        (
            "quota.source",
            "quota.schema.json",
            "QuotaSourceResult",
            json!({"settings_id":"codex"}),
        ),
        (
            "quota.probe",
            "quota.schema.json",
            "QuotaProbeResult",
            json!({"settings_id":"codex"}),
        ),
    ] {
        let contract: Value =
            serde_json::from_slice(&fs::read(root.join(schema_file)).unwrap()).unwrap();
        let selected_schema =
            json!({"$ref":format!("https://contract.test/{schema_file}#/$defs/{definition}")});
        let validator = jsonschema::JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_document(format!("https://contract.test/{schema_file}"), contract)
            .with_document(
                "https://contract.test/common.schema.json".into(),
                common.clone(),
            )
            .compile(&selected_schema)
            .unwrap();
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
            if let Err(errors) = validator.validate(&response["result"]) {
                panic!(
                    "{operation}: {}",
                    errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ")
                );
            };
        }
    }
}
