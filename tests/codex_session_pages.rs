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
    fn request_value(&self, p: Value) -> Value {
        json!({"contract":"oulipoly.provider/v1","request_id":"page-test","provider_instance_id":"codex-provider","host":{"app":"test","data_root":self.root.path().join("state"),"env":{"HOME":self.root.path()}},"params":p})
    }
    fn request(&self, p: Value) -> RequestEnvelope {
        serde_json::from_value(self.request_value(p)).unwrap()
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
                    root.join(format!("cursors-{}.pack", &token[..2]))
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
    let mut legacy: Value = serde_json::from_slice(
        fs::read(root.join(format!("cursors-{}.pack", &token[..2])))
            .unwrap()
            .split(|b| *b == b'\n')
            .find(|line| line.starts_with(token.as_bytes()))
            .unwrap()
            .get(65..)
            .unwrap(),
    )
    .unwrap();
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

fn observation_params(f: &Fixture) -> Value {
    let mut p = f.params();
    p["turn_projection"] = json!("user_observation");
    p["expected_delivery_nonce"] = json!("a".repeat(64));
    p["max_source_bytes"] = json!(512);
    p["max_response_bytes"] = json!(524_288);
    p
}

fn staging_root(f: &Fixture) -> PathBuf {
    f.root
        .path()
        .join("state/provider-state/codex/session-pages-v1")
}

fn exhaust_staging(f: &Fixture) -> PathBuf {
    let root = staging_root(f);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("unrelated-retained-fixture");
    // Sparse, synthetic file: exercises the production logical-byte admission
    // boundary without allocating a large body or reading production state.
    fs::File::create(&path)
        .unwrap()
        .set_len(536_870_912)
        .unwrap();
    path
}

fn observation_io(page: &Value, budget: usize, expected_metadata: usize) -> (usize, usize) {
    let warnings = page["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    let fields: Vec<_> = warnings[0]
        .as_str()
        .unwrap()
        .strip_prefix("codex_observation_io_v1:")
        .unwrap()
        .split(';')
        .collect();
    assert_eq!(fields.len(), 3);
    let forward: usize = fields[0].strip_prefix("forward=").unwrap().parse().unwrap();
    let reconstruction: usize = fields[1]
        .strip_prefix("reconstruction=")
        .unwrap()
        .parse()
        .unwrap();
    let metadata: usize = fields[2]
        .strip_prefix("metadata=")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(metadata, expected_metadata);
    assert!(forward + metadata <= budget);
    assert!(reconstruction < 8_388_608);
    assert_eq!(
        page["source_bytes_examined"],
        forward + metadata + reconstruction
    );
    (forward, reconstruction)
}

fn header_len(f: &Fixture) -> usize {
    fs::read(&f.path)
        .unwrap()
        .iter()
        .position(|b| *b == b'\n')
        .unwrap()
        + 1
}

#[test]
fn age347_exhausted_staging_tail_beginning_continuation_and_canonical_preservation() {
    let f = Fixture::new();
    append_raw(&f, padded_record("compacted", 900).as_bytes());
    f.append("user", "retained");
    // Real canonical cursor + immutable prefix, preserved while unrelated
    // retained evidence brings the shared pool above production admission.
    let mut canonical = f.params();
    canonical["max_source_bytes"] = json!(512);
    let checkpoint = f.read(canonical.clone());
    let canonical_next = f.continuation(&canonical, &checkpoint);
    let retained: Vec<_> = fs::read_dir(staging_root(&f))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert_eq!(retained.len(), 2);
    let unrelated = exhaust_staging(&f);
    assert_eq!(
        session::handle("session.read_turns", &f.request(canonical_next.clone()))
            .unwrap_err()
            .code,
        "session_turn_staging_capacity_exceeded"
    );
    assert_eq!(f.read(canonical.clone()), checkpoint);
    let original = fs::read(&f.path).unwrap();
    let p = observation_params(&f);
    let mut tail_p = p.clone();
    tail_p["start_mode"] = json!("tail");
    let tail = observation_subprocess(&f, &tail_p);
    assert_eq!(tail, f.read(tail_p));
    assert_eq!(tail["snapshot_complete"], true);
    assert_eq!(tail["source_final"], false);
    assert_eq!(tail["turns"], json!([]));
    assert_eq!(observation_io(&tail, 512, header_len(&f)).1, 0);
    assert!(tail["resume_token"]
        .as_str()
        .unwrap()
        .starts_with("codex-obs1-"));
    assert_eq!(original, fs::read(&f.path).unwrap());
    f.append(
        "user",
        &format!(
            "accepted completion\n[OULIPOLY-DELIVERY {}]",
            "a".repeat(64)
        ),
    );
    f.append("assistant", "already answered");
    let mut resumed = p.clone();
    resumed["after_token"] = tail["resume_token"].clone();
    let mut request = resumed.clone();
    let mut seen = Vec::new();
    let mut complete = false;
    for _ in 0..10 {
        let page = f.read(request.clone());
        assert_eq!(page, f.read(request));
        observation_io(&page, 512, header_len(&f));
        seen.extend(page["turns"].as_array().unwrap().iter().cloned());
        if page["snapshot_complete"] == true {
            complete = true;
            break;
        }
        request = f.continuation(&resumed, &page);
    }
    assert!(complete);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["body"][0]["text"], "accepted completion");
    // Missing legacy anchors recover via bounded beginning, not another submit.
    let mut request = p.clone();
    let mut seen = Vec::new();
    let mut complete = false;
    for _ in 0..12 {
        let page = f.read(request.clone());
        assert_eq!(page, f.read(request));
        observation_io(&page, 512, header_len(&f));
        seen.extend(page["turns"].as_array().unwrap().iter().cloned());
        if page["snapshot_complete"] == true {
            complete = true;
            break;
        }
        request = f.continuation(&p, &page);
    }
    assert!(complete);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["body"][0]["text"], "retained");
    assert_eq!(seen[1]["body"][0]["text"], "accepted completion");
    for (path, bytes) in retained {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(fs::metadata(unrelated).unwrap().len(), 536_870_912);
    assert_eq!(fs::read_dir(staging_root(&f)).unwrap().count(), 3);
    assert_eq!(
        session::handle("session.read_turns", &f.request(canonical_next))
            .unwrap_err()
            .code,
        "session_turn_staging_capacity_exceeded"
    );
    // The source grew; old continuation replay, not a new beginning snapshot,
    // preserves the original canonical token and immutable prefix.
    assert_eq!(checkpoint["warnings"], json!([]));
}

#[test]
#[ignore = "offline fixture subprocess only; parent supplies isolated JSON paths"]
fn age347_observation_page_subprocess_fixture() {
    let request_path = std::env::var("AGE347_REQUEST_PATH").unwrap();
    let result_path = std::env::var("AGE347_RESULT_PATH").unwrap();
    let request: RequestEnvelope =
        serde_json::from_slice(&fs::read(request_path).unwrap()).unwrap();
    let result = session::handle("session.read_turns", &request).unwrap();
    fs::write(result_path, serde_json::to_vec(&result).unwrap()).unwrap();
}

fn observation_subprocess(f: &Fixture, p: &Value) -> Value {
    let input = f.root.path().join("synthetic-request.json");
    let output = f.root.path().join("synthetic-result.json");
    fs::write(
        &input,
        serde_json::to_vec(&f.request_value(p.clone())).unwrap(),
    )
    .unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "age347_observation_page_subprocess_fixture",
            "--ignored",
        ])
        .env("AGE347_REQUEST_PATH", &input)
        .env("AGE347_RESULT_PATH", &output)
        .status()
        .unwrap();
    assert!(status.success());
    serde_json::from_slice(&fs::read(output).unwrap()).unwrap()
}

#[test]
fn age347_large_inline_multiquantum_replay_after_process_restart_counts_native_rereads() {
    let f = Fixture::new();
    let unrelated = exhaust_staging(&f);
    // 60,000 varied printable bytes cannot be carried in the <=4096 byte token.
    let mut text = String::new();
    for i in 0u32..938 {
        text.push_str(&sha256_hex(&i.to_le_bytes()));
    }
    text.truncate(60_000);
    f.append(
        "user",
        &format!("{text}\n[OULIPOLY-DELIVERY {}]", "a".repeat(64)),
    );
    let source = fs::read(&f.path).unwrap();
    let header = header_len(&f);
    let record_len = source.len() - header;
    let p = observation_params(&f);
    let mut request = p.clone();
    let mut last_request = None;
    let mut last_page = None;
    let mut pages = 0;
    for index in 0..256 {
        let page = if index == 0 {
            observation_subprocess(&f, &request)
        } else {
            f.read(request.clone())
        };
        assert_eq!(page, f.read(request.clone()));
        assert_eq!(page["page_index"], index);
        let (forward, reconstruction) = observation_io(&page, 512, header);
        let expected_prefix = index * (512 - header);
        assert_eq!(reconstruction, expected_prefix);
        assert_eq!(forward, (record_len - expected_prefix).min(512 - header));
        for field in ["snapshot_id", "next_page_token", "resume_token"] {
            if let Some(token) = page[field].as_str() {
                assert!(token.len() <= 4096);
            }
        }
        assert!(
            serde_json::to_vec(&success_response("page-test", page.clone()))
                .unwrap()
                .len()
                < 524_288
        );
        pages += 1;
        if page["snapshot_complete"] == true {
            last_request = Some(request);
            last_page = Some(page);
            break;
        }
        assert_eq!(page["turns"], json!([]));
        assert_eq!(page["scan_progress"], true);
        request = f.continuation(&p, &page);
    }
    assert!(pages > 100);
    let last_request = last_request.expect("bounded progress reaches final record");
    let last = last_page.unwrap();
    assert_eq!(last["page_turn_count"], 1);
    assert_eq!(last["turns"][0]["body_state"], "inline");
    assert_eq!(last["turns"][0]["body"][0]["text"], text);
    assert_eq!(last["turns"][0]["turn_id"], format!("{ID}:byte:{header}"));
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
    assert_eq!(last["turns"][0]["body_bytes"], body.len());
    assert_eq!(last["turns"][0]["body_sha256"], sha256_hex(&body));
    assert_eq!(
        last["turns"][0]["canonical_text_sha256"],
        sha256_hex(text.as_bytes())
    );
    assert!(last["source_bytes_examined"].as_u64().unwrap() > 60_000);
    // The key and authenticated token are sufficient in an independent process;
    // the final request reproduces identical serialized page bytes after restart.
    let restarted = observation_subprocess(&f, &last_request);
    assert_eq!(
        serde_json::to_vec(&restarted).unwrap(),
        serde_json::to_vec(&last).unwrap()
    );
    f.append("user", "later distinct completion");
    assert_eq!(last, observation_subprocess(&f, &last_request));
    assert!(fs::read(&f.path).unwrap().starts_with(&source));
    assert_eq!(fs::metadata(unrelated).unwrap().len(), 536_870_912);
    assert_eq!(fs::read_dir(staging_root(&f)).unwrap().count(), 1);
    let auth = staging_root(&f)
        .parent()
        .unwrap()
        .join("observation-auth-v1");
    assert_eq!(fs::read_dir(&auth).unwrap().count(), 1);
    assert_eq!(fs::metadata(auth.join("key")).unwrap().len(), 32);
}

#[test]
fn age347_observation_identity_and_source_rejection_never_manufactures_a_turn() {
    for mutation in [
        "provider",
        "account",
        "session",
        "nonce",
        "snapshot",
        "budget",
        "projection",
        "tamper",
        "truncate",
        "replace",
        "rewrite",
        "prefix_rewrite_and_append",
        "unavailable",
        "missing_key",
        "short_key",
    ] {
        let f = Fixture::new();
        exhaust_staging(&f);
        f.append("user", &"x".repeat(1200));
        let p = observation_params(&f);
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
            "session" => {
                let other_id = "22222222-2222-4222-8222-222222222222";
                let source = fs::read_to_string(&f.path).unwrap().replace(ID, other_id);
                fs::write(
                    f.path
                        .parent()
                        .unwrap()
                        .join(format!("rollout-test-{other_id}.jsonl")),
                    source,
                )
                .unwrap();
                request.params["session_id"] = json!(other_id);
            }
            "nonce" => request.params["expected_delivery_nonce"] = json!("b".repeat(64)),
            "snapshot" => request.params["snapshot_id"] = json!("b".repeat(64)),
            "budget" => request.params["max_source_bytes"] = json!(1024),
            "projection" => {
                request.params["turn_projection"] = json!("canonical_ingest");
                request
                    .params
                    .as_object_mut()
                    .unwrap()
                    .remove("expected_delivery_nonce");
            }
            "tamper" => {
                let mut token = first["next_page_token"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec();
                token[30] = if token[30] == b'A' { b'B' } else { b'A' };
                request.params["page_token"] = json!(String::from_utf8(token).unwrap());
            }
            "truncate" => fs::OpenOptions::new()
                .write(true)
                .open(&f.path)
                .unwrap()
                .set_len(header_len(&f) as u64)
                .unwrap(),
            "replace" => {
                let other = f.path.with_extension("replacement");
                fs::copy(&f.path, &other).unwrap();
                fs::rename(other, &f.path).unwrap();
            }
            "rewrite" | "prefix_rewrite_and_append" => {
                let mut bytes = fs::read(&f.path).unwrap();
                let index = bytes
                    .windows(8)
                    .position(|bytes| bytes == b"xxxxxxxx")
                    .unwrap();
                bytes[index] = b'y';
                if mutation == "prefix_rewrite_and_append" {
                    bytes.extend_from_slice(b"\n");
                }
                fs::write(&f.path, bytes).unwrap();
            }
            "unavailable" => fs::remove_file(&f.path).unwrap(),
            "missing_key" => fs::remove_file(
                staging_root(&f)
                    .parent()
                    .unwrap()
                    .join("observation-auth-v1/key"),
            )
            .unwrap(),
            "short_key" => fs::write(
                staging_root(&f)
                    .parent()
                    .unwrap()
                    .join("observation-auth-v1/key"),
                b"short",
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(
            session::handle("session.read_turns", &request).is_err(),
            "{mutation}"
        );
        assert_eq!(fs::read_dir(staging_root(&f)).unwrap().count(), 1);
    }
}

#[test]
fn age347_observation_partial_eof_append_and_wrong_marker_preserve_uncertainty() {
    let f = Fixture::new();
    exhaust_staging(&f);
    let record = format!(
        "{}\n",
        json!({"timestamp":"2026-09-04T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("{}\n[OULIPOLY-DELIVERY {}]", "z".repeat(1200), "b".repeat(64))}]}})
    );
    let split = record.len() - 5;
    append_raw(&f, &record.as_bytes()[..split]);
    let p = observation_params(&f);
    let mut request = p.clone();
    let mut end = None;
    for _ in 0..10 {
        let page = f.read(request.clone());
        assert_eq!(page["turns"], json!([]));
        observation_io(&page, 512, header_len(&f));
        if page["snapshot_complete"] == true {
            end = Some(page);
            break;
        }
        request = f.continuation(&p, &page);
    }
    let end = end.unwrap();
    let mut resume = p.clone();
    resume["after_token"] = end["resume_token"].clone();
    let still_partial = f.read(resume.clone());
    assert_eq!(still_partial["snapshot_complete"], true);
    assert_eq!(still_partial["turns"], json!([]));
    assert_eq!(
        observation_io(&still_partial, 512, header_len(&f)),
        (0, split)
    );
    append_raw(&f, &record.as_bytes()[split..]);
    let completed = f.read(resume);
    assert_eq!(completed["page_turn_count"], 1);
    assert!(completed["turns"][0]["body"][0]["text"]
        .as_str()
        .unwrap()
        .ends_with(&format!("[OULIPOLY-DELIVERY {}]", "b".repeat(64))));
    assert_eq!(observation_io(&completed, 512, header_len(&f)), (5, split));
    assert_eq!(fs::read_dir(staging_root(&f)).unwrap().count(), 1);
}

#[test]
fn age347_observation_concurrent_start_uses_one_durable_key_no_staging_admission() {
    let f = Fixture::new();
    exhaust_staging(&f);
    f.append("user", "exact concurrent turn");
    let request = f.request(observation_params(&f));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let request = request.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                session::handle("session.read_turns", &request).unwrap()
            })
        })
        .collect();
    let pages: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(pages.iter().all(|page| page == &pages[0]));
    assert_eq!(pages[0]["page_turn_count"], 1);
    assert_eq!(fs::read_dir(staging_root(&f)).unwrap().count(), 1);
    let auth = staging_root(&f)
        .parent()
        .unwrap()
        .join("observation-auth-v1");
    assert_eq!(fs::read_dir(&auth).unwrap().count(), 1);
    assert_eq!(fs::metadata(auth.join("key")).unwrap().len(), 32);
}

#[test]
fn combined_legacy_and_packed_observation_prefix_upgrade_preserves_snapshot_and_dependencies() {
    for packed in [false, true] {
        let f = Fixture::new();
        let text = "x".repeat(1200);
        f.append("user", &text);
        let mut canonical = f.params();
        canonical["max_source_bytes"] = json!(512);
        canonical["max_response_bytes"] = json!(524_288);
        let first = f.read(canonical.clone());
        let digest = first["next_page_token"]
            .as_str()
            .unwrap()
            .strip_prefix("codex-stp1-")
            .unwrap();
        let pack =
            fs::read(staging_root(&f).join(format!("cursors-{}.pack", &digest[..2]))).unwrap();
        let mut legacy: Value = serde_json::from_slice(
            pack.split(|b| *b == b'\n')
                .find(|line| line.starts_with(digest.as_bytes()))
                .unwrap()
                .get(65..)
                .unwrap(),
        )
        .unwrap();
        legacy["binding"]["projection"] = json!("user_observation");
        legacy["binding"]["nonce"] = json!("a".repeat(64));
        let bytes = serde_json::to_vec(&legacy).unwrap();
        let digest = sha256_hex(&bytes);
        let path = if packed {
            let path = staging_root(&f).join(format!("cursors-{}.pack", &digest[..2]));
            let mut out = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(
                out,
                "{digest} {}",
                String::from_utf8(bytes.clone()).unwrap()
            )
            .unwrap();
            path
        } else {
            let path = staging_root(&f).join(format!("{digest}.json"));
            fs::write(&path, &bytes).unwrap();
            path
        };
        let retained: Vec<_> = fs::read_dir(staging_root(&f))
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        exhaust_staging(&f);
        // Canonical packed replay survives at cap; a new partial dependency
        // cannot be admitted, so success below is not spare canonical capacity.
        assert_eq!(f.read(canonical.clone()), first);
        let blocked = f.continuation(&canonical, &first);
        assert_eq!(
            session::handle("session.read_turns", &f.request(blocked))
                .unwrap_err()
                .code,
            "session_turn_staging_capacity_exceeded"
        );
        let p = observation_params(&f);
        let mut request = f.continuation(&p, &first);
        request["page_token"] = json!(format!("codex-stp1-{digest}"));
        let page = f.read(request.clone());
        assert_eq!(page, observation_subprocess(&f, &request));
        assert!(page["next_page_token"]
            .as_str()
            .unwrap()
            .starts_with("codex-obs1-"));
        assert_eq!(
            observation_io(&page, 512, header_len(&f)).1,
            512 - header_len(&f)
        );
        // Appending cannot extend the issued continuation's frozen snapshot.
        f.append("user", "after frozen snapshot");
        assert_eq!(page, f.read(request.clone()));
        let mut wrong_nonce = request.clone();
        wrong_nonce["expected_delivery_nonce"] = json!("b".repeat(64));
        assert_eq!(
            session::handle("session.read_turns", &f.request(wrong_nonce))
                .unwrap_err()
                .code,
            "session_turn_page_token_stale"
        );
        let mut current = page;
        let mut turns = Vec::new();
        for _ in 0..8 {
            observation_io(&current, 512, header_len(&f));
            turns.extend(current["turns"].as_array().unwrap().iter().cloned());
            if current["snapshot_complete"] == true {
                break;
            }
            current = f.read(f.continuation(&p, &current));
        }
        assert_eq!(current["snapshot_complete"], true);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["body"][0]["text"], text);
        let mut resume = p.clone();
        resume["after_token"] = current["resume_token"].clone();
        let appended = f.read(resume);
        assert_eq!(
            appended["turns"][0]["body"][0]["text"],
            "after frozen snapshot"
        );
        assert!(path.exists());
        for (path, bytes) in retained {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }
}

#[test]
fn age347_observation_keeps_record_ceiling_and_response_bounds_under_exhaustion() {
    let validator = paging_result_schema();
    for record_size in [8_388_608, 8_388_609] {
        let f = Fixture::new();
        exhaust_staging(&f);
        append_raw(&f, padded_record("compacted", record_size).as_bytes());
        f.append("user", &"z".repeat(12000));
        let mut p = observation_params(&f);
        p["max_source_bytes"] = json!(1_048_576);
        p["max_response_bytes"] = json!(4096);
        let mut request = p.clone();
        let mut terminal = false;
        let mut reached_extra_native_reads = false;
        for _ in 0..12 {
            match session::handle("session.read_turns", &f.request(request.clone())) {
                Ok(page) => {
                    observation_io(&page, 1_048_576, header_len(&f));
                    if page["source_bytes_examined"].as_u64().unwrap() > 8_388_608 {
                        reached_extra_native_reads = true;
                        assert_eq!(page["source_bytes_examined"], 8_388_608 + header_len(&f));
                        println!("observation ceiling: record={record_size}; native_total={}; warnings={}", page["source_bytes_examined"], page["warnings"]);
                        let mut canonical_negative = page.clone();
                        canonical_negative["turn_projection"] = json!("canonical_ingest");
                        canonical_negative["warnings"] = json!([]);
                        assert!(!validator.is_valid(&canonical_negative));
                    }
                    assert_page_schema(&validator, &page);
                    assert!(
                        serde_json::to_vec(&success_response("page-test", page.clone()))
                            .unwrap()
                            .len()
                            < 4096
                    );
                    if page["snapshot_complete"] == true {
                        assert_eq!(record_size, 8_388_608);
                        assert_eq!(page["page_turn_count"], 1);
                        assert_eq!(page["turns"][0]["body_state"], "omitted_oversize");
                        terminal = true;
                        break;
                    }
                    assert_eq!(page["turns"], json!([]));
                    request = f.continuation(&p, &page);
                }
                Err(error) => {
                    assert_eq!(record_size, 8_388_609);
                    assert_eq!(error.code, "session_turn_record_ceiling_exceeded");
                    terminal = true;
                    break;
                }
            }
        }
        assert!(terminal);
        if record_size == 8_388_608 {
            assert!(reached_extra_native_reads);
        }
        assert_eq!(fs::read_dir(staging_root(&f)).unwrap().count(), 1);
    }
}

fn paging_result_schema() -> jsonschema::JSONSchema {
    let schema: Value =
        serde_json::from_str(include_str!("../contract/v1/session.schema.json")).unwrap();
    let common: Value =
        serde_json::from_str(include_str!("../contract/v1/common.schema.json")).unwrap();
    jsonschema::JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_document("https://contract.test/session.schema.json".into(), schema)
        .with_document("https://contract.test/common.schema.json".into(), common)
        .compile(&json!({"$ref":"https://contract.test/session.schema.json#/$defs/SessionReadTurnsResult"}))
        .unwrap()
}

fn assert_page_schema(validator: &jsonschema::JSONSchema, page: &Value) {
    if let Err(errors) = validator.validate(page) {
        panic!(
            "page contract: {}",
            errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ")
        );
    }
}

#[test]
fn observation_result_contract_preserves_projection_specific_native_limits() {
    let validator = paging_result_schema();
    let f = Fixture::new();
    f.append("user", "schema control");
    let canonical = f.read(f.params());
    assert_page_schema(&validator, &canonical);
    let mut candidate = canonical.clone();
    candidate["source_bytes_examined"] = json!(8_388_608);
    assert_page_schema(&validator, &candidate);
    candidate["source_bytes_examined"] = json!(8_388_609);
    assert!(!validator.is_valid(&candidate));

    let observation = f.read(observation_params(&f));
    assert_page_schema(&validator, &observation);
    // Structural boundary controls; arithmetic against each actual request is
    // separately checked by observation_io on executed provider pages.
    let mut candidate = observation.clone();
    candidate["source_bytes_examined"] = json!(16_777_215);
    candidate["warnings"] =
        json!(["codex_observation_io_v1:forward=1;reconstruction=8388607;metadata=8388607"]);
    assert_page_schema(&validator, &candidate);
    candidate["source_bytes_examined"] = json!(16_777_216);
    assert!(!validator.is_valid(&candidate));
    for total in [json!(-1), json!(1.5)] {
        candidate["source_bytes_examined"] = total;
        assert!(!validator.is_valid(&candidate));
    }
    for warnings in [
        json!([]),
        json!(["not accounting"]),
        json!(["codex_observation_io_v1:forward=-1;reconstruction=0;metadata=0"]),
        json!([
            "codex_observation_io_v1:forward=1;reconstruction=0;metadata=0",
            "codex_observation_io_v1:forward=1;reconstruction=0;metadata=0"
        ]),
    ] {
        candidate = observation.clone();
        candidate["warnings"] = warnings;
        assert!(!validator.is_valid(&candidate));
    }
    candidate = canonical;
    candidate["unexpected"] = json!(true);
    assert!(!validator.is_valid(&candidate));
}

#[test]
fn continuation_replay_ignores_unrelated_rollout_discovery_for_both_projections() {
    for observation in [true, false] {
        for fallback in [false, true] {
            let mut f = Fixture::new();
            if fallback {
                let path = f
                    .path
                    .parent()
                    .unwrap()
                    .join("rollout-nonstandard-target.jsonl");
                fs::rename(&f.path, &path).unwrap();
                f.path = path;
            }
            f.append("user", &"x".repeat(1800));
            let original = fs::read(&f.path).unwrap();
            let mut p = if observation {
                observation_params(&f)
            } else {
                f.params()
            };
            p["max_source_bytes"] = json!(512);
            let first = f.read(p.clone());
            let request = f.continuation(&p, &first);
            let before = f.read(request.clone());
            let key = if observation {
                Some(
                    fs::read(
                        staging_root(&f)
                            .parent()
                            .unwrap()
                            .join("observation-auth-v1/key"),
                    )
                    .unwrap(),
                )
            } else {
                None
            };
            let unrelated = f.path.parent().unwrap().join("rollout-unrelated.jsonl");
            fs::write(&unrelated, format!("{}\n", json!({"type":"session_meta","payload":{"id":"unrelated-session","cwd":"/other"}}))).unwrap();
            let after = f.read(request.clone());
            assert!(before == after, "same continuation must survive unrelated discovery: observation={observation}, fallback={fallback}");
            if observation {
                assert_eq!(
                    observation_io(&after, 512, header_len(&f)),
                    (512 - header_len(&f), 512 - header_len(&f))
                );
                assert!(after == observation_subprocess(&f, &request));
                assert_eq!(
                    key.unwrap(),
                    fs::read(
                        staging_root(&f)
                            .parent()
                            .unwrap()
                            .join("observation-auth-v1/key")
                    )
                    .unwrap()
                );
            } else {
                assert_eq!(after["source_bytes_examined"], 512);
            }
            // Even an unrelated header larger than the request's remaining
            // budget must never be opened/charged when a source is already bound.
            fs::write(&unrelated, format!("{}\n", json!({"type":"session_meta","payload":{"id":"unrelated-session","padding":"z".repeat(1000)}}))).unwrap();
            assert!(before == f.read(request.clone()));
            assert_eq!(original, fs::read(&f.path).unwrap());
            let mut seen = Vec::new();
            let mut request = request;
            let mut complete = false;
            for _ in 0..10 {
                let page = f.read(request);
                seen.extend(page["turns"].as_array().unwrap().iter().cloned());
                if page["snapshot_complete"] == true {
                    complete = true;
                    break;
                }
                request = f.continuation(&p, &page);
            }
            assert!(complete);
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0]["body"][0]["text"], "x".repeat(1800));
        }
    }
}

fn near_limit_inline_fixture() -> (Fixture, Value, String) {
    let f = Fixture::new();
    let header = fs::read(&f.path).unwrap();
    let mut p = observation_params(&f);
    p["max_source_bytes"] = json!(1_048_576);
    p["max_response_bytes"] = json!(4096);
    p["max_turns"] = json!(10);
    // Find the envelope boundary using a complete one-turn neighbor. This
    // compensates only for fixed-width platform timestamp/token serialization,
    // not for the behavior of the partial-prefix candidate under test.
    let (mut low, mut high) = (0, 4096);
    while low + 1 < high {
        let size = (low + high) / 2;
        fs::write(&f.path, &header).unwrap();
        f.append("user", &"x".repeat(size));
        if f.read(p.clone())["turns"][0]["body_state"] == "inline" {
            low = size;
        } else {
            high = size;
        }
    }
    assert!(low > 256);
    let text = "x".repeat(low - 32);
    fs::write(&f.path, header).unwrap();
    f.append("user", &text);
    let control = f.read(p.clone());
    assert_eq!(control["turns"][0]["body_state"], "inline");
    let size = serde_json::to_vec(&success_response("page-test", control))
        .unwrap()
        .len()
        + 1;
    assert!(
        (4030..=4096).contains(&size),
        "neighbor response bytes={size}"
    );
    (f, p, text)
}

#[test]
fn observation_final_cursor_fits_near_limit_turn_and_partial_trailer() {
    let validator = paging_result_schema();
    for at_eof in [true, false] {
        let (f, mut p, text) = near_limit_inline_fixture();
        exhaust_staging(&f);
        let header = header_len(&f);
        let boundary = fs::metadata(&f.path).unwrap().len() as usize;
        let record = format!(
            "{}\n",
            json!({"timestamp":"2026-09-04T12:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"next notification"}]}})
        );
        let split = record.len() / 2;
        append_raw(
            &f,
            if at_eof {
                &record.as_bytes()[..split]
            } else {
                record.as_bytes()
            },
        );
        p["max_source_bytes"] = json!(boundary + split);
        let source = fs::read(&f.path).unwrap();
        let page = f.read(p.clone());
        assert_page_schema(&validator, &page);
        let response_bytes = serde_json::to_vec(&success_response("page-test", page.clone()))
            .unwrap()
            .len()
            + 1;
        assert!(response_bytes <= 4096);
        println!("partial-trailer observation: at_eof={at_eof}; response_bytes={response_bytes}; body_state={}; warnings={}", page["turns"][0]["body_state"], page["warnings"]);
        assert_eq!(page["page_turn_count"], 1);
        assert_eq!(page["snapshot_complete"], at_eof);
        assert_eq!(page["source_final"], false);
        assert_eq!(page["turns"][0]["turn_id"], format!("{ID}:byte:{header}"));
        assert_eq!(
            page["turns"][0]["canonical_text_sha256"],
            sha256_hex(text.as_bytes())
        );
        let body = format!("[{{\"type\":\"text\",\"text\":\"{text}\"}}]");
        assert_eq!(page["turns"][0]["body_bytes"], body.len());
        assert_eq!(page["turns"][0]["body_sha256"], sha256_hex(body.as_bytes()));
        assert_eq!(
            observation_io(&page, boundary + split, header),
            (boundary + split - header, 0)
        );
        assert!(page == observation_subprocess(&f, &p));
        assert_eq!(source, fs::read(&f.path).unwrap());
        // Sufficient-space neighbor retains the identical inline turn and prefix.
        let mut roomy = p.clone();
        roomy["max_response_bytes"] = json!(8192);
        let control = f.read(roomy);
        assert_eq!(control["turns"][0]["body_state"], "inline");
        assert_eq!(control["turns"][0]["body"][0]["text"], text);
        let next = if at_eof {
            let mut resume = p.clone();
            resume["after_token"] = page["resume_token"].clone();
            let unfinished = f.read(resume.clone());
            assert_eq!(unfinished["turns"], json!([]));
            assert_eq!(
                observation_io(&unfinished, boundary + split, header),
                (0, split)
            );
            append_raw(&f, &record.as_bytes()[split..]);
            resume
        } else {
            f.continuation(&p, &page)
        };
        let second = f.read(next.clone());
        assert_eq!(second["page_turn_count"], 1);
        assert_eq!(second["snapshot_complete"], true);
        assert_eq!(second["turns"][0]["body"][0]["text"], "next notification");
        assert_eq!(
            second["turns"][0]["turn_id"],
            format!("{ID}:byte:{boundary}")
        );
        assert_eq!(
            observation_io(&second, boundary + split, header),
            (record.len() - split, split)
        );
        assert!(second == observation_subprocess(&f, &next));
        assert_eq!(fs::read_dir(staging_root(&f)).unwrap().count(), 1);
    }
}

#[test]
fn observation_metadata_only_page_keeps_fitting_boundary_and_charges_unused_suffix() {
    let f = Fixture::new();
    exhaust_staging(&f);
    f.append("user", &"x".repeat(128));
    let boundary = fs::metadata(&f.path).unwrap().len() as usize;
    f.append("user", &"y".repeat(1200));
    let mut p = observation_params(&f);
    p["max_inline_body_bytes"] = json!(0);
    p["max_source_bytes"] = json!(boundary);
    // Smallest envelope admitting one metadata-only turn, at a framed boundary
    // before frozen EOF. Both source quanta below have the same decimal width.
    let (mut low, mut high) = (1023, 4096);
    while low + 1 < high {
        let budget = (low + high) / 2;
        p["max_response_bytes"] = json!(budget);
        if session::handle("session.read_turns", &f.request(p.clone())).is_ok() {
            high = budget;
        } else {
            low = budget;
        }
    }
    // Allow later decimal counter/body-length growth, but not the additional
    // authenticated partial-prefix object (over 100 encoded bytes).
    high += 32;
    p["max_response_bytes"] = json!(high);
    let control = f.read(p.clone());
    assert_eq!(control["page_turn_count"], 1);
    assert_eq!(control["snapshot_complete"], false);
    assert_eq!(
        boundary.to_string().len(),
        (boundary + 80).to_string().len()
    );
    p["max_source_bytes"] = json!(boundary + 80);
    let page = f.read(p.clone());
    assert_eq!(page["turns"], control["turns"]);
    assert_eq!(page["snapshot_complete"], false);
    assert_eq!(
        observation_io(&page, boundary + 80, header_len(&f)),
        (boundary + 80 - header_len(&f), 0)
    );
    assert!(
        serde_json::to_vec(&success_response("page-test", page.clone()))
            .unwrap()
            .len()
            < high
    );
    assert!(page == observation_subprocess(&f, &p));
    let mut request = f.continuation(&p, &page);
    let mut seen = Vec::new();
    let mut finished = false;
    for index in 0..10 {
        let page = f.read(request.clone());
        let (_, reconstruction) = observation_io(&page, boundary + 80, header_len(&f));
        // The first suffix was genuinely read/charged, but not checkpointed:
        // the next call starts at that complete boundary, without a prefix.
        if index == 0 {
            assert_eq!(reconstruction, 0);
        }
        assert!(page == f.read(request));
        assert!(
            serde_json::to_vec(&success_response("page-test", page.clone()))
                .unwrap()
                .len()
                < high
        );
        seen.extend(page["turns"].as_array().unwrap().iter().cloned());
        if page["snapshot_complete"] == true {
            finished = true;
            break;
        }
        request = f.continuation(&p, &page);
    }
    assert!(finished);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["turn_id"], format!("{ID}:byte:{boundary}"));
    assert_eq!(
        seen[0]["canonical_text_sha256"],
        sha256_hex("y".repeat(1200).as_bytes())
    );
}
