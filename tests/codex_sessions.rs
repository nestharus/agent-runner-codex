use agent_runner_codex::envelope::RequestEnvelope;
use agent_runner_codex::session;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const OTHER: &str = "22222222-2222-4222-8222-222222222222";
const CHILD: &str = "33333333-3333-4333-8333-333333333333";

struct Fixture {
    home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn request(&self, params: Value) -> RequestEnvelope {
        serde_json::from_value(json!({"contract":"oulipoly.provider/v1","request_id":"session-fixture","provider_instance_id":"codex",
            "host":{"app":"test","working_directory":"/workspace","env":{"HOME":self.home.path(),"CODEX_HOME":"/must/not/use/ambient/account"}},
            "params":params})).unwrap()
    }

    fn invoke(&self, operation: &str, params: Value) -> Value {
        session::handle(operation, &self.request(params)).unwrap()
    }

    fn write(&self, account: &str, filename_id: &str, records: Vec<Value>) -> PathBuf {
        let root = self
            .home
            .path()
            .join(format!(".{account}/sessions/2026/09/04"));
        fs::create_dir_all(&root).unwrap();
        let path = root.join(format!("rollout-2026-09-04T12-00-00-{filename_id}.jsonl"));
        let mut file = fs::File::create(&path).unwrap();
        for record in records {
            writeln!(file, "{record}").unwrap();
        }
        path
    }
}

fn meta(id: &str) -> Value {
    json!({"timestamp":"2026-09-04T12:00:00Z","type":"session_meta",
        "payload":{"id":id,"cwd":"/workspace","timestamp":"2026-09-04T12:00:00Z"}})
}

fn message(role: &str, time: &str, body: &str, phase: Option<&str>) -> Value {
    json!({"timestamp":time,"type":"response_item","payload":{"type":"message","id":null,
        "role":role,"phase":phase,"content":[{"type":if role == "user" {"input_text"} else {"output_text"},"text":body}]}})
}

fn records(id: &str) -> Vec<Value> {
    vec![
        meta(id),
        json!({"timestamp":"2026-09-04T12:00:01Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"private system configuration"}]}}),
        message(
            "user",
            "2026-09-04T12:00:02.000000001Z",
            "Do the work",
            None,
        ),
        message(
            "assistant",
            "2026-09-04T12:00:03Z",
            "Working",
            Some("commentary"),
        ),
        json!({"timestamp":"2026-09-04T12:00:04Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"private tool details"}}),
        message(
            "assistant",
            "2026-09-04T12:00:05Z",
            "Done",
            Some("final_answer"),
        ),
        json!({"timestamp":"2026-09-04T12:00:06Z","type":"event_msg","payload":{"type":"task_complete"}}),
    ]
}

fn params(account: &str, id: &str) -> Value {
    json!({"settings_id":account,"session_id":id})
}

#[test]
fn transcript_identity_comes_from_first_metadata_not_filename_or_inherited_parent() {
    let fixture = Fixture::new();
    let mut child_records = records(CHILD);
    child_records.insert(1, meta(SESSION));
    let child = fixture.write("codex", SESSION, child_records);
    let located = fixture.invoke("session.locate_transcript", params("codex", CHILD));
    assert_eq!(located["path"], child.to_str().unwrap());
    assert_eq!(located["format_id"], "codex.rollout/jsonl");
    assert_eq!(
        fixture.invoke("session.locate_transcript", params("codex", SESSION)),
        json!({"located":false})
    );
    let turns = fixture.invoke("session.read_turns", params("codex", CHILD));
    assert!(turns["turns"]
        .as_array()
        .unwrap()
        .iter()
        .all(|t| t["session_id"] == CHILD));
}

#[test]
fn complete_read_has_stable_ids_text_projection_and_final_completion() {
    let fixture = Fixture::new();
    let path = fixture.write("codex", SESSION, records(SESSION));
    let read = fixture.invoke("session.read_turns", params("codex", SESSION));
    assert_eq!(read["complete"], true);
    assert_eq!(read["turn_count"], 3);
    assert_eq!(read["turns"][0]["turn_id"], format!("{SESSION}:line:3"));
    assert_eq!(
        read["turns"][0]["body"],
        json!([{"type":"text","text":"Do the work"}])
    );
    assert_eq!(read["turns"][1]["native"]["completed"], false);
    assert_eq!(read["turns"][2]["native"]["completed"], true);
    let archived = fixture.home.path().join(".codex/archived_sessions");
    fs::create_dir_all(&archived).unwrap();
    fs::rename(&path, archived.join(path.file_name().unwrap())).unwrap();
    assert_eq!(
        read,
        fixture.invoke("session.read_turns", params("codex1", SESSION))
    );
    let captured = fixture.invoke("session.capture", params("codex", SESSION));
    assert_eq!(captured["state"]["task_completed"], true);
    assert_eq!(captured["state"]["transcript_complete"], true);
}

#[test]
fn timestamp_filter_is_exclusive_and_preserves_submillisecond_precision() {
    let fixture = Fixture::new();
    fixture.write("codex", SESSION, records(SESSION));
    let mut query = params("codex", SESSION);
    query["after_timestamp"] = json!("2026-09-04T05:00:02-07:00");
    assert_eq!(
        fixture.invoke("session.read_turns", query.clone())["turn_count"],
        3
    );
    query["after_timestamp"] = json!("2026-09-04T12:00:03Z");
    let result = fixture.invoke("session.read_turns", query.clone());
    assert_eq!(result["turn_count"], 1);
    assert_eq!(result["turns"][0]["body"][0]["text"], "Done");
    query["after_timestamp"] = json!("invalid");
    assert_eq!(
        session::handle("session.read_turns", &fixture.request(query))
            .unwrap_err()
            .code,
        "invalid_session_params"
    );
}

#[test]
fn incomplete_tail_is_not_mistaken_for_complete_snapshot_or_corrupt_history() {
    let fixture = Fixture::new();
    let path = fixture.write("codex", SESSION, records(SESSION));
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":")
        .unwrap();
    let result = fixture.invoke("session.read_turns", params("codex", SESSION));
    assert_eq!(result["complete"], false);
    assert_eq!(result["turn_count"], 3);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let failure = session::handle(
        "session.read_turns",
        &fixture.request(params("codex", SESSION)),
    )
    .unwrap_err();
    assert_eq!(failure.code, "codex_rollout_invalid");
    assert!(!failure.message.contains("Do the work"));
}

#[test]
fn account_selection_isolates_locate_read_capture_and_enumerate() {
    let fixture = Fixture::new();
    fixture.write("codex", SESSION, records(SESSION));
    fixture.write("codex2", OTHER, records(OTHER));
    assert_eq!(
        fixture.invoke("session.locate_transcript", params("codex2", SESSION)),
        json!({"located":false})
    );
    for operation in ["session.read_turns", "session.capture"] {
        assert_eq!(
            session::handle(operation, &fixture.request(params("codex2", SESSION)))
                .unwrap_err()
                .code,
            "codex_session_not_found"
        );
    }
    for (account, id) in [("codex", SESSION), ("codex2", OTHER)] {
        let enumeration = fixture.invoke(
            "session.enumerate",
            json!({"settings_id":account,"include_cwd":true,"include_turn_count":true}),
        );
        assert_eq!(enumeration["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(enumeration["sessions"][0]["provider_session_id"], id);
        assert_eq!(enumeration["sessions"][0]["cwd"], "/workspace");
        assert_eq!(enumeration["sessions"][0]["turn_count"], 3);
        assert_eq!(enumeration["complete"], true);
    }
}

#[test]
fn live_capture_requires_matching_invocation_workspace_and_consistent_identity() {
    let fixture = Fixture::new();
    fixture.write("codex", SESSION, records(SESSION));
    let mut query = json!({"settings_id":"codex","invocation_uuid":"invoke-1",
        "live_report":{"provider_session_id":SESSION,"invocation_uuid":"invoke-1"},
        "launch":{"session":{"provider_session_id":SESSION}}});
    let result = fixture.invoke("session.capture", query.clone());
    assert_eq!(result["provider_session_id"], SESSION);
    assert_eq!(result["state"]["source"], "live_report.provider_session_id");
    query["session_id"] = json!(OTHER);
    assert_eq!(
        session::handle("session.capture", &fixture.request(query.clone()))
            .unwrap_err()
            .code,
        "invalid_session_params"
    );
    query.as_object_mut().unwrap().remove("session_id");
    let mut request = fixture.request(query.clone());
    request.host.working_directory = Some("/another-workspace".into());
    assert_eq!(
        session::handle("session.capture", &request)
            .unwrap_err()
            .code,
        "invalid_session_params"
    );
    query["invocation_uuid"] = json!("wrong");
    assert_eq!(
        session::handle("session.capture", &fixture.request(query))
            .unwrap_err()
            .code,
        "invalid_session_params"
    );
    assert!(
        fixture.invoke("session.capture", json!({"settings_id":"codex"}))["provider_session_id"]
            .is_null()
    );
    assert!(session::handle(
        "session.capture",
        &fixture.request(json!({"settings_id":"codex","live_report":{}}))
    )
    .is_err());
}

#[test]
fn enumeration_cursor_is_account_bound_and_rejects_changed_population() {
    let fixture = Fixture::new();
    fixture.write("codex", SESSION, records(SESSION));
    fixture.write("codex", OTHER, records(OTHER));
    fixture.write("codex2", SESSION, records(SESSION));
    let first = fixture.invoke(
        "session.enumerate",
        json!({"settings_id":"codex","limit":1}),
    );
    assert_eq!(first["complete"], false);
    let next = json!({"settings_id":"codex","limit":1,"cursor":first["next_cursor"]});
    let second = fixture.invoke("session.enumerate", next.clone());
    assert_eq!(second["complete"], true);
    assert_ne!(
        first["sessions"][0]["provider_session_id"],
        second["sessions"][0]["provider_session_id"]
    );
    let mut other_account = next.clone();
    other_account["settings_id"] = json!("codex2");
    assert_eq!(
        session::handle("session.enumerate", &fixture.request(other_account))
            .unwrap_err()
            .code,
        "codex_session_cursor_stale"
    );
    fixture.write("codex", CHILD, records(CHILD));
    assert_eq!(
        session::handle("session.enumerate", &fixture.request(next))
            .unwrap_err()
            .code,
        "codex_session_cursor_stale"
    );
}

#[test]
fn user_observation_retains_user_ids_with_bounded_body_tail() {
    let fixture = Fixture::new();
    let mut data = records(SESSION);
    for i in 6..=9 {
        data.push(message(
            "user",
            &format!("2026-09-04T12:00:{i:02}Z"),
            &format!("request {i}"),
            None,
        ));
    }
    fixture.write("codex", SESSION, data);
    let read = fixture.invoke("session.read_turns",json!({"settings_id":"codex","session_id":SESSION,"turn_projection":"user_observation","body_tail_limit":2}));
    assert_eq!(read["turn_count"], 5);
    let turns = read["turns"].as_array().unwrap();
    assert!(turns
        .iter()
        .all(|turn| turn["role"] == "user" && turn.get("native").is_none()));
    assert!(turns[..3].iter().all(|turn| turn.get("body").is_none()));
    assert!(turns[3..].iter().all(|turn| turn.get("body").is_some()));
    assert_eq!(
        fixture.invoke("session.capture", params("codex", SESSION))["state"]["task_completed"],
        false
    );
}

#[cfg(unix)]
#[test]
fn symlinked_rollouts_cannot_cross_account_boundaries() {
    let fixture = Fixture::new();
    let path = fixture.write("codex2", SESSION, records(SESSION));
    let root = fixture.home.path().join(".codex/sessions");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&path, root.join(path.file_name().unwrap())).unwrap();
    std::os::unix::fs::symlink(path.parent().unwrap(), root.join("foreign")).unwrap();
    assert_eq!(
        fixture.invoke("session.locate_transcript", params("codex", SESSION)),
        json!({"located":false})
    );
    assert!(
        fixture.invoke("session.enumerate", json!({"settings_id":"codex"}))["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn duplicate_metadata_identity_and_path_injection_are_rejected() {
    let fixture = Fixture::new();
    fixture.write("codex", SESSION, records(SESSION));
    fixture.write("codex", OTHER, records(SESSION));
    assert_eq!(
        session::handle(
            "session.locate_transcript",
            &fixture.request(params("codex", SESSION))
        )
        .unwrap_err()
        .code,
        "codex_session_ambiguous"
    );
    assert_eq!(
        session::handle(
            "session.locate_transcript",
            &fixture.request(params("codex", "../codex2/session"))
        )
        .unwrap_err()
        .code,
        "invalid_session_params"
    );
}

#[test]
fn export_and_replace_are_explicitly_unsupported() {
    let fixture = Fixture::new();
    for operation in ["session.export", "session.replace"] {
        assert_eq!(
            session::handle(operation, &fixture.request(params("codex", SESSION)))
                .unwrap_err()
                .category,
            "unsupported"
        );
    }
}

#[test]
fn session_result_shapes_match_the_provider_contract() {
    let fixture = Fixture::new();
    fixture.write("codex", SESSION, records(SESSION));
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("contract/v1");
    let session_schema: Value =
        serde_json::from_slice(&fs::read(root.join("session.schema.json")).unwrap()).unwrap();
    let common: Value =
        serde_json::from_slice(&fs::read(root.join("common.schema.json")).unwrap()).unwrap();
    for (operation, definition, query) in [
        (
            "session.locate_transcript",
            "SessionLocateTranscriptResult",
            params("codex", SESSION),
        ),
        (
            "session.read_turns",
            "SessionReadTurnsResult",
            json!({"settings_id":"codex","session_id":SESSION,
                "read_protocol":"oulipoly.session_turn_pages/v1","turn_projection":"canonical_ingest",
                "start_mode":"beginning","after_token":null,"snapshot_id":null,"page_token":null,
                "max_turns":256,"max_response_bytes":524288,"max_source_bytes":8388608,"max_inline_body_bytes":65536}),
        ),
        (
            "session.read_turns",
            "SessionReadTurnsResult",
            json!({"settings_id":"codex","session_id":SESSION,
                "read_protocol":"oulipoly.session_turn_pages/v1","turn_projection":"user_observation",
                "expected_delivery_nonce":"a".repeat(64),
                "start_mode":"beginning","after_token":null,"snapshot_id":null,"page_token":null,
                "max_turns":256,"max_response_bytes":524288,"max_source_bytes":8388608,"max_inline_body_bytes":65536}),
        ),
        (
            "session.capture",
            "SessionCaptureResult",
            params("codex", SESSION),
        ),
        (
            "session.enumerate",
            "SessionEnumerateResult",
            json!({"settings_id":"codex","include_cwd":true,"include_turn_count":true}),
        ),
    ] {
        let schema = json!({"$ref":format!("https://contract.test/session.schema.json#/$defs/{definition}")});
        let validator = jsonschema::JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_document(
                "https://contract.test/session.schema.json".into(),
                session_schema.clone(),
            )
            .with_document(
                "https://contract.test/common.schema.json".into(),
                common.clone(),
            )
            .compile(&schema)
            .unwrap();
        let result = fixture.invoke(operation, query);
        if let Err(errors) = validator.validate(&result) {
            panic!(
                "{operation}: {}",
                errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ")
            );
        };
    }
}
