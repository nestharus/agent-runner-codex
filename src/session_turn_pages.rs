//! Bounded, append-only Codex JSONL paging. Cursor contents remain provider-owned.
use crate::encoding::sha256_hex;
use crate::envelope::{success_response, ProviderFailure, RequestEnvelope};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const PROTOCOL: &str = "oulipoly.session_turn_pages/v1";
const PREFIX: &str = "codex-stp1-";
// Independent of the native-source quantum: at most one bounded record is
// staged between requests. Never skip a record based on its unparsed prefix.
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PartialRecord {
    start: u64,
    sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    settings_id: String,
    session_id: String,
    read_protocol: String,
    turn_projection: String,
    expected_delivery_nonce: Option<String>,
    start_mode: String,
    after_token: Option<String>,
    snapshot_id: Option<String>,
    page_token: Option<String>,
    max_turns: usize,
    max_response_bytes: usize,
    max_source_bytes: usize,
    max_inline_body_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Binding {
    provider: String,
    account: PathBuf,
    settings: String,
    session: String,
    projection: String,
    nonce: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Budgets {
    turns: usize,
    response: usize,
    source: usize,
    inline: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Stamp {
    device: u64,
    inode: u64,
    len: u64,
    modified: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    kind: String,
    binding: Binding,
    budgets: Budgets,
    stamp: Stamp,
    snapshot: String,
    offset: u64,
    page: u64,
    sequence: u64,
    // Absent on all legacy cursors; preserve their byte serialization/tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    partial_record: Option<PartialRecord>,
}

fn invalid(request: &RequestEnvelope, message: &str) -> ProviderFailure {
    ProviderFailure::invalid_request(
        &request.request_id,
        "invalid_session_read_turns_params",
        message,
    )
}

fn stale(request: &RequestEnvelope) -> ProviderFailure {
    ProviderFailure::conflict(&request.request_id, "session_turn_page_token_stale", "Cursor does not match the selected account, session, projection, budgets, or append-only rollout generation", json!({}))
}

fn io_error(request: &RequestEnvelope) -> ProviderFailure {
    ProviderFailure::internal(
        &request.request_id,
        "session_turn_page_io",
        "Could not read or persist bounded session paging state",
    )
}

fn capacity(request: &RequestEnvelope, message: &str) -> ProviderFailure {
    ProviderFailure::invalid_request(
        &request.request_id,
        "session_turn_page_budget_too_small",
        message,
    )
}

fn params(request: &RequestEnvelope) -> Result<Params, ProviderFailure> {
    for key in [
        "settings_id",
        "session_id",
        "read_protocol",
        "turn_projection",
        "start_mode",
        "after_token",
        "snapshot_id",
        "page_token",
        "max_turns",
        "max_response_bytes",
        "max_source_bytes",
        "max_inline_body_bytes",
    ] {
        if request.params.get(key).is_none() {
            return Err(invalid(request, "Missing required paging field"));
        }
    }
    let p: Params = serde_json::from_value(request.params.clone())
        .map_err(|_| invalid(request, "Invalid paging parameters"))?;
    if p.read_protocol != PROTOCOL
        || p.settings_id.is_empty()
        || p.settings_id.len() > 1024
        || p.session_id.is_empty()
        || p.session_id.len() > 256
        || !p
            .session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid(request, "Invalid paging protocol or identity"));
    }
    if !(1..=256).contains(&p.max_turns)
        || !(1024..=524288).contains(&p.max_response_bytes)
        || !(1..=8388608).contains(&p.max_source_bytes)
        || p.max_inline_body_bytes > 65536
    {
        return Err(invalid(
            request,
            "Paging budgets are outside supported bounds",
        ));
    }
    match (
        p.turn_projection.as_str(),
        p.expected_delivery_nonce.as_deref(),
    ) {
        ("canonical_ingest", None) if request.params.get("expected_delivery_nonce").is_none() => (),
        ("user_observation", Some(n))
            if n.len() == 64
                && n.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            ()
        }
        _ => return Err(invalid(request, "Invalid projection or delivery nonce")),
    }
    match p.start_mode.as_str() {
        "beginning" if p.snapshot_id.is_none() && p.page_token.is_none() => (),
        "tail"
            if p.turn_projection == "user_observation"
                && p.after_token.is_none()
                && p.snapshot_id.is_none()
                && p.page_token.is_none() =>
        {
            ()
        }
        "continuation"
            if p.after_token.is_none() && p.snapshot_id.is_some() && p.page_token.is_some() =>
        {
            ()
        }
        _ => {
            return Err(invalid(
                request,
                "Invalid paging start mode or token combination",
            ))
        }
    }
    for token in [&p.after_token, &p.snapshot_id, &p.page_token]
        .into_iter()
        .flatten()
    {
        if token.is_empty() || token.len() > 4096 {
            return Err(invalid(request, "Invalid paging token length"));
        }
    }
    Ok(p)
}

fn stamp(file: &File, request: &RequestEnvelope) -> Result<Stamp, ProviderFailure> {
    let m = file.metadata().map_err(|_| io_error(request))?;
    #[cfg(unix)]
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (m.dev(), m.ino())
    };
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    Ok(Stamp {
        device,
        inode,
        len: m.len(),
        modified: m
            .modified()
            .map_err(|_| io_error(request))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| io_error(request))?
            .as_nanos(),
    })
}

fn require_generation(
    old: &Stamp,
    current: &Stamp,
    request: &RequestEnvelope,
) -> Result<(), ProviderFailure> {
    if old.device != current.device
        || old.inode != current.inode
        || current.len < old.len
        || (current.len == old.len && old.modified != current.modified)
    {
        return Err(stale(request));
    }
    Ok(())
}

fn state_root(request: &RequestEnvelope) -> Result<PathBuf, ProviderFailure> {
    let root = match &request.host.data_root {
        Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
        Some(_) => return Err(invalid(request, "host.data_root must be absolute")),
        None => crate::account::user_home(&request.host)?.join(".local/share/agent-runner-codex"),
    }
    .join("provider-state/codex/session-pages-v1");
    crate::durable_fs::create_private_directories(&root).map_err(|_| io_error(request))?;
    Ok(root)
}

fn token(cursor: &Cursor) -> String {
    format!(
        "{PREFIX}{}",
        sha256_hex(&serde_json::to_vec(cursor).unwrap())
    )
}

fn load(root: &Path, value: &str, request: &RequestEnvelope) -> Result<Cursor, ProviderFailure> {
    let suffix = value
        .strip_prefix(PREFIX)
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| stale(request))?;
    let bytes = crate::durable_fs::read_file_bounded(&root.join(format!("{suffix}.json")), 32768)
        .map_err(|_| stale(request))?;
    if sha256_hex(&bytes) != suffix {
        return Err(stale(request));
    }
    serde_json::from_slice(&bytes).map_err(|_| stale(request))
}

fn persist(root: &Path, cursor: &Cursor, request: &RequestEnvelope) -> Result<(), ProviderFailure> {
    let bytes = serde_json::to_vec(cursor).unwrap();
    let path = root.join(format!("{}.json", sha256_hex(&bytes)));
    if path.exists() {
        if crate::durable_fs::read_file_bounded(&path, 32768).map_err(|_| io_error(request))?
            == bytes
        {
            return Ok(());
        }
        return Err(stale(request));
    }
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|_| io_error(request))?;
    temp.write_all(&bytes).map_err(|_| io_error(request))?;
    temp.as_file().sync_all().map_err(|_| io_error(request))?;
    match temp.persist_noclobber(&path) {
        Ok(_) => (),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err(io_error(request)),
    }
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|_| io_error(request))
}

fn record_limit(request: &RequestEnvelope) -> ProviderFailure {
    ProviderFailure::unsupported(
        &request.request_id,
        "session_turn_record_ceiling_exceeded",
        "JSONL record exceeds the supported 8388608-byte framing ceiling; checkpoint retained",
    )
}

fn partial_bytes(
    root: &Path,
    state: &Cursor,
    request: &RequestEnvelope,
) -> Result<Vec<u8>, ProviderFailure> {
    let Some(partial) = &state.partial_record else {
        return Ok(Vec::new());
    };
    let len = state
        .offset
        .checked_sub(partial.start)
        .ok_or_else(|| stale(request))?;
    if len == 0
        || len >= MAX_RECORD_BYTES as u64
        || partial.sha256.len() != 64
        || !partial
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(stale(request));
    }
    let bytes = crate::durable_fs::read_file_bounded(
        &root.join(format!("record-{}.part", partial.sha256)),
        len as usize,
    )
    .map_err(|_| stale(request))?;
    if bytes.len() as u64 != len || sha256_hex(&bytes) != partial.sha256 || bytes.contains(&b'\n') {
        return Err(stale(request));
    }
    Ok(bytes)
}

fn stage_partial(
    root: &Path,
    start: u64,
    bytes: &[u8],
    request: &RequestEnvelope,
) -> Result<PartialRecord, ProviderFailure> {
    if bytes.len() >= MAX_RECORD_BYTES {
        return Err(record_limit(request));
    }
    let sha256 = sha256_hex(bytes);
    let path = root.join(format!("record-{sha256}.part"));
    // Immutable content-addressed private staging. Publish before its cursor so
    // interruption cannot expose a cursor whose bytes were not synchronized.
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|_| io_error(request))?;
    temp.write_all(bytes).map_err(|_| io_error(request))?;
    temp.as_file().sync_all().map_err(|_| io_error(request))?;
    match temp.persist_noclobber(&path) {
        Ok(_) => (),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = crate::durable_fs::read_file_bounded(&path, bytes.len())
                .map_err(|_| stale(request))?;
            if existing != bytes {
                return Err(stale(request));
            }
        }
        Err(_) => return Err(io_error(request)),
    }
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|_| io_error(request))?;
    Ok(PartialRecord { start, sha256 })
}

#[derive(Serialize)]
struct Chunk<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    text: &'a str,
}

fn project(
    value: &Value,
    p: &Params,
    offset: u64,
    sequence: u64,
    request: &RequestEnvelope,
) -> Result<Option<Value>, ProviderFailure> {
    let payload = &value["payload"];
    if value["type"] != "response_item" || payload["type"] != "message" {
        return Ok(None);
    }
    let role = payload["role"].as_str().unwrap_or("");
    if !matches!(role, "user" | "assistant")
        || (p.turn_projection == "user_observation" && role != "user")
    {
        return Ok(None);
    }
    let timestamp = value["timestamp"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .ok_or_else(|| invalid(request, "Rollout message timestamp is invalid"))?
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::AutoSi, true);
    let mut body = crate::session::content_chunks(&payload["content"]);
    if let Some(nonce) = &p.expected_delivery_nonce {
        let text = body
            .iter()
            .filter_map(|v| v["text"].as_str())
            .collect::<String>();
        let marker = format!("[OULIPOLY-DELIVERY {nonce}]");
        if let Some(prefix) = text.trim_end().strip_suffix(&marker) {
            if prefix.is_empty() || prefix.ends_with(char::is_whitespace) {
                body = vec![json!({"type":"text", "text":prefix.trim_end()})];
            }
        }
    }
    // Serialize the exact generated host chunk field order: type, then text.
    let chunks: Vec<_> = body
        .iter()
        .filter_map(|v| v["text"].as_str().map(|text| Chunk { kind: "text", text }))
        .collect();
    let encoded = serde_json::to_vec(&chunks).unwrap();
    let text = chunks
        .iter()
        .map(|c| c.text)
        .collect::<String>()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let empty = chunks.is_empty();
    let body_state = if empty {
        "absent"
    } else if encoded.len() > p.max_inline_body_bytes {
        "omitted_oversize"
    } else {
        "inline"
    };
    let body_value = if body_state == "inline" {
        serde_json::to_value(&chunks).unwrap()
    } else {
        Value::Null
    };
    Ok(Some(
        json!({"session_id":p.session_id, "turn_id":format!("{}:byte:{offset}",p.session_id), "snapshot_sequence":sequence,
        "timestamp":timestamp,"role":role,"parent_turn_id":null,"is_sidechain":false,"is_compaction_boundary":false,
        "body_state":body_state,"body":body_value,"body_bytes":(!empty).then_some(encoded.len()),
        "body_sha256":(!empty).then(||sha256_hex(&encoded)),"canonical_text_sha256":(!empty).then(||sha256_hex(text.trim().as_bytes()))}),
    ))
}

fn page_result(
    state: &Cursor,
    mut next: Cursor,
    turns: &[Value],
    examined: usize,
    complete: bool,
) -> (Value, Cursor) {
    next.kind = if complete { "resume" } else { "page" }.into();
    next.page = state.page + 1;
    next.sequence = state.sequence + turns.len() as u64;
    let next_token = token(&next);
    (
        json!({"read_protocol":PROTOCOL,"provider_instance_id":state.binding.provider,"settings_id":state.binding.settings,
        "session_id":state.binding.session,"turn_projection":state.binding.projection,"snapshot_id":state.snapshot,
        "page_index":state.page,"page_start_sequence":state.sequence,"turns":turns,"page_turn_count":turns.len(),
        "source_bytes_examined":examined,"scan_progress":!complete && turns.is_empty() && next.offset > state.offset,
        "snapshot_complete":complete,"next_page_token":if complete { Value::Null } else { json!(next_token) },
        "resume_token":if complete { json!(next_token) } else { Value::Null },"source_final":false,"warnings":[]}),
        next,
    )
}

fn fits(value: &Value, p: &Params, request: &RequestEnvelope) -> bool {
    serde_json::to_vec(&success_response(&request.request_id, value.clone()))
        .unwrap()
        .len()
        + 1
        <= p.max_response_bytes
}

pub(crate) fn read_turns(request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    let p = params(request)?;
    let provider = request
        .provider_instance_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid(request, "provider_instance_id is required"))?;
    let account = crate::session::account_home(request, &p.settings_id)?;
    let (mut file, metadata_bytes, metadata_end) =
        crate::session::locate_page_source(&account, &p.session_id, p.max_source_bytes, request)?;
    let current = stamp(&file, request)?;
    let binding = Binding {
        provider: provider.into(),
        account,
        settings: p.settings_id.clone(),
        session: p.session_id.clone(),
        projection: p.turn_projection.clone(),
        nonce: p.expected_delivery_nonce.clone(),
    };
    let budgets = Budgets {
        turns: p.max_turns,
        response: p.max_response_bytes,
        source: p.max_source_bytes,
        inline: p.max_inline_body_bytes,
    };
    let root = state_root(request)?;
    let old = p
        .page_token
        .as_ref()
        .or(p.after_token.as_ref())
        .map(|t| load(&root, t, request))
        .transpose()?;
    if let Some(old) = &old {
        if old.binding != binding || old.offset > current.len {
            return Err(stale(request));
        }
        require_generation(&old.stamp, &current, request)?;
    }
    let state = if p.start_mode == "continuation" {
        let old = old.unwrap();
        if old.kind != "page"
            || old.budgets != budgets
            || Some(&old.snapshot) != p.snapshot_id.as_ref()
        {
            return Err(stale(request));
        }
        old
    } else {
        if old.as_ref().is_some_and(|s| s.kind != "resume") {
            return Err(stale(request));
        }
        let offset = old.as_ref().map_or(metadata_end, |s| s.offset);
        let snapshot = sha256_hex(
            &serde_json::to_vec(
                &json!({"binding":binding,"stamp":current,"offset":offset,"budgets":budgets}),
            )
            .unwrap(),
        );
        Cursor {
            kind: "page".into(),
            binding,
            budgets,
            stamp: current.clone(),
            snapshot,
            offset,
            page: 0,
            sequence: 0,
            partial_record: old.and_then(|s| s.partial_record),
        }
    };
    let mut next = state.clone();
    let examined;
    let mut turns = Vec::new();
    let mut complete = false;
    if p.start_mode == "tail" {
        let start = current
            .len
            .saturating_sub((p.max_source_bytes - metadata_bytes) as u64);
        file.seek(SeekFrom::Start(start))
            .map_err(|_| io_error(request))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(current.len - start)
            .read_to_end(&mut bytes)
            .map_err(|_| io_error(request))?;
        examined = metadata_bytes + bytes.len();
        next.offset = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map(|i| start + i as u64 + 1)
            .unwrap_or(0);
        if next.offset == 0 && current.len > 0 {
            return Err(capacity(
                request,
                "Tail budget cannot locate a complete record boundary",
            ));
        }
        complete = true;
    } else {
        file.seek(SeekFrom::Start(state.offset))
            .map_err(|_| io_error(request))?;
        let maximum =
            (state.stamp.len - state.offset).min((p.max_source_bytes - metadata_bytes) as u64);
        let mut bytes = partial_bytes(&root, &state, request)?;
        let prefix_len = bytes.len();
        let record_start = state.offset - prefix_len as u64;
        // Native reads retain the old budget, including identity metadata.
        // Staging reads are separately bounded by MAX_RECORD_BYTES.
        (&mut file)
            .take(maximum.min((MAX_RECORD_BYTES - prefix_len) as u64))
            .read_to_end(&mut bytes)
            .map_err(|_| io_error(request))?;
        let native_read = bytes.len() - prefix_len;
        examined = metadata_bytes + native_read;
        let mut consumed = 0;
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") {
                break;
            }
            let offset = record_start + consumed as u64;
            let proposed_offset = offset + line.len() as u64;
            let value = if line.iter().all(u8::is_ascii_whitespace) {
                Value::Null
            } else {
                serde_json::from_slice::<Value>(line)
                    .map_err(|_| invalid(request, "Malformed complete Codex rollout record"))?
            };
            if offset == 0
                && (value["type"] != "session_meta" || value["payload"]["id"] != p.session_id)
            {
                return Err(stale(request));
            }
            let projected = project(
                &value,
                &p,
                offset,
                state.sequence + turns.len() as u64,
                request,
            )?;
            if let Some(mut turn) = projected {
                if turns.len() == p.max_turns {
                    break;
                }
                let mut candidate = next.clone();
                candidate.offset = proposed_offset;
                candidate.partial_record = None;
                turns.push(turn.clone());
                let (result, _) = page_result(
                    &state,
                    candidate.clone(),
                    &turns,
                    examined,
                    proposed_offset == state.stamp.len,
                );
                if !fits(&result, &p, request) {
                    if turn["body_state"] == "inline" {
                        turn["body_state"] = json!("omitted_oversize");
                        turn["body"] = Value::Null;
                    }
                    *turns.last_mut().unwrap() = turn;
                    let (result, _) = page_result(
                        &state,
                        candidate,
                        &turns,
                        examined,
                        proposed_offset == state.stamp.len,
                    );
                    if !fits(&result, &p, request) {
                        turns.pop();
                        if turns.is_empty() {
                            return Err(capacity(
                                request,
                                "Response budget cannot hold the next turn metadata",
                            ));
                        }
                        break;
                    }
                }
            }
            consumed += line.len();
            next.offset = proposed_offset;
            next.partial_record = None;
        }
        if next.offset == state.stamp.len {
            complete = true;
        }
        // Only an unframed suffix can be staged. If a turn/response limit
        // stopped us before a newline, leave that entire record for replay.
        if consumed < bytes.len() && !bytes[consumed..].contains(&b'\n') {
            let suffix = &bytes[consumed..];
            next.partial_record = Some(stage_partial(
                &root,
                record_start + consumed as u64,
                suffix,
                request,
            )?);
            next.offset = record_start + bytes.len() as u64;
            // EOF coverage does not project an unfinished record. Resume keeps
            // its immutable prefix, and append supplies the missing suffix.
            complete = next.offset == state.stamp.len;
        }
        if next.offset == state.offset && !complete {
            return Err(capacity(
                request,
                "Source budget cannot hold the next complete JSONL record",
            ));
        }
    }
    require_generation(&current, &stamp(&file, request)?, request)?;
    let (result, cursor) = page_result(&state, next, &turns, examined, complete);
    if !fits(&result, &p, request) {
        return Err(capacity(
            request,
            "Response budget cannot hold paging metadata",
        ));
    }
    persist(&root, &cursor, request)?;
    Ok(result)
}
