use agent_runner_codex::encoding::sha256_hex;
use agent_runner_codex::envelope::{success_response, RequestEnvelope};
use agent_runner_codex::session;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

const ID: &str = "11111111-1111-4111-8111-111111111111";

struct Fixture {
    root: TempDir,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".codex/sessions/2026/09/04");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-test-{ID}.jsonl"));
        fs::write(&path, format!("{}\n", json!({"timestamp":"2026-09-04T12:00:00Z","type":"session_meta","payload":{"id":ID,"cwd":"/workspace"}}))).unwrap();
        Self { root, path }
    }
    fn append(&self, role: &str, text: &str) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .unwrap();
        writeln!(file,"{}",json!({"timestamp":"2026-09-04T12:00:01Z","type":"response_item","payload":{"type":"message","role":role,"content":[{"type":"input_text","text":text}]}})).unwrap();
    }
    fn params(&self) -> Value {
        json!({"settings_id":"codex","session_id":ID,"read_protocol":"oulipoly.session_turn_pages/v1","turn_projection":"canonical_ingest","start_mode":"beginning","after_token":null,"snapshot_id":null,"page_token":null,"max_turns":1,"max_response_bytes":4096,"max_source_bytes":1048576,"max_inline_body_bytes":65536})
    }
    fn request(&self, p: Value) -> RequestEnvelope {
        serde_json::from_value(json!({"contract":"oulipoly.provider/v1","request_id":"page-test","provider_instance_id":"codex-provider","host":{"app":"test","data_root":self.root.path().join("state"),"env":{"HOME":self.root.path()}},"params":p})).unwrap()
    }
    fn read(&self, p: Value) -> Value {
        session::handle("session.read_turns", &self.request(p)).unwrap()
    }
    fn continuation(&self, p: &Value, page: &Value) -> Value {
        let mut next = p.clone();
        next["start_mode"] = json!("continuation");
        next["after_token"] = Value::Null;
        next["snapshot_id"] = page["snapshot_id"].clone();
        next["page_token"] = page["next_page_token"].clone();
        next
    }
}

#[test]
fn deterministic_pages_replay_and_complete_with_bounded_envelope() {
    let f = Fixture::new();
    f.append("user", "first");
    f.append("assistant", "second");
    let original = fs::read(&f.path).unwrap();
    let p = f.params();
    let first = f.read(p.clone());
    assert_eq!(first, f.read(p.clone()));
    assert_eq!(first["page_turn_count"], 1);
    assert_eq!(first["snapshot_complete"], false);
    let next = f.continuation(&p, &first);
    let second = f.read(next.clone());
    assert_eq!(second, f.read(next));
    assert_eq!(second["page_index"], 1);
    assert_eq!(second["page_start_sequence"], 1);
    assert_eq!(second["snapshot_complete"], true);
    assert_ne!(first["turns"][0]["turn_id"], second["turns"][0]["turn_id"]);
    assert!(
        serde_json::to_vec(&success_response("page-test", first))
            .unwrap()
            .len()
            + 1
            <= 4096
    );
    assert_eq!(original, fs::read(&f.path).unwrap());
}

#[test]
fn continuation_and_resume_bind_account_projection_nonce_and_budgets() {
    let f = Fixture::new();
    f.append("user", "first");
    f.append("user", "second");
    let p = f.params();
    let first = f.read(p.clone());
    let next = f.continuation(&p, &first);
    let mut changed = next.clone();
    changed["max_turns"] = json!(2);
    assert_eq!(
        session::handle("session.read_turns", &f.request(changed))
            .unwrap_err()
            .code,
        "session_turn_page_token_stale"
    );
    let other = f.root.path().join(".codex2/sessions");
    fs::create_dir_all(&other).unwrap();
    fs::copy(&f.path, other.join(format!("rollout-test-{ID}.jsonl"))).unwrap();
    let mut changed = next.clone();
    changed["settings_id"] = json!("codex2");
    assert_eq!(
        session::handle("session.read_turns", &f.request(changed))
            .unwrap_err()
            .code,
        "session_turn_page_token_stale"
    );
    let mut changed = next;
    changed["turn_projection"] = json!("user_observation");
    changed["expected_delivery_nonce"] = json!("a".repeat(64));
    assert_eq!(
        session::handle("session.read_turns", &f.request(changed))
            .unwrap_err()
            .code,
        "session_turn_page_token_stale"
    );
}

#[test]
fn host_body_serialization_and_canonical_digest_match() {
    let f = Fixture::new();
    f.append("user", " one\r\ntwo ");
    let page = f.read(f.params());
    let turn = &page["turns"][0];
    #[derive(Serialize)]
    struct HostChunk<'a> {
        #[serde(rename = "type")]
        kind: &'a str,
        text: Option<&'a str>,
    }
    let bytes = serde_json::to_vec(&[HostChunk {
        kind: "text",
        text: Some(" one\r\ntwo "),
    }])
    .unwrap();
    assert_eq!(turn["body_bytes"], bytes.len());
    assert_eq!(turn["body_sha256"], sha256_hex(&bytes));
    assert_eq!(turn["canonical_text_sha256"], sha256_hex(b"one\ntwo"));
}

#[test]
fn omitted_bodies_preserve_digests_and_response_limit() {
    let f = Fixture::new();
    f.append("user", &"x".repeat(12000));
    let mut p = f.params();
    p["max_inline_body_bytes"] = json!(10);
    p["max_response_bytes"] = json!(1800);
    let page = f.read(p);
    assert_eq!(page["turns"][0]["body_state"], "omitted_oversize");
    assert!(page["turns"][0]["body"].is_null());
    assert_eq!(page["turns"][0]["body_sha256"].as_str().unwrap().len(), 64);
    assert!(
        serde_json::to_vec(&success_response("page-test", page))
            .unwrap()
            .len()
            + 1
            <= 1800
    );
}

#[test]
fn tail_anchor_then_append_returns_only_new_user_and_strips_matching_marker() {
    let f = Fixture::new();
    f.append("user", "old");
    let mut p = f.params();
    p["start_mode"] = json!("tail");
    p["turn_projection"] = json!("user_observation");
    let nonce = "a".repeat(64);
    p["expected_delivery_nonce"] = json!(nonce);
    let tail = f.read(p.clone());
    assert_eq!(tail["snapshot_complete"], true);
    assert_eq!(tail["scan_progress"], false);
    assert_eq!(tail["turns"], json!([]));
    f.append("assistant", "skip");
    f.append("user", &format!("new task\n[OULIPOLY-DELIVERY {nonce}]"));
    p["start_mode"] = json!("beginning");
    p["after_token"] = tail["resume_token"].clone();
    let page = f.read(p);
    assert_eq!(page["turns"][0]["body"][0]["text"], "new task");
    assert_eq!(page["page_start_sequence"], 0);
}

#[test]
fn partial_final_record_is_retried_when_append_finishes_it() {
    let f = Fixture::new();
    f.append("user", "old");
    let record=json!({"timestamp":"2026-09-04T12:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"new"}]}}).to_string();
    let split = record.len() / 2;
    let mut file = fs::OpenOptions::new().append(true).open(&f.path).unwrap();
    write!(file, "{}", &record[..split]).unwrap();
    file.flush().unwrap();
    let mut p = f.params();
    p["max_turns"] = json!(10);
    let page = f.read(p.clone());
    assert_eq!(page["snapshot_complete"], true);
    assert_eq!(page["page_turn_count"], 1);
    writeln!(file, "{}", &record[split..]).unwrap();
    file.flush().unwrap();
    p["after_token"] = page["resume_token"].clone();
    let next = f.read(p);
    assert_eq!(next["page_turn_count"], 1);
    assert_eq!(next["turns"][0]["body"][0]["text"], "new");
}

#[test]
fn small_source_pages_advance_over_non_message_records() {
    let f = Fixture::new();
    for _ in 0..8 {
        f.append("developer", "hidden");
    }
    f.append("user", "visible");
    let mut p = f.params();
    p["max_source_bytes"] = json!(512);
    let mut request = p.clone();
    let mut seen = 0;
    let mut scanned = false;
    for _ in 0..20 {
        let page = f.read(request);
        assert!(page["source_bytes_examined"].as_u64().unwrap() <= 512);
        seen += page["page_turn_count"].as_u64().unwrap();
        scanned |= page["scan_progress"] == true;
        if page["snapshot_complete"] == true {
            break;
        }
        request = f.continuation(&p, &page);
    }
    assert!(scanned);
    assert_eq!(seen, 1);
}

#[test]
fn replaced_source_and_forged_tokens_are_rejected() {
    let f = Fixture::new();
    f.append("user", "first");
    f.append("user", "second");
    let p = f.params();
    let first = f.read(p.clone());
    let next = f.continuation(&p, &first);
    let replacement = f.path.with_extension("replacement");
    fs::copy(&f.path, &replacement).unwrap();
    fs::rename(replacement, &f.path).unwrap();
    assert_eq!(
        session::handle("session.read_turns", &f.request(next))
            .unwrap_err()
            .code,
        "session_turn_page_token_stale"
    );
    let mut forged = p;
    forged["after_token"] = json!(format!("codex-stp1-{}", "0".repeat(64)));
    assert_eq!(
        session::handle("session.read_turns", &f.request(forged))
            .unwrap_err()
            .code,
        "session_turn_page_token_stale"
    );
}

#[test]
fn native_filename_lookup_avoids_unrelated_large_metadata_and_counts_selected_header() {
    let f = Fixture::new();
    f.append("user", "visible");
    fs::write(
        f.path.parent().unwrap().join("rollout-other.jsonl"),
        "x".repeat(2 * 1024 * 1024),
    )
    .unwrap();
    let mut p = f.params();
    p["max_source_bytes"] = json!(512);
    let page = f.read(p.clone());
    assert_eq!(page["page_turn_count"], 1);
    assert_eq!(
        page["source_bytes_examined"],
        fs::metadata(&f.path).unwrap().len()
    );
    p["max_source_bytes"] = json!(10);
    assert_eq!(
        session::handle("session.read_turns", &f.request(p))
            .unwrap_err()
            .code,
        "session_turn_page_budget_too_small"
    );
}
