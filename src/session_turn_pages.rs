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

fn persist(
    root: &Path,
    admission: &mut StagingAdmission,
    limits: (u64, u64),
    cursor: &Cursor,
    request: &RequestEnvelope,
) -> Result<(), ProviderFailure> {
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
    admission.reserve(root, bytes.len(), limits, request)?;
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|_| io_error(request))?;
    temp.write_all(&bytes).map_err(|_| io_error(request))?;
    temp.as_file().sync_all().map_err(|_| io_error(request))?;
    temp.persist(&path).map_err(|_| io_error(request))?;
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

// The directory inode is the cross-process lock: no lock-file allocation and
// aliases to the same canonical scope share the same lock. Never unlink it.
// A held guard reserves the one pending write numerically before allocation;
// no other paging writer can admit until publication or failure releases it.
struct StagingAdmission {
    _lock: File,
    bytes: u64,
    objects: u64,
}

const STAGING_LIMITS: (u64, u64) = (512 * 1024 * 1024, 4096);

fn storage_limit(request: &RequestEnvelope) -> ProviderFailure {
    ProviderFailure::unsupported(
        &request.request_id,
        "session_turn_staging_capacity_exceeded",
        "Paging staging capacity exhausted; checkpoint retained; operator intervention required",
    )
}

impl StagingAdmission {
    fn acquire(root: &Path, request: &RequestEnvelope) -> Result<Self, ProviderFailure> {
        let lock = File::open(root).map_err(|_| io_error(request))?;
        fs2::FileExt::lock_exclusive(&lock).map_err(|_| io_error(request))?;
        Ok(Self {
            _lock: lock,
            bytes: 0,
            objects: 0,
        })
    }

    fn reserve(
        &mut self,
        root: &Path,
        bytes: usize,
        limits: (u64, u64),
        request: &RequestEnvelope,
    ) -> Result<(), ProviderFailure> {
        // Recover from the filesystem, not a possibly stale ledger. Interrupted
        // temporary writes and published-but-unreferenced prefixes remain
        // charged forever; no replay dependency is collected.
        let (mut retained_bytes, mut retained_objects) = (0u64, 0u64);
        for entry in std::fs::read_dir(root).map_err(|_| io_error(request))? {
            let entry = entry.map_err(|_| io_error(request))?;
            let metadata =
                std::fs::symlink_metadata(entry.path()).map_err(|_| io_error(request))?;
            if !metadata.is_file() {
                return Err(storage_limit(request));
            }
            retained_bytes = retained_bytes.saturating_add(metadata.len());
            retained_objects = retained_objects.saturating_add(1);
            if retained_bytes > limits.0 || retained_objects > limits.1 {
                return Err(storage_limit(request));
            }
        }
        self.bytes = retained_bytes.saturating_add(bytes as u64);
        self.objects = retained_objects.saturating_add(1);
        if self.bytes > limits.0 || self.objects > limits.1 {
            return Err(storage_limit(request));
        }
        Ok(())
    }
}

fn stage_partial(
    root: &Path,
    admission: &mut StagingAdmission,
    limits: (u64, u64),
    start: u64,
    bytes: &[u8],
    request: &RequestEnvelope,
) -> Result<PartialRecord, ProviderFailure> {
    if bytes.len() >= MAX_RECORD_BYTES {
        return Err(record_limit(request));
    }
    let sha256 = sha256_hex(bytes);
    let path = root.join(format!("record-{sha256}.part"));
    // Deduplicate before reserving or allocating, even at/above capacity.
    if path.exists() {
        let existing =
            crate::durable_fs::read_file_bounded(&path, bytes.len()).map_err(|_| stale(request))?;
        if existing != bytes {
            return Err(stale(request));
        }
        return Ok(PartialRecord { start, sha256 });
    }
    admission.reserve(root, bytes.len(), limits, request)?;
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|_| io_error(request))?;
    temp.write_all(bytes).map_err(|_| io_error(request))?;
    temp.as_file().sync_all().map_err(|_| io_error(request))?;
    // Atomic rename (not persist_noclobber's hard-link/unlink pair): one
    // reserved object throughout publication, including interruption. The
    // scope lock excludes competing forward writers; the name was checked.
    temp.persist(&path).map_err(|_| io_error(request))?;
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

// The separately frozen containment commit changes only this switch.
const PAGING_ENABLED: bool = true;

pub(crate) fn read_turns(request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    read_turns_mode(request, PAGING_ENABLED)
}

fn read_turns_mode(request: &RequestEnvelope, enabled: bool) -> Result<Value, ProviderFailure> {
    if !enabled {
        return Err(ProviderFailure::unsupported(
            &request.request_id,
            "session_turn_paging_paused",
            "Session paging is paused by the containment provider; checkpoint retained; operator intervention required",
        ));
    }
    read_turns_with_limits(request, STAGING_LIMITS)
}

fn read_turns_with_limits(
    request: &RequestEnvelope,
    limits: (u64, u64),
) -> Result<Value, ProviderFailure> {
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
    let mut admission = StagingAdmission::acquire(&root, request)?;
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
                &mut admission,
                limits,
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
    persist(&root, &mut admission, limits, &cursor, request)?;
    Ok(result)
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use std::fs;

    fn request(root: &Path) -> RequestEnvelope {
        serde_json::from_value(json!({"contract":"oulipoly.provider/v1","request_id":"admission-test","provider_instance_id":"codex-provider","host":{"app":"test","data_root":root.join("state"),"env":{"HOME":root}},"params":{"settings_id":"codex","session_id":"test-session","read_protocol":PROTOCOL,"turn_projection":"canonical_ingest","start_mode":"beginning","after_token":null,"snapshot_id":null,"page_token":null,"max_turns":8,"max_response_bytes":4096,"max_source_bytes":512,"max_inline_body_bytes":100}})).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, RequestEnvelope, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let dir = root.path().join(".codex/sessions/2026/09/04");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-test-test-session.jsonl");
        fs::write(&path, format!("{}\n{}\n{}\n", json!({"type":"session_meta","payload":{"id":"test-session","cwd":"/workspace"}}), json!({"type":"compacted","payload":{"text":"x".repeat(900)}}), json!({"timestamp":"2026-09-04T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"after"}]}}))).unwrap();
        (root, req, path)
    }

    fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        if root.exists() {
            for entry in fs::read_dir(root).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    out.extend(files(&p));
                } else {
                    out.push((p.clone(), fs::read(p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn numeric_reservation_pending_orphan_recovery_dedup_and_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let mut guard = StagingAdmission::acquire(root.path(), &req).unwrap();
        guard.reserve(root.path(), 6, (10, 2), &req).unwrap();
        assert_eq!((guard.bytes, guard.objects), (6, 1));
        // Simulated interruption after partial temp write: reservation dies
        // with writer, but the actual retained orphan is charged on restart.
        fs::write(root.path().join("interrupted-temp"), b"1234").unwrap();
        drop(guard);
        let mut guard = StagingAdmission::acquire(root.path(), &req).unwrap();
        let first = stage_partial(root.path(), &mut guard, (10, 2), 0, b"abcdef", &req).unwrap();
        assert_eq!((guard.bytes, guard.objects), (10, 2));
        let before = files(root.path());
        stage_partial(root.path(), &mut guard, (10, 2), 0, b"abcdef", &req).unwrap();
        assert_eq!(files(root.path()), before); // no duplicate temp even at limit
        assert_eq!(
            stage_partial(root.path(), &mut guard, (10, 2), 0, b"z", &req)
                .unwrap_err()
                .code,
            "session_turn_staging_capacity_exceeded"
        );
        // Above-limit existing contents are retained, dedup still works.
        assert_eq!(
            stage_partial(root.path(), &mut guard, (1, 1), 0, b"abcdef", &req)
                .unwrap()
                .sha256,
            first.sha256
        );
        assert!(stage_partial(root.path(), &mut guard, (9, 9), 0, b"y", &req).is_err());
        assert!(stage_partial(root.path(), &mut guard, (100, 2), 0, b"y", &req).is_err());
        assert_eq!(files(root.path()), before);
    }

    #[test]
    fn concurrent_independent_requests_cannot_double_admit() {
        let root = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let root = root.path().to_owned();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let req = request(&root);
                    barrier.wait();
                    let mut guard = StagingAdmission::acquire(&root, &req).unwrap();
                    stage_partial(&root, &mut guard, (12, 2), 0, &[b'a' + i; 6], &req).map(|_| ())
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 2);
        assert!(results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| e.code == "session_turn_staging_capacity_exceeded"));
        let retained = files(root.path());
        assert_eq!(retained.len(), 2);
        assert_eq!(retained.iter().map(|(_, b)| b.len()).sum::<usize>(), 12);
    }

    #[test]
    #[ignore = "fixture subprocess only; parent supplies an isolated temporary scope"]
    fn abrupt_writer_fixture() {
        let root = PathBuf::from(std::env::var("AGE343_ORPHAN_FIXTURE_ROOT").unwrap());
        assert_eq!(
            std::env::var("AGE343_ORPHAN_FIXTURE_MODE").unwrap(),
            "partial-temp"
        );
        let req = request(&root);
        let mut guard = StagingAdmission::acquire(&root, &req).unwrap();
        guard.reserve(&root, 6, (10, 2), &req).unwrap();
        let mut temp = tempfile::NamedTempFile::new_in(&root).unwrap();
        temp.write_all(b"abc").unwrap();
        temp.as_file().sync_all().unwrap();
        // No signal, native session, or destructor cleanup: fixture process
        // exit leaves a real partial temporary and releases its kernel lock.
        std::process::exit(0);
    }

    #[test]
    fn subprocess_exit_recovers_actual_partial_temp_and_releases_reservation() {
        let root = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session_turn_pages::admission_tests::abrupt_writer_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("AGE343_ORPHAN_FIXTURE_ROOT", root.path())
            .env("AGE343_ORPHAN_FIXTURE_MODE", "partial-temp")
            .status()
            .unwrap();
        assert!(status.success());
        let orphan = files(root.path());
        assert_eq!(orphan.len(), 1);
        assert_eq!(orphan[0].1, b"abc");
        let req = request(root.path());
        let mut guard = StagingAdmission::acquire(root.path(), &req).unwrap();
        stage_partial(root.path(), &mut guard, (10, 2), 0, b"123456", &req).unwrap();
        assert_eq!((guard.bytes, guard.objects), (9, 2));
        assert!(stage_partial(root.path(), &mut guard, (10, 2), 0, b"x", &req).is_err());
        assert!(files(root.path()).iter().any(|entry| entry == &orphan[0]));
    }

    #[test]
    fn exhausted_request_preserves_checkpoint_and_replay_not_completion() {
        let (_tmp, req, path) = fixture();
        let original = fs::read(&path).unwrap();
        let first = read_turns_with_limits(&req, (2000, 2)).unwrap();
        assert_eq!(first["snapshot_complete"], false);
        assert_eq!(first["scan_progress"], true);
        let root = state_root(&req).unwrap();
        let before = files(&root);
        assert_eq!(read_turns_with_limits(&req, (2000, 2)).unwrap(), first);
        let mut next = req.clone();
        next.params["start_mode"] = json!("continuation");
        next.params["snapshot_id"] = first["snapshot_id"].clone();
        next.params["page_token"] = first["next_page_token"].clone();
        assert_eq!(
            read_turns_with_limits(&next, (2000, 2)).unwrap_err().code,
            "session_turn_staging_capacity_exceeded"
        );
        assert_eq!(files(&root), before);
        assert_eq!(fs::read(path).unwrap(), original);
        assert_eq!(read_turns_with_limits(&req, (0, 0)).unwrap(), first);
    }

    #[test]
    fn failed_cursor_publication_leaves_only_admitted_recoverable_prefix() {
        let (_tmp, req, _) = fixture();
        assert_eq!(
            read_turns_with_limits(&req, (2000, 1)).unwrap_err().code,
            "session_turn_staging_capacity_exceeded"
        );
        let root = state_root(&req).unwrap();
        let orphan = files(&root);
        assert_eq!(orphan.len(), 1);
        assert!(orphan[0].0.extension().is_some_and(|s| s == "part"));
        // Simulate restart after prefix publication, before cursor publication.
        // Reuse that prefix; reserve only the newly needed cursor.
        let first = read_turns_with_limits(&req, (2000, 2)).unwrap();
        assert_eq!(first["snapshot_complete"], false);
        let state = files(&root);
        assert_eq!(state.len(), 2);
        assert!(state.iter().any(|entry| entry == &orphan[0]));
        assert_eq!(read_turns_with_limits(&req, (0, 0)).unwrap(), first);
        assert_eq!(files(&root), state);
    }

    #[test]
    fn concurrent_page_requests_deduplicate_cursor_and_prefix_at_exact_object_limit() {
        let (_tmp, req, _) = fixture();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let req = req.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    read_turns_with_limits(&req, (2000, 2)).unwrap()
                })
            })
            .collect();
        let pages: Vec<_> = threads.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(pages.iter().all(|page| page == &pages[0]));
        assert_eq!(files(&state_root(&req).unwrap()).len(), 2);
    }

    #[test]
    fn forward_containment_forward_chain_preserves_bytes_and_fences() {
        let (tmp, req, path) = fixture();
        let first = read_turns_with_limits(&req, STAGING_LIMITS).unwrap();
        let mut next = req.clone();
        next.params["start_mode"] = json!("continuation");
        next.params["snapshot_id"] = first["snapshot_id"].clone();
        next.params["page_token"] = first["next_page_token"].clone();
        let root = state_root(&req).unwrap();
        let cursor = load(&root, first["next_page_token"].as_str().unwrap(), &req).unwrap();
        assert!(cursor.partial_record.is_some());
        let before = files(tmp.path());
        assert_eq!(
            read_turns_mode(&next, false).unwrap_err().code,
            "session_turn_paging_paused"
        );
        if !PAGING_ENABLED {
            assert_eq!(
                read_turns(&next).unwrap_err().code,
                "session_turn_paging_paused"
            );
        }
        let mut wrong = next.clone();
        wrong.provider_instance_id = Some("other-provider".into());
        assert_eq!(
            read_turns_mode(&wrong, false).unwrap_err().code,
            "session_turn_paging_paused"
        );
        assert_eq!(files(tmp.path()), before);
        assert_eq!(
            read_turns_with_limits(&wrong, STAGING_LIMITS)
                .unwrap_err()
                .code,
            "session_turn_page_token_stale"
        );
        let mut seen = Vec::new();
        let mut complete = false;
        for _ in 0..8 {
            let page = read_turns_with_limits(&next, STAGING_LIMITS).unwrap();
            assert_eq!(page, read_turns_with_limits(&next, STAGING_LIMITS).unwrap());
            seen.extend(page["turns"].as_array().unwrap().iter().cloned());
            if page["snapshot_complete"] == true {
                complete = true;
                break;
            }
            next.params["snapshot_id"] = page["snapshot_id"].clone();
            next.params["page_token"] = page["next_page_token"].clone();
        }
        assert!(complete);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["role"], "user");
        assert_eq!(
            fs::read(path).unwrap(),
            before
                .iter()
                .find(|(p, _)| p.extension().is_some_and(|s| s == "jsonl"))
                .unwrap()
                .1
        );
        // No account or state directory is created by containment on new input.
        let fresh = tempfile::tempdir().unwrap();
        assert!(read_turns_mode(&request(fresh.path()), false).is_err());
        assert!(files(fresh.path()).is_empty());
    }
}
