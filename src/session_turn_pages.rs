//! Bounded, append-only Codex JSONL paging. Cursor contents remain provider-owned.
use crate::encoding::sha256_hex;
use crate::envelope::{success_response, ProviderFailure, RequestEnvelope};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

mod observation;

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
    let (device, inode) = crate::session::file_identity(&m);
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
    let legacy = root.join(format!("{suffix}.json"));
    let bytes = if legacy.exists() {
        crate::durable_fs::read_file_bounded(&legacy, 32768).map_err(|_| stale(request))?
    } else {
        scan_pack(&pack_path(root, suffix), suffix)
            .map_err(|_| stale(request))?
            .0
            .ok_or_else(|| stale(request))?
    };
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
    if bytes.len() > 32768 {
        return Err(io_error(request));
    }
    let path = root.join(format!("{}.json", sha256_hex(&bytes)));
    if path.exists() {
        if crate::durable_fs::read_file_bounded(&path, 32768).map_err(|_| io_error(request))?
            == bytes
        {
            return Ok(());
        }
        return Err(stale(request));
    }
    let digest = sha256_hex(&bytes);
    let path = pack_path(root, &digest);
    let (existing, valid_end, physical_len) =
        scan_pack(&path, &digest).map_err(|_| io_error(request))?;
    if let Some(existing) = existing {
        if existing != bytes {
            return Err(stale(request));
        }
        // A previous process may have died after completing the frame but
        // before syncing it. Dedup must complete publication before returning.
        return sync_pack(&path, root).map_err(|_| io_error(request));
    }
    let frame = format!("{digest} {}\n", String::from_utf8(bytes).unwrap());
    let old_charge = if path.exists() {
        charged_bytes(physical_len)
    } else {
        0
    };
    let new_charge = charged_bytes(valid_end + frame.len() as u64);
    admission.reserve_growth(
        root,
        new_charge.saturating_sub(old_charge),
        u64::from(!path.exists()),
        limits,
        request,
    )?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|_| io_error(request))?;
    // Only an incomplete final frame is reclaimable: no issued token could
    // reference it. Complete frames, even unreferenced ones, never expire.
    if physical_len != valid_end {
        file.set_len(valid_end).map_err(|_| io_error(request))?;
        file.sync_all().map_err(|_| io_error(request))?;
    }
    file.seek(SeekFrom::Start(valid_end))
        .map_err(|_| io_error(request))?;
    file.write_all(frame.as_bytes())
        .map_err(|_| io_error(request))?;
    sync_pack(&path, root).map_err(|_| io_error(request))
}

// Fixed hash buckets bound new cursor inode growth to 256, without a mutable
// index or migration of legacy tokens. All access is under the directory lock.
fn pack_path(root: &Path, digest: &str) -> PathBuf {
    root.join(format!("cursors-{}.pack", &digest[..2]))
}

fn sync_pack(path: &Path, root: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()?;
    File::open(root)?.sync_all()
}

// Returns matching content, end of complete verified frames, physical length.
// An interrupted append has no newline; bounded streaming never loads a pack.
fn scan_pack(path: &Path, digest: &str) -> std::io::Result<(Option<Vec<u8>>, u64, u64)> {
    use std::io::{Error, ErrorKind};
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok((None, 0, 0)),
        Err(e) => return Err(e),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > STAGING_LIMITS.0 {
        return Err(Error::other("invalid cursor pack"));
    }
    let mut reader = BufReader::new(file);
    let mut end = 0;
    let mut found = None;
    loop {
        let mut frame = Vec::new();
        (&mut reader).take(32835).read_until(b'\n', &mut frame)?;
        if frame.is_empty() {
            break;
        }
        if frame.len() > 32834 {
            return Err(Error::other("oversized cursor frame"));
        }
        if frame.last() != Some(&b'\n') {
            break;
        }
        if frame.len() < 67 || frame[64] != b' ' {
            return Err(Error::other("invalid cursor frame"));
        }
        let bytes = &frame[65..frame.len() - 1];
        let hash = sha256_hex(bytes);
        if hash.as_bytes() != &frame[..64] {
            return Err(Error::other("corrupt cursor frame"));
        }
        if hash == digest {
            found = Some(bytes.to_vec());
        }
        end += frame.len() as u64;
    }
    Ok((found, end, metadata.len()))
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

const STAGING_LIMITS: (u64, u64) = (512 * 1024 * 1024, 512 * 1024 * 1024 / 4096);

// Conservative allocation quantum plus per-inode allowance. Logical content
// and metadata both consume budget; tiny legacy files cannot evade accounting.
fn charged_bytes(len: u64) -> u64 {
    (len.saturating_add(4095) / 4096 * 4096).saturating_add(4096)
}

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
        self.reserve_growth(root, charged_bytes(bytes as u64), 1, limits, request)
    }

    fn reserve_growth(
        &mut self,
        root: &Path,
        bytes: u64,
        objects: u64,
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
            retained_bytes = retained_bytes.saturating_add(charged_bytes(metadata.len()));
            retained_objects = retained_objects.saturating_add(1);
            if retained_bytes > limits.0 || retained_objects > limits.1 {
                return Err(storage_limit(request));
            }
        }
        self.bytes = retained_bytes.saturating_add(bytes);
        self.objects = retained_objects.saturating_add(objects);
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

// Storage strategy is selected only by the authenticated request projection.
// Canonical admission and immutable cursor/prefix serialization stay unchanged.
enum PageStorage {
    Canonical {
        root: PathBuf,
        admission: StagingAdmission,
        limits: (u64, u64),
    },
    Observation {
        root: PathBuf,
        key: [u8; 32],
    },
}

impl PageStorage {
    fn new(
        root: PathBuf,
        p: &Params,
        limits: (u64, u64),
        request: &RequestEnvelope,
    ) -> Result<Self, ProviderFailure> {
        if p.turn_projection == "user_observation" {
            let key = observation::key(&root, request)?;
            Ok(Self::Observation { root, key })
        } else {
            let admission = StagingAdmission::acquire(&root, request)?;
            Ok(Self::Canonical {
                root,
                admission,
                limits,
            })
        }
    }

    fn load(
        &self,
        value: &str,
        binding: &Binding,
        request: &RequestEnvelope,
    ) -> Result<Cursor, ProviderFailure> {
        match self {
            Self::Observation { key, .. } if observation::is_token(value) => {
                observation::load(key, value, binding, request)
            }
            Self::Observation { root, .. } => {
                // Legacy observation tokens may now reside in mutable packs.
                // Serialize with cooperating appends/tail recovery, but never
                // reserve canonical capacity for observation or retain the lock
                // across source reconstruction.
                let _guard = StagingAdmission::acquire(root, request)?;
                load(root, value, request)
            }
            Self::Canonical { root, .. } => load(root, value, request),
        }
    }

    fn token(&self, cursor: &Cursor) -> String {
        match self {
            Self::Observation { key, .. } => observation::token(key, cursor),
            Self::Canonical { .. } => token(cursor),
        }
    }

    fn prefix(
        &self,
        file: &mut File,
        state: &Cursor,
        request: &RequestEnvelope,
    ) -> Result<Vec<u8>, ProviderFailure> {
        match self {
            Self::Observation { .. } => observation::reconstruct(file, state, request),
            Self::Canonical { root, .. } => partial_bytes(root, state, request),
        }
    }

    fn stage(
        &mut self,
        start: u64,
        bytes: &[u8],
        request: &RequestEnvelope,
    ) -> Result<PartialRecord, ProviderFailure> {
        match self {
            Self::Observation { .. } => {
                if bytes.len() >= MAX_RECORD_BYTES {
                    return Err(record_limit(request));
                }
                Ok(PartialRecord {
                    start,
                    sha256: sha256_hex(bytes),
                })
            }
            Self::Canonical {
                root,
                admission,
                limits,
            } => stage_partial(root, admission, *limits, start, bytes, request),
        }
    }

    fn persist(
        &mut self,
        cursor: &Cursor,
        request: &RequestEnvelope,
    ) -> Result<(), ProviderFailure> {
        match self {
            Self::Observation { .. } => Ok(()),
            Self::Canonical {
                root,
                admission,
                limits,
            } => persist(root, admission, *limits, cursor, request),
        }
    }
}

#[derive(Default)]
struct ReadAccounting {
    metadata: usize,
    forward: usize,
    reconstruction: usize,
}

impl ReadAccounting {
    fn total(&self) -> usize {
        self.metadata + self.forward + self.reconstruction
    }

    fn warnings(&self, storage: &PageStorage) -> Vec<String> {
        match storage {
            PageStorage::Canonical { .. } => Vec::new(),
            PageStorage::Observation { .. } => vec![format!(
                "codex_observation_io_v1:forward={};reconstruction={};metadata={}",
                self.forward, self.reconstruction, self.metadata
            )],
        }
    }
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
    examined: &ReadAccounting,
    complete: bool,
    storage: &PageStorage,
) -> (Value, Cursor) {
    next.kind = if complete { "resume" } else { "page" }.into();
    next.page = state.page + 1;
    next.sequence = state.sequence + turns.len() as u64;
    let next_token = storage.token(&next);
    (
        json!({"read_protocol":PROTOCOL,"provider_instance_id":state.binding.provider,"settings_id":state.binding.settings,
        "session_id":state.binding.session,"turn_projection":state.binding.projection,"snapshot_id":state.snapshot,
        "page_index":state.page,"page_start_sequence":state.sequence,"turns":turns,"page_turn_count":turns.len(),
        "source_bytes_examined":examined.total(),"scan_progress":!complete && turns.is_empty() && next.offset > state.offset,
        "snapshot_complete":complete,"next_page_token":if complete { Value::Null } else { json!(next_token) },
        "resume_token":if complete { json!(next_token) } else { Value::Null },"source_final":false,"warnings":examined.warnings(storage)}),
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
    let mut storage = PageStorage::new(root, &p, limits, request)?;
    let old = p
        .page_token
        .as_ref()
        .or(p.after_token.as_ref())
        .map(|t| storage.load(t, &binding, request))
        .transpose()?;
    if old.as_ref().is_some_and(|old| old.binding != binding) {
        return Err(stale(request));
    }
    let (mut file, metadata_bytes, metadata_end) = crate::session::locate_page_source(
        &binding.account,
        &p.session_id,
        p.max_source_bytes,
        old.as_ref().map(|old| (old.stamp.device, old.stamp.inode)),
        request,
    )
    .map_err(|error| {
        // A bound physical source disappearing/replacing is a stale cursor,
        // not a fresh session lookup miss. Preserve the existing cursor error.
        if old.is_some() && error.code == "codex_session_not_found" {
            stale(request)
        } else {
            error
        }
    })?;
    let current = stamp(&file, request)?;
    if let Some(old) = &old {
        if old.offset > current.len {
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
    let mut examined = ReadAccounting {
        metadata: metadata_bytes,
        ..ReadAccounting::default()
    };
    let mut turns = Vec::new();
    let mut complete = false;
    let mut framed_checkpoint = None;
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
        examined.forward = bytes.len();
        if bytes.len() as u64 != current.len - start {
            return Err(stale(request));
        }
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
        let maximum =
            (state.stamp.len - state.offset).min((p.max_source_bytes - metadata_bytes) as u64);
        let mut bytes = storage.prefix(&mut file, &state, request)?;
        file.seek(SeekFrom::Start(state.offset))
            .map_err(|_| io_error(request))?;
        let prefix_len = bytes.len();
        let record_start = state.offset - prefix_len as u64;
        // The forward quantum still includes metadata. Only observation may
        // reconstruct a prefix from native source under a separate record bound.
        if matches!(storage, PageStorage::Observation { .. }) {
            examined.reconstruction = prefix_len;
        }
        (&mut file)
            .take(maximum.min((MAX_RECORD_BYTES - prefix_len) as u64))
            .read_to_end(&mut bytes)
            .map_err(|_| io_error(request))?;
        let native_read = bytes.len() - prefix_len;
        examined.forward = native_read;
        if matches!(storage, PageStorage::Observation { .. })
            && native_read as u64 != maximum.min((MAX_RECORD_BYTES - prefix_len) as u64)
        {
            return Err(stale(request));
        }
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
                    &examined,
                    proposed_offset == state.stamp.len,
                    &storage,
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
                        &examined,
                        proposed_offset == state.stamp.len,
                        &storage,
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
            if consumed > 0 {
                framed_checkpoint = Some(next.clone());
            }
            let suffix = &bytes[consumed..];
            next.partial_record =
                Some(storage.stage(record_start + consumed as u64, suffix, request)?);
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
    let (mut result, mut cursor) =
        page_result(&state, next.clone(), &turns, &examined, complete, &storage);
    // The observation token grows when a later unframed suffix is retained.
    // Fit the final cursor, not just each provisional complete-record candidate.
    // Keep every turn and its digests; omit inline bodies only as needed.
    for index in (0..turns.len()).rev() {
        if fits(&result, &p, request) {
            break;
        }
        if turns[index]["body_state"] == "inline" {
            turns[index]["body_state"] = json!("omitted_oversize");
            turns[index]["body"] = Value::Null;
            (result, cursor) =
                page_result(&state, next.clone(), &turns, &examined, complete, &storage);
        }
    }
    if !fits(&result, &p, request) {
        if let Some(checkpoint) = framed_checkpoint {
            // A metadata-only page may fit at the preceding complete boundary
            // but not with the larger prefix token. Publish that real progress;
            // leave the unfinished suffix for replay, charging all reads now.
            (result, cursor) = page_result(&state, checkpoint, &turns, &examined, false, &storage);
        }
    }
    if !fits(&result, &p, request) {
        return Err(capacity(
            request,
            "Response budget cannot hold paging metadata",
        ));
    }
    storage.persist(&cursor, request)?;
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
        guard.reserve(root.path(), 6, (16384, 2), &req).unwrap();
        assert_eq!((guard.bytes, guard.objects), (8192, 1));
        // Simulated interruption after partial temp write: reservation dies
        // with writer, but the actual retained orphan is charged on restart.
        fs::write(root.path().join("interrupted-temp"), b"1234").unwrap();
        drop(guard);
        let mut guard = StagingAdmission::acquire(root.path(), &req).unwrap();
        let first = stage_partial(root.path(), &mut guard, (16384, 2), 0, b"abcdef", &req).unwrap();
        assert_eq!((guard.bytes, guard.objects), (16384, 2));
        let before = files(root.path());
        stage_partial(root.path(), &mut guard, (16384, 2), 0, b"abcdef", &req).unwrap();
        assert_eq!(files(root.path()), before); // no duplicate temp even at limit
        assert_eq!(
            stage_partial(root.path(), &mut guard, (16384, 2), 0, b"z", &req)
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
        assert!(stage_partial(root.path(), &mut guard, (16383, 9), 0, b"y", &req).is_err());
        assert!(stage_partial(root.path(), &mut guard, (100000, 2), 0, b"y", &req).is_err());
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
                    stage_partial(&root, &mut guard, (16384, 2), 0, &[b'a' + i; 6], &req)
                        .map(|_| ())
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
        guard.reserve(&root, 6, (16384, 2), &req).unwrap();
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
        stage_partial(root.path(), &mut guard, (16384, 2), 0, b"123456", &req).unwrap();
        assert_eq!((guard.bytes, guard.objects), (16384, 2));
        assert!(stage_partial(root.path(), &mut guard, (16384, 2), 0, b"x", &req).is_err());
        assert!(files(root.path()).iter().any(|entry| entry == &orphan[0]));
    }

    #[test]
    fn exhausted_request_preserves_checkpoint_and_replay_not_completion() {
        let (_tmp, req, path) = fixture();
        let original = fs::read(&path).unwrap();
        let first = read_turns_with_limits(&req, (16384, 2)).unwrap();
        assert_eq!(first["snapshot_complete"], false);
        assert_eq!(first["scan_progress"], true);
        let root = state_root(&req).unwrap();
        let before = files(&root);
        assert_eq!(read_turns_with_limits(&req, (16384, 2)).unwrap(), first);
        let mut next = req.clone();
        next.params["start_mode"] = json!("continuation");
        next.params["snapshot_id"] = first["snapshot_id"].clone();
        next.params["page_token"] = first["next_page_token"].clone();
        assert_eq!(
            read_turns_with_limits(&next, (16384, 2)).unwrap_err().code,
            "session_turn_staging_capacity_exceeded"
        );
        assert_eq!(files(&root), before);
        assert_eq!(fs::read(path).unwrap(), original);
        assert_eq!(read_turns_with_limits(&req, (0, 0)).unwrap(), first);
    }

    #[test]
    fn observation_bypasses_exhausted_object_and_byte_admission_without_mutating_it() {
        let (_tmp, req, path) = fixture();
        let canonical = read_turns_with_limits(&req, (16384, 2)).unwrap();
        let root = state_root(&req).unwrap();
        let before = files(&root);
        let native = fs::read(&path).unwrap();
        let mut observation = req.clone();
        observation.params["turn_projection"] = json!("user_observation");
        observation.params["expected_delivery_nonce"] = json!("a".repeat(64));
        let page = read_turns_with_limits(&observation, (0, 0)).unwrap();
        assert_eq!(page["scan_progress"], true);
        assert_eq!(page, read_turns_with_limits(&observation, (0, 0)).unwrap());
        observation.params["start_mode"] = json!("tail");
        assert_eq!(
            read_turns_with_limits(&observation, (0, 0)).unwrap()["turns"],
            json!([])
        );
        assert_eq!(files(&root), before);
        assert_eq!(read_turns_with_limits(&req, (0, 0)).unwrap(), canonical);
        assert_eq!(fs::read(path).unwrap(), native);
    }

    #[test]
    fn failed_cursor_publication_leaves_only_admitted_recoverable_prefix() {
        let (_tmp, req, _) = fixture();
        assert_eq!(
            read_turns_with_limits(&req, (16384, 1)).unwrap_err().code,
            "session_turn_staging_capacity_exceeded"
        );
        let root = state_root(&req).unwrap();
        let orphan = files(&root);
        assert_eq!(orphan.len(), 1);
        assert!(orphan[0].0.extension().is_some_and(|s| s == "part"));
        // Simulate restart after prefix publication, before cursor publication.
        // Reuse that prefix; reserve only the newly needed cursor.
        let first = read_turns_with_limits(&req, (16384, 2)).unwrap();
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
                    read_turns_with_limits(&req, (16384, 2)).unwrap()
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
    fn seed_cursor() -> Cursor {
        Cursor {
            kind: "resume".into(),
            binding: Binding {
                provider: "p".into(),
                account: "/a".into(),
                settings: "s".into(),
                session: "s".into(),
                projection: "canonical_ingest".into(),
                nonce: None,
            },
            budgets: Budgets {
                turns: 8,
                response: 4096,
                source: 512,
                inline: 100,
            },
            stamp: Stamp {
                device: 1,
                inode: 2,
                len: 1000,
                modified: 1,
            },
            snapshot: "s".repeat(64),
            offset: 1000,
            page: 0,
            sequence: 0,
            partial_record: None,
        }
    }

    #[test]
    fn inherited_4464_small_legacy_cursors_restore_tail_and_postanchor() {
        let (_tmp, mut req, native) = fixture();
        req.params["turn_projection"] = json!("user_observation");
        req.params["expected_delivery_nonce"] = json!("a".repeat(64));
        req.params["start_mode"] = json!("tail");
        let anchor = read_turns_with_limits(&req, STAGING_LIMITS).unwrap();
        let root = state_root(&req).unwrap();
        let storage =
            PageStorage::new(root.clone(), &params(&req).unwrap(), STAGING_LIMITS, &req).unwrap();
        let binding = Binding {
            provider: "codex-provider".into(),
            account: crate::session::account_home(&req, "codex").unwrap(),
            settings: "codex".into(),
            session: "test-session".into(),
            projection: "user_observation".into(),
            nonce: Some("a".repeat(64)),
        };
        let state = storage
            .load(anchor["resume_token"].as_str().unwrap(), &binding, &req)
            .unwrap();
        let old_token = token(&state);
        // Synthetic pre-upgrade layout, not a migration or production copy.
        fs::write(
            root.join(format!("{}.json", &old_token[PREFIX.len()..])),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        let mut tokens = vec![old_token.to_owned()];
        for i in 0..4463 {
            let mut cursor = seed_cursor();
            cursor.sequence = i;
            cursor.snapshot.clear();
            let len = serde_json::to_vec(&cursor).unwrap().len();
            cursor.snapshot = "s".repeat(503 - len);
            let bytes = serde_json::to_vec(&cursor).unwrap();
            let digest = sha256_hex(&bytes);
            fs::write(root.join(format!("{digest}.json")), bytes).unwrap();
            tokens.push(format!("{PREFIX}{digest}"));
        }
        let inherited = files(&root);
        assert_eq!(inherited.len(), 4464);
        let logical: usize = inherited.iter().map(|(_, bytes)| bytes.len()).sum();
        assert!((2_240_000..2_250_000).contains(&logical), "{logical}");
        // Both durable and memory-only issued tokens survive, not just a mark set.
        for token in tokens {
            assert_eq!(token, super::token(&load(&root, &token, &req).unwrap()));
        }
        assert_eq!(read_turns_with_limits(&req, (0, 0)).unwrap(), anchor);
        req.params["expected_delivery_nonce"] = json!("b".repeat(64));
        let fresh = read_turns_with_limits(&req, STAGING_LIMITS).unwrap();
        assert_ne!(fresh["resume_token"], anchor["resume_token"]);
        let mut source = fs::OpenOptions::new().append(true).open(&native).unwrap();
        writeln!(
            source,
            "{}",
            json!({"type":"compacted","payload":{"text":"x".repeat(900)}})
        )
        .unwrap();
        writeln!(source, "{}", json!({"timestamp":"2026-09-04T12:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"new delivery"}]}})).unwrap();
        req.params["start_mode"] = json!("beginning");
        req.params["after_token"] = fresh["resume_token"].clone();
        let mut seen = Vec::new();
        let mut completed = false;
        for _ in 0..8 {
            let page = read_turns_with_limits(&req, STAGING_LIMITS).unwrap();
            assert_eq!(page, read_turns_with_limits(&req, (0, 0)).unwrap());
            seen.extend(page["turns"].as_array().unwrap().iter().cloned());
            if page["snapshot_complete"] == true {
                completed = true;
                break;
            }
            let mut binding = binding.clone();
            binding.nonce = Some("b".repeat(64));
            let cursor = storage
                .load(page["next_page_token"].as_str().unwrap(), &binding, &req)
                .unwrap();
            let mut file = File::open(&native).unwrap();
            assert!(!storage.prefix(&mut file, &cursor, &req).unwrap().is_empty());
            req.params["start_mode"] = json!("continuation");
            req.params["after_token"] = Value::Null;
            req.params["snapshot_id"] = page["snapshot_id"].clone();
            req.params["page_token"] = page["next_page_token"].clone();
        }
        assert!(completed);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["body"][0]["text"], "new delivery");
        let retained = files(&root);
        assert!(inherited.iter().all(|entry| retained.contains(entry)));
        // Original old-schema token still resumes against the appended source.
        req.params["start_mode"] = json!("beginning");
        req.params["expected_delivery_nonce"] = json!("a".repeat(64));
        req.params["after_token"] = json!(old_token);
        req.params["page_token"] = Value::Null;
        req.params["snapshot_id"] = Value::Null;
        assert!(read_turns_with_limits(&req, STAGING_LIMITS).is_ok());
    }

    #[test]
    fn canonical_churn_bounds_cursor_objects_and_charges_retained_history() {
        let (_tmp, req, native) = fixture();
        let root = state_root(&req).unwrap();
        let mut tokens = Vec::new();
        // Exercise the canonical publisher with distinct bound cursor states.
        let page = read_turns_with_limits(&req, STAGING_LIMITS).unwrap();
        let mut cursor = load(&root, page["next_page_token"].as_str().unwrap(), &req).unwrap();
        let mut guard = StagingAdmission::acquire(&root, &req).unwrap();
        for i in 0..1024 {
            cursor.sequence = i;
            persist(&root, &mut guard, STAGING_LIMITS, &cursor, &req).unwrap();
            tokens.push(token(&cursor));
        }
        drop(guard);
        assert!(native.exists());
        let root = state_root(&req).unwrap();
        let retained = files(&root);
        assert!(retained.len() <= 257);
        assert_eq!(
            retained
                .iter()
                .filter(|(p, _)| p.extension().unwrap() == "part")
                .count(),
            1
        );
        assert_eq!(
            retained
                .iter()
                .filter(|(path, _)| path.extension().unwrap() == "pack")
                .map(|(_, bytes)| bytes.iter().filter(|b| **b == b'\n').count())
                .sum::<usize>(),
            1024
        );
        let charged: u64 = retained
            .iter()
            .map(|(_, bytes)| charged_bytes(bytes.len() as u64))
            .sum();
        assert!(charged < 4 * 1024 * 1024);
        for token in tokens {
            assert_eq!(token, super::token(&load(&root, &token, &req).unwrap()));
        }
        let page = read_turns_with_limits(&req, (0, 0)).unwrap();
        assert!(page["next_page_token"].is_string());
        assert_eq!(files(&root), retained);
        let mut guard = StagingAdmission::acquire(&root, &req).unwrap();
        assert!(guard
            .reserve_growth(&root, 1, 0, (charged, STAGING_LIMITS.1), &req)
            .is_err());
        guard
            .reserve_growth(&root, 0, 0, (charged, STAGING_LIMITS.1), &req)
            .unwrap();
        assert_eq!(guard.bytes, charged);
    }

    #[test]
    fn observation_churn_has_one_key_no_retained_pool_and_replays_old_nonces() {
        let (_tmp, mut req, _) = fixture();
        req.params["turn_projection"] = json!("user_observation");
        req.params["start_mode"] = json!("tail");
        let mut pages = Vec::new();
        for i in 0..1024 {
            req.params["expected_delivery_nonce"] = json!(format!("{i:064x}"));
            pages.push(read_turns_with_limits(&req, (0, 0)).unwrap());
        }
        let root = state_root(&req).unwrap();
        assert!(files(&root).is_empty());
        let auth = root.parent().unwrap().join("observation-auth-v1");
        let authority = files(&auth);
        assert_eq!(authority.len(), 1);
        assert_eq!(authority[0].1.len(), 32);
        for (i, page) in pages.iter().enumerate() {
            req.params["expected_delivery_nonce"] = json!(format!("{i:064x}"));
            assert_eq!(&read_turns_with_limits(&req, (0, 0)).unwrap(), page);
            req.params["start_mode"] = json!("beginning");
            req.params["after_token"] = page["resume_token"].clone();
            assert_eq!(
                read_turns_with_limits(&req, (0, 0)).unwrap()["snapshot_complete"],
                true
            );
            req.params["start_mode"] = json!("tail");
            req.params["after_token"] = Value::Null;
        }
        assert_eq!(files(&auth), authority);
        assert!(files(&root).is_empty());
    }

    #[test]
    fn packed_observation_load_waits_for_canonical_publication_lock() {
        let (_tmp, mut req, _) = fixture();
        req.params["turn_projection"] = json!("user_observation");
        req.params["expected_delivery_nonce"] = json!("a".repeat(64));
        let root = state_root(&req).unwrap();
        let mut cursor = seed_cursor();
        cursor.binding.projection = "user_observation".into();
        cursor.binding.nonce = Some("a".repeat(64));
        let mut guard = StagingAdmission::acquire(&root, &req).unwrap();
        persist(&root, &mut guard, STAGING_LIMITS, &cursor, &req).unwrap();
        let value = token(&cursor);
        let binding = cursor.binding.clone();
        let storage = PageStorage::new(root, &params(&req).unwrap(), (0, 0), &req).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let child = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let loaded = storage.load(&value, &binding, &req).unwrap();
            done_tx.send(token(&loaded)).unwrap();
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let before_release = done_rx.recv_timeout(std::time::Duration::from_millis(100));
        drop(guard);
        let after_release = done_rx.recv_timeout(std::time::Duration::from_secs(5));
        child.join().unwrap();
        assert!(matches!(
            before_release,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(after_release.unwrap(), token(&cursor));
    }

    fn same_bucket_pair() -> (Cursor, Cursor) {
        let first = seed_cursor();
        let bucket = &token(&first)[PREFIX.len()..PREFIX.len() + 2];
        for i in 1..10000 {
            let mut second = first.clone();
            second.sequence = i;
            if &token(&second)[PREFIX.len()..PREFIX.len() + 2] == bucket {
                return (first, second);
            }
        }
        panic!("synthetic same-bucket search exhausted");
    }

    #[test]
    fn interrupted_pack_publication_all_byte_boundaries_preserve_issued_frames() {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let mut guard = StagingAdmission::acquire(root.path(), &req).unwrap();
        let (first, second) = same_bucket_pair();
        persist(root.path(), &mut guard, STAGING_LIMITS, &first, &req).unwrap();
        let path = pack_path(root.path(), &token(&first)[PREFIX.len()..]);
        let published = fs::read(&path).unwrap();
        let frame = format!(
            "{} {}\n",
            &token(&second)[PREFIX.len()..],
            serde_json::to_string(&second).unwrap()
        );
        // Every partial append, complete-before-fsync, and empty-file creation.
        for split in 0..=frame.len() {
            let mut bytes = published.clone();
            bytes.extend_from_slice(&frame.as_bytes()[..split]);
            fs::write(&path, bytes).unwrap();
            assert_eq!(
                token(&load(root.path(), &token(&first), &req).unwrap()),
                token(&first)
            );
            persist(root.path(), &mut guard, STAGING_LIMITS, &second, &req).unwrap();
            assert_eq!(
                token(&load(root.path(), &token(&second), &req).unwrap()),
                token(&second)
            );
            let complete = fs::read(&path).unwrap();
            assert_eq!(complete, [published.as_slice(), frame.as_bytes()].concat());
            persist(root.path(), &mut guard, (0, 0), &second, &req).unwrap();
            assert_eq!(fs::read(&path).unwrap(), complete);
        }
        fs::write(&path, []).unwrap();
        persist(root.path(), &mut guard, STAGING_LIMITS, &first, &req).unwrap();
        assert_eq!(fs::read(&path).unwrap(), published);
        let mut corrupted = published.clone();
        corrupted[65] ^= 1;
        fs::write(&path, &corrupted).unwrap();
        assert!(persist(root.path(), &mut guard, STAGING_LIMITS, &second, &req).is_err());
        assert!(load(root.path(), &token(&first), &req).is_err());
        assert_eq!(fs::read(&path).unwrap(), corrupted);
    }

    #[test]
    fn pack_allocation_boundary_dedup_and_growth() {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let mut guard = StagingAdmission::acquire(root.path(), &req).unwrap();
        let (first, second) = same_bucket_pair();
        persist(root.path(), &mut guard, (8192, 1), &first, &req).unwrap();
        // Same bucket append within already charged allocation works at object cap.
        persist(root.path(), &mut guard, (8192, 1), &second, &req).unwrap();
        let retained = files(root.path());
        persist(root.path(), &mut guard, (0, 0), &first, &req).unwrap();
        assert_eq!(files(root.path()), retained);
        let path = pack_path(root.path(), &token(&first)[PREFIX.len()..]);
        assert_eq!(retained.len(), 1);
        let before_len = fs::metadata(&path).unwrap().len();
        assert!(before_len <= 4096);
        assert_eq!(charged_bytes(before_len), 8192);
        // Select the bucket AFTER enlarging the serialized cursor: changing any
        // cursor field changes its digest and can otherwise test a new object.
        let mut enlarged = second.clone();
        enlarged.snapshot = "x".repeat(5000);
        let enlarged = (1..10000)
            .find_map(|sequence| {
                enlarged.sequence = sequence;
                (pack_path(root.path(), &token(&enlarged)[PREFIX.len()..]) == path)
                    .then(|| enlarged.clone())
            })
            .expect("synthetic enlarged same-bucket search exhausted");
        assert_ne!(token(&enlarged), token(&first));
        assert_ne!(token(&enlarged), token(&second));
        assert_eq!(
            pack_path(root.path(), &token(&enlarged)[PREFIX.len()..]),
            path
        );
        let frame = format!(
            "{} {}\n",
            &token(&enlarged)[PREFIX.len()..],
            serde_json::to_string(&enlarged).unwrap()
        );
        let after_len = before_len + frame.len() as u64;
        let after_charge = charged_bytes(after_len);
        assert!(after_len > 4096);
        assert!(after_charge > 8192);
        let err = persist(root.path(), &mut guard, (8192, 1), &enlarged, &req).unwrap_err();
        assert_eq!(err.code, "session_turn_staging_capacity_exceeded");
        assert!(!err.retryable);
        assert_eq!(files(root.path()), retained);
        // Raise only the byte cap: the same existing object must now grow.
        persist(root.path(), &mut guard, (after_charge, 1), &enlarged, &req).unwrap();
        assert_eq!(files(root.path()).len(), 1);
        assert_eq!(fs::metadata(&path).unwrap().len(), after_len);
        assert_eq!(
            fs::read(&path).unwrap(),
            [retained[0].1.as_slice(), frame.as_bytes()].concat()
        );
        assert_eq!((guard.bytes, guard.objects), (after_charge, 1));
        for cursor in [&first, &second, &enlarged] {
            assert_eq!(
                token(&load(root.path(), &token(cursor), &req).unwrap()),
                token(cursor)
            );
        }
    }

    #[test]
    fn new_pack_object_cap_refuses_without_mutation() {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let mut guard = StagingAdmission::acquire(root.path(), &req).unwrap();
        let first = seed_cursor();
        persist(root.path(), &mut guard, (8192, 1), &first, &req).unwrap();
        let path = pack_path(root.path(), &token(&first)[PREFIX.len()..]);
        let mut second = first.clone();
        let second = (1..10000)
            .find_map(|sequence| {
                second.sequence = sequence;
                (pack_path(root.path(), &token(&second)[PREFIX.len()..]) != path)
                    .then(|| second.clone())
            })
            .expect("synthetic different-bucket search exhausted");
        assert_ne!(
            pack_path(root.path(), &token(&second)[PREFIX.len()..]),
            path
        );
        assert!(serde_json::to_vec(&second).unwrap().len() + 66 <= 4096);
        let retained = files(root.path());
        // Enough bytes for both packs: only the new-object cap can refuse.
        let err = persist(root.path(), &mut guard, (16384, 1), &second, &req).unwrap_err();
        assert_eq!(err.code, "session_turn_staging_capacity_exceeded");
        assert!(!err.retryable);
        assert_eq!(files(root.path()), retained);
        fs::create_dir(root.path().join("unexpected-directory")).unwrap();
        assert!(persist(root.path(), &mut guard, STAGING_LIMITS, &second, &req).is_err());
        fs::remove_dir(root.path().join("unexpected-directory")).unwrap();
        persist(root.path(), &mut guard, (16384, 2), &second, &req).unwrap();
        assert_eq!((guard.bytes, guard.objects), (16384, 2));
        assert_eq!(files(root.path()).len(), 2);
        assert_eq!(fs::read(path).unwrap(), retained[0].1);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_pack_writers_and_readers_share_alias_lock() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("store");
        fs::create_dir(&directory).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = if i % 2 == 0 {
                    directory.clone()
                } else {
                    alias.clone()
                };
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let req = request(&path);
                    barrier.wait();
                    for j in 0..32 {
                        let mut cursor = seed_cursor();
                        cursor.sequence = j;
                        let mut guard = StagingAdmission::acquire(&path, &req).unwrap();
                        persist(&path, &mut guard, STAGING_LIMITS, &cursor, &req).unwrap();
                        assert_eq!(
                            token(&load(&path, &token(&cursor), &req).unwrap()),
                            token(&cursor)
                        );
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            files(&directory)
                .iter()
                .map(|(_, bytes)| bytes.iter().filter(|b| **b == b'\n').count())
                .sum::<usize>(),
            32
        );
    }

    #[test]
    fn real_production_budget_rejects_sparse_retained_exhaustion_without_mutation() {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let mut guard = StagingAdmission::acquire(root.path(), &req).unwrap();
        let first = seed_cursor();
        persist(root.path(), &mut guard, STAGING_LIMITS, &first, &req).unwrap();
        let orphan = File::create(root.path().join("retained-orphan")).unwrap();
        orphan.set_len(STAGING_LIMITS.0).unwrap();
        assert!(charged_bytes(u64::MAX) >= STAGING_LIMITS.0);
        let mut second = first.clone();
        second.sequence = 123;
        let error = persist(root.path(), &mut guard, STAGING_LIMITS, &second, &req).unwrap_err();
        assert_eq!(error.code, "session_turn_staging_capacity_exceeded");
        assert!(!error.retryable);
        persist(root.path(), &mut guard, STAGING_LIMITS, &first, &req).unwrap();
        assert_eq!(orphan.metadata().unwrap().len(), STAGING_LIMITS.0);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    #[ignore = "fixture subprocess only; private root required"]
    fn pack_process_fixture() {
        let root = PathBuf::from(std::env::var("AGE353_PACK_FIXTURE_ROOT").unwrap());
        let req = request(&root);
        for i in 0..32 {
            let mut cursor = seed_cursor();
            cursor.sequence = i;
            let mut guard = StagingAdmission::acquire(&root, &req).unwrap();
            persist(&root, &mut guard, STAGING_LIMITS, &cursor, &req).unwrap();
            assert_eq!(
                token(&load(&root, &token(&cursor), &req).unwrap()),
                token(&cursor)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn independent_process_pack_writers_share_alias_lock() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("store");
        fs::create_dir(&directory).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        let mut children: Vec<_> = (0..4)
            .map(|i| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "session_turn_pages::admission_tests::pack_process_fixture",
                        "--ignored",
                    ])
                    .env(
                        "AGE353_PACK_FIXTURE_ROOT",
                        if i % 2 == 0 { &directory } else { &alias },
                    )
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(
            files(&directory)
                .iter()
                .map(|(_, bytes)| bytes.iter().filter(|b| **b == b'\n').count())
                .sum::<usize>(),
            32
        );
    }
}
