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

fn padded_record(kind: &str, bytes: usize) -> String {
    let mut value = json!({"type":kind,"payload":{"padding":""}});
    let overhead = value.to_string().len() + 1;
    value["payload"]["padding"] = json!("x".repeat(bytes - overhead));
    let line = format!("{value}\n");
    assert_eq!(line.len(), bytes);
    line
}

fn exact_metadata(f: &Fixture, bytes: usize) {
    let mut value = json!({"timestamp":"2026-09-04T12:00:00Z","type":"session_meta","payload":{"id":ID,"cwd":"/workspace","padding":""}});
    let overhead = value.to_string().len() + 1;
    value["payload"]["padding"] = json!("x".repeat(bytes - overhead));
    let line = format!("{value}\n");
    assert_eq!(line.len(), bytes);
    fs::write(&f.path, line).unwrap();
}

fn append_raw(f: &Fixture, bytes: &[u8]) {
    fs::OpenOptions::new()
        .append(true)
        .open(&f.path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

#[test]
fn age343_existing_page3_sequence18_crosses_exact_remaining_budget_barrier() {
    let f = Fixture::new();
    exact_metadata(&f, 22_030);
    for _ in 0..18 {
        f.append("user", "prefix");
    }
    append_raw(&f, padded_record("compacted", 1_036_666).as_bytes());
    f.append("user", "after compaction");
    f.append("assistant", "answer");
    let original = fs::read(&f.path).unwrap();
    let mut p = f.params();
    p["max_turns"] = json!(6);
    let mut request = p.clone();
    for index in 0..3 {
        let page = f.read(request);
        assert_eq!(page["page_index"], index);
        assert_eq!(page["page_turn_count"], 6);
        request = f.continuation(&p, &page);
    }
    // This is the existing old-format budget-bound page3 / sequence18 boundary.
    let mut seen = Vec::new();
    let mut completed = false;
    for index in 3..9 {
        let page = f.read(request.clone());
        assert_eq!(page, f.read(request)); // interrupted/replayed request is identical
        assert_eq!(page["page_index"], index);
        assert_eq!(page["page_start_sequence"], 18 + seen.len());
        assert!(page["source_bytes_examined"].as_u64().unwrap() <= 1_048_576);
        seen.extend(page["turns"].as_array().unwrap().iter().cloned());
        if page["snapshot_complete"] == true {
            completed = true;
            break;
        }
        assert!(page["scan_progress"] == true || page["page_turn_count"].as_u64().unwrap() > 0);
        request = f.continuation(&p, &page);
    }
    assert!(completed);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["body"][0]["text"], "after compaction");
    assert_eq!(seen[1]["body"][0]["text"], "answer");
    assert_ne!(seen[0]["turn_id"], seen[1]["turn_id"]);
    assert_eq!(fs::read(&f.path).unwrap(), original);
}

fn bounded_drain(f: &Fixture, p: &Value, limit: usize) -> (Vec<Value>, Value) {
    let mut request = p.clone();
    let mut turns = Vec::new();
    for _ in 0..limit {
        let page = f.read(request.clone());
        assert_eq!(page, f.read(request));
        assert!(
            page["source_bytes_examined"].as_u64().unwrap()
                <= p["max_source_bytes"].as_u64().unwrap()
        );
        assert!(
            serde_json::to_vec(&success_response("page-test", page.clone()))
                .unwrap()
                .len()
                + 1
                <= p["max_response_bytes"].as_u64().unwrap() as usize
        );
        turns.extend(page["turns"].as_array().unwrap().iter().cloned());
        if page["snapshot_complete"] == true {
            return (turns, page);
        }
        assert!(page["scan_progress"] == true || page["page_turn_count"].as_u64().unwrap() > 0);
        request = f.continuation(p, &page);
    }
    panic!("bounded drain did not reach snapshot EOF");
}

#[test]
fn age343_remaining_quantum_and_record_ceiling_neighbors() {
    for size in [
        1_026_545, 1_026_546, 1_026_547, 1_048_577, 3_000_000, 8_388_608,
    ] {
        let f = Fixture::new();
        exact_metadata(&f, 22_030);
        append_raw(&f, padded_record("compacted", size).as_bytes());
        f.append("user", "eligible");
        let original = fs::read(&f.path).unwrap();
        let (turns, _) = bounded_drain(&f, &f.params(), 12);
        assert_eq!(turns.len(), 1, "record size {size}");
        assert_eq!(turns[0]["turn_id"], format!("{ID}:byte:{}", 22_030 + size));
        assert_eq!(fs::read(&f.path).unwrap(), original);
    }
}

#[test]
fn age343_projected_multiquantum_body_keeps_exact_digests_and_order() {
    let f = Fixture::new();
    exact_metadata(&f, 22_030);
    let text = "a\\\"\r\n".repeat(250_000);
    f.append("user", &text);
    f.append("assistant", "following");
    let (turns, _) = bounded_drain(&f, &f.params(), 16);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["body_state"], "omitted_oversize");
    #[derive(Serialize)]
    struct Chunk<'a> {
        #[serde(rename = "type")]
        kind: &'a str,
        text: &'a str,
    }
    let body = serde_json::to_vec(&[Chunk {
        kind: "text",
        text: &text,
    }])
    .unwrap();
    assert_eq!(turns[0]["body_bytes"], body.len());
    assert_eq!(turns[0]["body_sha256"], sha256_hex(&body));
    assert_eq!(
        turns[0]["canonical_text_sha256"],
        sha256_hex(text.replace("\r\n", "\n").trim().as_bytes())
    );
    assert_eq!(turns[1]["body"][0]["text"], "following");
    assert_eq!(turns[0]["turn_id"], format!("{ID}:byte:22030"));
}

#[test]
fn age343_unsupported_ceiling_is_bounded_and_never_skips_to_following_turn() {
    let f = Fixture::new();
    append_raw(&f, padded_record("compacted", 8_388_609).as_bytes());
    f.append("user", "must not be silently reached");
    let p = f.params();
    let mut request = p.clone();
    let mut stopped = false;
    for _ in 0..12 {
        match session::handle("session.read_turns", &f.request(request.clone())) {
            Ok(page) => {
                assert_eq!(page["snapshot_complete"], false);
                assert_eq!(page["turns"], json!([]));
                request = f.continuation(&p, &page);
            }
            Err(error) => {
                assert_eq!(error.code, "session_turn_record_ceiling_exceeded");
                assert!(!error.retryable);
                assert_eq!(
                    session::handle("session.read_turns", &f.request(request))
                        .unwrap_err()
                        .code,
                    error.code
                );
                stopped = true;
                break;
            }
        }
    }
    assert!(stopped);
}

#[test]
fn age343_multiquantum_partial_eof_resumes_after_append_without_losing_prefix() {
    let f = Fixture::new();
    let record = padded_record("compacted", 2_500_000);
    append_raw(&f, &record.as_bytes()[..2_000_000]);
    let p = f.params();
    let (turns, end) = bounded_drain(&f, &p, 6);
    assert!(turns.is_empty());
    let mut resumed = p.clone();
    resumed["after_token"] = end["resume_token"].clone();
    let still_partial = f.read(resumed.clone());
    assert_eq!(still_partial["snapshot_complete"], true);
    assert_eq!(still_partial["turns"], json!([]));
    append_raw(&f, &record.as_bytes()[2_000_000..]);
    f.append("user", "appended");
    let (turns, _) = bounded_drain(&f, &resumed, 6);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["body"][0]["text"], "appended");
}

#[test]
fn age343_malformed_complete_record_is_not_a_nonmessage_skip() {
    let f = Fixture::new();
    append_raw(&f, format!("{}!\n", " ".repeat(1_100_000)).as_bytes());
    f.append("user", "must not be reached");
    let p = f.params();
    let first = f.read(p.clone());
    assert_eq!(first["snapshot_complete"], false);
    assert_eq!(first["scan_progress"], true);
    let request = f.continuation(&p, &first);
    let error = session::handle("session.read_turns", &f.request(request)).unwrap_err();
    assert_eq!(error.code, "invalid_session_read_turns_params");
}

#[test]
fn age343_staged_prefix_preserves_identity_generation_and_integrity_fences() {
    for mutation in [
        "provider",
        "account",
        "projection",
        "nonce",
        "budget",
        "protocol",
        "truncate",
        "inode",
        "cursor_digest",
        "staging_digest",
    ] {
        let f = Fixture::new();
        append_raw(&f, padded_record("compacted", 1_100_000).as_bytes());
        f.append("user", "next");
        let mut p = f.params();
        if mutation == "nonce" {
            p["turn_projection"] = json!("user_observation");
            p["expected_delivery_nonce"] = json!("a".repeat(64));
        }
        let first = f.read(p.clone());
        assert_eq!(first["scan_progress"], true);
        let mut request = f.request(f.continuation(&p, &first));
        match mutation {
            "provider" => request.provider_instance_id = Some("other".into()),
            "account" => {
                let other = f.root.path().join(".codex2/sessions");
                fs::create_dir_all(&other).unwrap();
                fs::copy(&f.path, other.join(format!("rollout-test-{ID}.jsonl"))).unwrap();
                request.params["settings_id"] = json!("codex2");
            }
            "projection" => {
                request.params["turn_projection"] = json!("user_observation");
                request.params["expected_delivery_nonce"] = json!("a".repeat(64));
            }
            "nonce" => request.params["expected_delivery_nonce"] = json!("b".repeat(64)),
            "budget" => request.params["max_source_bytes"] = json!(2_097_152),
            "protocol" => request.params["read_protocol"] = json!("oulipoly.session_turn_pages/v2"),
            "truncate" => fs::OpenOptions::new()
                .write(true)
                .open(&f.path)
                .unwrap()
                .set_len(200)
                .unwrap(),
            "inode" => {
                let replacement = f.path.with_extension("replacement");
                fs::copy(&f.path, &replacement).unwrap();
                fs::rename(replacement, &f.path).unwrap();
            }
            "cursor_digest" | "staging_digest" => {
                let root = f
                    .root
                    .path()
                    .join("state/provider-state/codex/session-pages-v1");
                let path = if mutation == "cursor_digest" {
                    let token = first["next_page_token"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("codex-stp1-")
                        .unwrap();
                    root.join(format!("{token}.json"))
                } else {
                    fs::read_dir(root)
                        .unwrap()
                        .map(|p| p.unwrap().path())
                        .find(|p| p.extension().unwrap() == "part")
                        .unwrap()
                };
                let mut bytes = fs::read(&path).unwrap();
                bytes[0] ^= 1;
                fs::write(path, bytes).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            session::handle("session.read_turns", &request).is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn age343_metadata_budget_and_partial_prefix_without_read_allowance_fail_closed() {
    let f = Fixture::new();
    exact_metadata(&f, 1_048_576);
    f.append("user", "next");
    let error = session::handle("session.read_turns", &f.request(f.params())).unwrap_err();
    assert_eq!(error.code, "session_turn_page_budget_too_small");
    exact_metadata(&f, 1_048_577);
    let error = session::handle("session.read_turns", &f.request(f.params())).unwrap_err();
    assert_eq!(error.code, "session_turn_page_budget_too_small");
}

#[test]
fn age343_canonical_backfill_does_not_change_native_resume_argv_authority() {
    use agent_runner_codex::{
        launch::native_args,
        policy::{Plan, RuntimeConfig},
    };
    use std::collections::BTreeMap;
    let f = Fixture::new();
    append_raw(&f, padded_record("compacted", 1_036_666).as_bytes());
    f.append("user", "canonical-only-sentinel");
    let config = RuntimeConfig {
        codex_bin: "/unused/codex".into(),
        bun_bin: "/unused/bun".into(),
        bash_mcp_path: "/unused/agent-bash-mcp.ts".into(),
        system_prompt_file: "/unused/system.md".into(),
        agent_bash_bin: "/unused/agent-bash".into(),
        agent_runner_bin: "/unused/agents".into(),
    };
    let plan = Plan {
        settings_id: "codex".into(),
        model: "gpt-6-astra".into(),
        effort: "high".into(),
        prompt: "current task".into(),
        env: BTreeMap::new(),
        argv: vec![],
    };
    // Pure argv composition; neither provider nor native CLI is launched.
    let before = native_args(&config, &plan, &BTreeMap::new(), Some(ID));
    let (turns, _) = bounded_drain(&f, &f.params(), 5);
    assert_eq!(turns.len(), 1);
    let after = native_args(&config, &plan, &BTreeMap::new(), Some(ID));
    assert_eq!(before, after);
    assert_eq!(&after[after.len() - 3..], &["resume", ID, "-"]);
    assert!(!after
        .iter()
        .any(|arg| arg.contains("canonical-only-sentinel")));
}

#[test]
fn age343_legacy_cursor_without_staging_fields_resumes_at_exact_record_boundary() {
    let f = Fixture::new();
    exact_metadata(&f, 22_030);
    for _ in 0..18 {
        f.append("user", "prefix");
    }
    let boundary = fs::metadata(&f.path).unwrap().len();
    append_raw(&f, padded_record("compacted", 1_036_666).as_bytes());
    f.append("user", "after legacy checkpoint");
    let mut p = f.params();
    p["max_turns"] = json!(6);
    let page = f.read(p.clone());
    let root = f
        .root
        .path()
        .join("state/provider-state/codex/session-pages-v1");
    let token = page["next_page_token"]
        .as_str()
        .unwrap()
        .strip_prefix("codex-stp1-")
        .unwrap();
    // Manufacture an OLD-schema checkpoint in this synthetic fixture only.
    // No provider-state, DB or cursor from the real incident is read or changed.
    let mut legacy: Value =
        serde_json::from_slice(&fs::read(root.join(format!("{token}.json"))).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("partial_record");
    legacy["offset"] = json!(boundary);
    legacy["page"] = json!(3);
    legacy["sequence"] = json!(18);
    let bytes = serde_json::to_vec(&legacy).unwrap();
    let digest = sha256_hex(&bytes);
    fs::write(root.join(format!("{digest}.json")), bytes).unwrap();
    let mut request = f.continuation(&p, &page);
    request["page_token"] = json!(format!("codex-stp1-{digest}"));
    let first = f.read(request.clone());
    assert_eq!(first, f.read(request));
    assert_eq!(first["page_index"], 3);
    assert_eq!(first["page_start_sequence"], 18);
    assert_eq!(first["source_bytes_examined"], 1_048_576);
    assert_eq!(first["snapshot_complete"], false);
    assert_eq!(first["scan_progress"], true);
    let next = f.read(f.continuation(&p, &first));
    assert_eq!(next["page_start_sequence"], 18);
    assert_eq!(
        next["turns"][0]["body"][0]["text"],
        "after legacy checkpoint"
    );
    assert_eq!(next["snapshot_complete"], true);
}
