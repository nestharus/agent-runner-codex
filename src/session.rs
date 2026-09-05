//! Codex rollout discovery and read-only projection into the provider session contract.
//!
//! The first session_meta owns the file. Forked rollouts can include their parent's
//! metadata later, so a later session_meta must never change transcript identity.

use crate::envelope::{ProviderFailure, RequestEnvelope};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const FORMAT_ID: &str = "codex.rollout/jsonl";
const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROLLOUT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ROLLOUTS: usize = 100_000;
const MAX_PAGE_SIZE: usize = 1000;

#[derive(Deserialize)]
struct SessionParams {
    settings_id: String,
    session_id: Option<String>,
    turn_projection: Option<String>,
    body_tail_limit: Option<usize>,
    after_timestamp: Option<String>,
    after_unix_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnumerateParams {
    settings_id: String,
    limit: Option<usize>,
    cursor: Option<String>,
    #[serde(default)]
    include_cwd: bool,
    #[serde(default)]
    include_turn_count: bool,
    since_unix_ms: Option<u64>,
}

struct RolloutMeta {
    id: String,
    cwd: Option<String>,
    created_unix_ms: Option<u64>,
}

struct Rollout {
    meta: RolloutMeta,
    turns: Vec<Value>,
    complete: bool,
    task_completed: Option<bool>,
}

pub fn handle(operation: &str, request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    if operation == "session.read_turns" && request.params.get("read_protocol").is_some() {
        return crate::session_turn_pages::read_turns(request);
    }
    match operation {
        "session.capture" => capture(request),
        "session.enumerate" => enumerate(request),
        "session.locate_transcript" | "session.read_turns" => {
            let params: SessionParams = parse(&request.params, request)?;
            let root = account_home(request, &params.settings_id)?;
            let session_id = params
                .session_id
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| invalid(request, "session_id must be non-empty"))?;
            validate_session_id(session_id, request)?;
            let located = locate(&root, session_id, request)?;
            if operation == "session.locate_transcript" {
                return Ok(match located {
                    Some(path) => json!({"located":true, "path":path,
                        "format_id":FORMAT_ID, "source_id":source_id(session_id),
                        "require_existing_observed":true}),
                    None => json!({"located":false}),
                });
            }
            let path = located.ok_or_else(|| not_found(request))?;
            read_turns(&path, session_id, &params, request)
        }
        "session.export" => Err(ProviderFailure::unsupported(
            &request.request_id,
            "session_export_unsupported",
            "Codex rollout projection cannot preserve a faithful canonical transcript for import",
        )),
        "session.replace" => Err(ProviderFailure::unsupported(
            &request.request_id,
            "session_replace_unsupported",
            "Codex does not expose a stable transcript replacement API",
        )),
        _ => Err(ProviderFailure::unsupported(
            &request.request_id,
            "session_operation_unsupported",
            "Unsupported Codex session operation",
        )),
    }
}

pub(crate) fn account_home(
    request: &RequestEnvelope,
    settings_id: &str,
) -> Result<PathBuf, ProviderFailure> {
    crate::account::home(&request.host, settings_id).map_err(|mut failure| {
        failure.request_id = request.request_id.clone();
        failure
    })
}

fn parse<T: serde::de::DeserializeOwned>(
    value: &Value,
    request: &RequestEnvelope,
) -> Result<T, ProviderFailure> {
    serde_json::from_value(value.clone())
        .map_err(|_| invalid(request, "Invalid session parameters"))
}

fn invalid(request: &RequestEnvelope, message: &str) -> ProviderFailure {
    ProviderFailure::invalid_request(&request.request_id, "invalid_session_params", message)
}

fn io_failure(request: &RequestEnvelope) -> ProviderFailure {
    ProviderFailure::internal(
        &request.request_id,
        "codex_rollout_io",
        "Could not read the selected Codex account's rollout files",
    )
}

fn corrupt(request: &RequestEnvelope) -> ProviderFailure {
    ProviderFailure::internal(
        &request.request_id,
        "codex_rollout_invalid",
        "Codex rollout contains invalid metadata or a malformed complete record",
    )
}

fn not_found(request: &RequestEnvelope) -> ProviderFailure {
    ProviderFailure::invalid_request(
        &request.request_id,
        "codex_session_not_found",
        "Session was not found in the selected Codex account",
    )
}

fn source_id(id: &str) -> String {
    format!("codex.rollout:{id}")
}

fn validate_session_id(id: &str, request: &RequestEnvelope) -> Result<(), ProviderFailure> {
    if id.is_empty()
        || id.len() > 256
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid(request, "Invalid Codex session ID"));
    }
    Ok(())
}

fn rollout_paths(root: &Path, request: &RequestEnvelope) -> Result<Vec<PathBuf>, ProviderFailure> {
    let mut paths = Vec::new();
    let mut pending = vec![root.join("sessions"), root.join("archived_sessions")];
    let mut visited = 0;
    while let Some(dir) = pending.pop() {
        let metadata = match fs::symlink_metadata(&dir) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(io_failure(request)),
        };
        // Account isolation includes refusing symlinks into another account's storage.
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        for entry in fs::read_dir(dir).map_err(|_| io_failure(request))? {
            let entry = entry.map_err(|_| io_failure(request))?;
            let kind = entry.file_type().map_err(|_| io_failure(request))?;
            visited += 1;
            if visited > MAX_ROLLOUTS * 4 {
                return Err(ProviderFailure::internal(
                    &request.request_id,
                    "codex_rollout_capacity",
                    "Codex rollout directory exceeds the scan capacity",
                ));
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                    paths.push(entry.path());
                    if paths.len() > MAX_ROLLOUTS {
                        return Err(ProviderFailure::internal(
                            &request.request_id,
                            "codex_rollout_capacity",
                            "Codex account exceeds the rollout capacity",
                        ));
                    }
                }
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_line(
    reader: &mut impl BufRead,
    request: &RequestEnvelope,
) -> Result<Vec<u8>, ProviderFailure> {
    let mut line = Vec::new();
    reader
        .take(MAX_LINE_BYTES + 1)
        .read_until(b'\n', &mut line)
        .map_err(|_| io_failure(request))?;
    if line.len() as u64 > MAX_LINE_BYTES {
        return Err(ProviderFailure::internal(
            &request.request_id,
            "codex_rollout_capacity",
            "Codex rollout record exceeds the size limit",
        ));
    }
    Ok(line)
}

fn parse_meta(value: &Value, request: &RequestEnvelope) -> Result<RolloutMeta, ProviderFailure> {
    if value["type"] != "session_meta" {
        return Err(corrupt(request));
    }
    let payload = &value["payload"];
    let id = payload["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| corrupt(request))?;
    validate_session_id(id, request).map_err(|_| corrupt(request))?;
    Ok(RolloutMeta {
        id: id.to_owned(),
        cwd: payload["cwd"].as_str().map(str::to_owned),
        created_unix_ms: timestamp_ms(
            payload["timestamp"]
                .as_str()
                .or(value["timestamp"].as_str()),
        ),
    })
}

fn metadata(path: &Path, request: &RequestEnvelope) -> Result<RolloutMeta, ProviderFailure> {
    let mut reader = BufReader::new(File::open(path).map_err(|_| io_failure(request))?);
    loop {
        let line = read_line(&mut reader, request)?;
        if line.is_empty() {
            return Err(corrupt(request));
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        return parse_meta(
            &serde_json::from_slice::<Value>(&line).map_err(|_| corrupt(request))?,
            request,
        );
    }
}

pub(crate) fn locate(
    root: &Path,
    id: &str,
    request: &RequestEnvelope,
) -> Result<Option<PathBuf>, ProviderFailure> {
    let mut found = None;
    for path in rollout_paths(root, request)? {
        // Filename conventions alone are not authoritative, including for forks.
        let meta = match metadata(&path, request) {
            Ok(meta) => meta,
            Err(error)
                if path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(&format!("-{id}.jsonl"))) =>
            {
                return Err(error)
            }
            Err(_) => continue,
        };
        if meta.id == id {
            if found.is_some() {
                return Err(ProviderFailure::conflict(
                    &request.request_id,
                    "codex_session_ambiguous",
                    "More than one rollout claims this Codex session",
                    json!({}),
                ));
            }
            found = Some(path);
        }
    }
    Ok(found)
}

/// Native filenames select candidates; their first metadata record still owns
/// identity. Nonstandard/fork filenames use a bounded metadata fallback.
pub(crate) fn locate_page_source(
    root: &Path,
    id: &str,
    maximum: usize,
    request: &RequestEnvelope,
) -> Result<(File, usize, u64), ProviderFailure> {
    let paths = rollout_paths(root, request)?;
    let suffix = format!("-{id}.jsonl");
    let named: Vec<_> = paths
        .iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(&suffix))
        })
        .collect();
    let candidates: Vec<_> = if named.is_empty() {
        paths.iter().collect()
    } else {
        named
    };
    let mut examined = 0;
    let mut found = None;
    for path in candidates {
        let remaining = maximum.saturating_sub(examined);
        if remaining == 0 {
            return Err(ProviderFailure::invalid_request(
                &request.request_id,
                "session_turn_page_budget_too_small",
                "Source budget cannot admit rollout identity metadata",
            ));
        }
        let mut reader = BufReader::new(
            File::open(path)
                .map_err(|_| io_failure(request))?
                .take(remaining as u64),
        );
        let mut first = Vec::new();
        reader
            .read_until(b'\n', &mut first)
            .map_err(|_| io_failure(request))?;
        examined += first.len();
        if !first.ends_with(b"\n") {
            return Err(ProviderFailure::invalid_request(
                &request.request_id,
                "session_turn_page_budget_too_small",
                "Source budget cannot admit the complete rollout identity record",
            ));
        }
        let meta = serde_json::from_slice::<Value>(&first)
            .map_err(|_| corrupt(request))
            .and_then(|v| parse_meta(&v, request))?;
        if meta.id == id {
            if found.is_some() {
                return Err(ProviderFailure::conflict(
                    &request.request_id,
                    "codex_session_ambiguous",
                    "More than one native rollout claims this session",
                    json!({}),
                ));
            }
            found = Some((reader.into_inner().into_inner(), first.len() as u64));
        }
    }
    found
        .map(|(file, offset)| (file, examined, offset))
        .ok_or_else(|| not_found(request))
}

fn timestamp_ms(value: Option<&str>) -> Option<u64> {
    value
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .and_then(|d| u64::try_from(d.timestamp_millis()).ok())
}

fn read_rollout(
    path: &Path,
    expected: Option<&str>,
    request: &RequestEnvelope,
) -> Result<Rollout, ProviderFailure> {
    let file = File::open(path).map_err(|_| io_failure(request))?;
    let len = file.metadata().map_err(|_| io_failure(request))?.len();
    if len > MAX_ROLLOUT_BYTES {
        return Err(ProviderFailure::internal(
            &request.request_id,
            "codex_rollout_capacity",
            "Codex rollout exceeds the read size limit",
        ));
    }
    // Bound the read to a snapshot even while Codex appends to its rollout.
    let mut reader = BufReader::new(file.take(len));
    let mut meta: Option<RolloutMeta> = None;
    let mut turns = Vec::new();
    let mut complete = true;
    let mut task_completed = None;
    let mut line_no = 0;
    loop {
        let line = read_line(&mut reader, request)?;
        if line.is_empty() {
            break;
        }
        line_no += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) if !line.ends_with(b"\n") => {
                complete = false;
                break;
            }
            Err(_) => return Err(corrupt(request)),
        };
        if meta.is_none() {
            let parsed = parse_meta(&value, request)?;
            if expected.is_some_and(|id| id != parsed.id) {
                return Err(corrupt(request));
            }
            meta = Some(parsed);
            continue;
        }
        let payload = &value["payload"];
        if value["type"] == "event_msg" {
            match payload["type"].as_str() {
                Some("task_complete" | "task_completed" | "turn_completed") => {
                    task_completed = Some(true)
                }
                Some("task_started" | "turn_started" | "turn_aborted" | "error") => {
                    task_completed = Some(false)
                }
                _ => (),
            }
        }
        if value["type"] != "response_item" || payload["type"] != "message" {
            continue;
        }
        let role = match payload["role"].as_str() {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => continue,
        };
        let timestamp = value["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .ok_or_else(|| corrupt(request))?
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::AutoSi, true);
        let id = &meta.as_ref().unwrap().id;
        let turn_id = payload["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{id}:line:{line_no}"));
        let final_answer = payload["phase"] == "final_answer" || payload["phase"] == "final";
        if role == "user" {
            task_completed = Some(false);
        }
        if role == "assistant" && final_answer {
            task_completed = Some(true);
        }
        turns.push(json!({"session_id":id, "turn_id":turn_id, "role":role,
            "timestamp":timestamp, "body":content_chunks(&payload["content"]),
            "native":{"message_id":payload["id"], "phase":payload["phase"],
                "line":line_no, "completed":role == "user" || final_answer}}));
    }
    Ok(Rollout {
        meta: meta.ok_or_else(|| corrupt(request))?,
        turns,
        complete,
        task_completed,
    })
}

pub(crate) fn content_chunks(value: &Value) -> Vec<Value> {
    match value {
        Value::String(text) => vec![json!({"type":"text", "text":text})],
        Value::Array(parts) => parts.iter().flat_map(content_chunks).collect(),
        Value::Object(part) => {
            if let Some(text) = part
                .get("text")
                .or_else(|| part.get("content"))
                .and_then(Value::as_str)
            {
                let kind = match part.get("type").and_then(Value::as_str) {
                    None | Some("input_text" | "output_text") => "text",
                    Some(kind) => kind,
                };
                vec![json!({"type":kind, "text":text})]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn read_turns(
    path: &Path,
    id: &str,
    params: &SessionParams,
    request: &RequestEnvelope,
) -> Result<Value, ProviderFailure> {
    if !matches!(
        params.turn_projection.as_deref(),
        None | Some("user_observation")
    ) {
        return Err(invalid(request, "Unsupported turn_projection"));
    }
    if !matches!(params.body_tail_limit, None | Some(1..=16)) {
        return Err(invalid(request, "body_tail_limit must be between 1 and 16"));
    }
    let after = params
        .after_timestamp
        .as_deref()
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map_err(|_| invalid(request, "after_timestamp must be an RFC3339 timestamp"))
        })
        .transpose()?;
    let rollout = read_rollout(path, Some(id), request)?;
    let mut turns: Vec<Value> = rollout
        .turns
        .into_iter()
        .filter(|turn| {
            let timestamp =
                DateTime::parse_from_rfc3339(turn["timestamp"].as_str().unwrap()).unwrap();
            after.as_ref().is_none_or(|after| timestamp > *after)
                && params.after_unix_ms.is_none_or(|after| {
                    timestamp.timestamp_millis() > i64::try_from(after).unwrap_or(i64::MAX)
                })
                && (params.turn_projection.is_none() || turn["role"] == "user")
        })
        .collect();
    if params.turn_projection.is_some() {
        let body_start = turns
            .len()
            .saturating_sub(params.body_tail_limit.unwrap_or(4));
        for (index, turn) in turns.iter_mut().enumerate() {
            turn.as_object_mut().unwrap().remove("native");
            if index < body_start {
                turn.as_object_mut().unwrap().remove("body");
            }
        }
    }
    Ok(json!({"turn_count":turns.len(), "turns":turns, "complete":rollout.complete}))
}

fn capture(request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    let params: SessionParams = parse(&request.params, request)?;
    let root = account_home(request, &params.settings_id)?;
    if request.params.get("evidence").is_some() {
        return Err(invalid(
            request,
            "The removed evidence field is unsupported",
        ));
    }
    if let Some(report) = request.params.get("live_report") {
        let object = report
            .as_object()
            .ok_or_else(|| invalid(request, "live_report must be an object"))?;
        if object.len() != 2
            || !["provider_session_id", "invocation_uuid"]
                .iter()
                .all(|key| {
                    object
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty())
                })
        {
            return Err(invalid(
                request,
                "live_report requires a session ID and invocation UUID",
            ));
        }
    }
    let candidates = [
        (
            "/live_report/provider_session_id",
            "live_report.provider_session_id",
        ),
        (
            "/launch/session/provider_session_id",
            "launch.session.provider_session_id",
        ),
        ("/session_id", "session_id"),
        ("/pinned_target", "pinned_target"),
        (
            "/start_bound_provider_session_id",
            "start_bound_provider_session_id",
        ),
    ];
    let mut selected: Option<(&str, &str)> = None;
    for (pointer, source) in candidates {
        let Some(value) = request.params.pointer(pointer).filter(|v| !v.is_null()) else {
            continue;
        };
        let id = value
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid(request, "Session capture identity must be non-empty"))?;
        validate_session_id(id, request)?;
        if selected.is_some_and(|(expected, _)| expected != id) {
            return Err(invalid(
                request,
                "Conflicting session capture identity evidence",
            ));
        }
        selected.get_or_insert((id, source));
    }
    let Some((id, source)) = selected else {
        return Ok(
            json!({"provider_session_id":null,"state":{"source":"none","format_id":FORMAT_ID},"artifacts":[]}),
        );
    };
    let path = locate(&root, id, request)?.ok_or_else(|| not_found(request))?;
    let rollout = read_rollout(&path, Some(id), request)?;
    if request.params.get("live_report").is_some() {
        let invocation = request.params["invocation_uuid"]
            .as_str()
            .filter(|s| !s.is_empty());
        if invocation.is_none()
            || invocation
                != request
                    .params
                    .pointer("/live_report/invocation_uuid")
                    .and_then(Value::as_str)
        {
            return Err(invalid(
                request,
                "live_report.invocation_uuid must match invocation_uuid",
            ));
        }
        let cwd = request
            .host
            .working_directory
            .as_deref()
            .filter(|s| !s.is_empty());
        if cwd.is_none() || cwd.map(Path::new) != rollout.meta.cwd.as_deref().map(Path::new) {
            return Err(invalid(
                request,
                "Live report workspace does not match the selected Codex rollout",
            ));
        }
    }
    Ok(json!({"provider_session_id":id,
        "state":{"source":source,"source_id":source_id(id),"format_id":FORMAT_ID,
            "transcript_complete":rollout.complete,"task_completed":rollout.task_completed},
        "artifacts":[{"kind":"codex-rollout","path":path}]}))
}

fn enumerate(request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    let params: EnumerateParams = parse(&request.params, request)?;
    let limit = params.limit.unwrap_or(100);
    if limit == 0 || limit > MAX_PAGE_SIZE {
        return Err(invalid(
            request,
            "Enumeration limit must be between 1 and 1000",
        ));
    }
    let root = account_home(request, &params.settings_id)?;
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut identities = BTreeSet::new();
    for path in rollout_paths(&root, request)? {
        let meta = match metadata(&path, request) {
            Ok(meta) => meta,
            Err(_) => {
                warnings.push("Skipped an unreadable or malformed Codex rollout".to_owned());
                continue;
            }
        };
        if !identities.insert(meta.id.clone()) {
            return Err(ProviderFailure::conflict(
                &request.request_id,
                "codex_session_ambiguous",
                "Multiple rollouts claim the same Codex session",
                json!({}),
            ));
        }
        let file_meta = fs::metadata(&path).map_err(|_| io_failure(request))?;
        let modified = file_meta
            .modified()
            .ok()
            .and_then(|s| s.duration_since(UNIX_EPOCH).ok());
        let updated = modified
            .and_then(|s| u64::try_from(s.as_millis()).ok())
            .or(meta.created_unix_ms);
        let mut turn_count = None;
        if params.include_turn_count {
            let rollout = read_rollout(&path, Some(&meta.id), request)?;
            turn_count = Some(rollout.turns.len());
            if !rollout.complete {
                warnings.push("A Codex rollout has an incomplete final record".to_owned());
            }
        }
        if params
            .since_unix_ms
            .is_some_and(|since| updated.is_some_and(|updated| updated < since))
        {
            continue;
        }
        let mut entry = json!({"provider_session_id":meta.id,
            "created_unix_ms":meta.created_unix_ms,"updated_unix_ms":updated,
            "source":{"kind":"codex.rollout","detail":path.to_string_lossy()}});
        if params.include_cwd {
            entry["cwd"] = json!(meta.cwd);
        }
        if params.include_turn_count {
            entry["turn_count"] = json!(turn_count);
        }
        entries.push(entry);
    }
    entries.sort_by(|a, b| {
        b["updated_unix_ms"]
            .as_u64()
            .cmp(&a["updated_unix_ms"].as_u64())
            .then_with(|| {
                a["provider_session_id"]
                    .as_str()
                    .cmp(&b["provider_session_id"].as_str())
            })
    });
    warnings.sort();
    warnings.dedup();
    // A cursor cannot silently jump between accounts, projections, or changing populations.
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!({
                "root":root,"settings_id":params.settings_id,"entries":entries,
                "include_cwd":params.include_cwd,"include_turn_count":params.include_turn_count,
                "since_unix_ms":params.since_unix_ms,"warnings":warnings
            }))
            .unwrap()
        )
    );
    let offset = match params.cursor.as_deref() {
        None => 0,
        Some(cursor) => {
            let pieces: Vec<_> = cursor.split(':').collect();
            if pieces.len() != 3 || pieces[0] != "codex-v1" || pieces[1] != fingerprint {
                return Err(ProviderFailure::conflict(&request.request_id, "codex_session_cursor_stale", "Enumeration cursor does not match this account or its current session population", json!({})));
            }
            pieces[2]
                .parse::<usize>()
                .ok()
                .filter(|n| *n <= entries.len())
                .ok_or_else(|| invalid(request, "Invalid Codex enumeration cursor"))?
        }
    };
    let end = offset.saturating_add(limit).min(entries.len());
    let complete = end == entries.len();
    let cursor = (!complete).then(|| format!("codex-v1:{fingerprint}:{end}"));
    Ok(
        json!({"sessions":entries[offset..end],"complete":complete,"next_cursor":cursor,"warnings":warnings}),
    )
}
