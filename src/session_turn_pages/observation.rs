//! Authenticated, source-backed observation cursors; no per-page storage.
use super::*;
use crate::encoding::{decode_base64, encode_base64};
use sha2::{Digest, Sha256};

const PREFIX: &str = "codex-obs1-";

// Hash the binding rather than embedding account paths or arbitrary settings in
// tokens. This also bounds token size independently of host identity lengths.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservationCursor {
    binding: String,
    kind: String,
    budgets: Budgets,
    stamp: Stamp,
    snapshot: String,
    offset: u64,
    page: u64,
    sequence: u64,
    partial_record: Option<PartialRecord>,
}

fn binding_digest(binding: &Binding) -> String {
    sha256_hex(&serde_json::to_vec(binding).unwrap())
}

// HMAC-SHA256 (RFC 2104), fixed 32-byte key; tested against RFC 4231.
fn authenticate(key: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    let mut inner_pad = [0x36; 64];
    let mut outer_pad = [0x5c; 64];
    for i in 0..key.len() {
        inner_pad[i] ^= key[i];
        outer_pad[i] ^= key[i];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(bytes);
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner.finalize());
    outer.finalize().into()
}

pub(super) fn is_token(value: &str) -> bool {
    value.starts_with(PREFIX)
}

pub(super) fn token(key: &[u8; 32], cursor: &Cursor) -> String {
    let compact = ObservationCursor {
        binding: binding_digest(&cursor.binding),
        kind: cursor.kind.clone(),
        budgets: cursor.budgets.clone(),
        stamp: cursor.stamp.clone(),
        snapshot: cursor.snapshot.clone(),
        offset: cursor.offset,
        page: cursor.page,
        sequence: cursor.sequence,
        partial_record: cursor.partial_record.clone(),
    };
    let mut bytes = serde_json::to_vec(&compact).unwrap();
    bytes.extend_from_slice(&authenticate(key, &bytes));
    format!("{PREFIX}{}", encode_base64(&bytes))
}

pub(super) fn load(
    key: &[u8; 32],
    value: &str,
    binding: &Binding,
    request: &RequestEnvelope,
) -> Result<Cursor, ProviderFailure> {
    let encoded = value.strip_prefix(PREFIX).ok_or_else(|| stale(request))?;
    let bytes = decode_base64(encoded).map_err(|_| stale(request))?;
    // Reject alternate encodings as well as truncation. Input is <=4096 bytes.
    if bytes.len() < 32 || encode_base64(&bytes) != encoded {
        return Err(stale(request));
    }
    let (payload, supplied_mac) = bytes.split_at(bytes.len() - 32);
    let expected_mac = authenticate(key, payload);
    let difference = supplied_mac
        .iter()
        .zip(expected_mac)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b));
    if difference != 0 {
        return Err(stale(request));
    }
    let compact: ObservationCursor = serde_json::from_slice(payload).map_err(|_| stale(request))?;
    if compact.binding != binding_digest(binding) || compact.offset > compact.stamp.len {
        return Err(stale(request));
    }
    Ok(Cursor {
        binding: binding.clone(),
        kind: compact.kind,
        budgets: compact.budgets,
        stamp: compact.stamp,
        snapshot: compact.snapshot,
        offset: compact.offset,
        page: compact.page,
        sequence: compact.sequence,
        partial_record: compact.partial_record,
    })
}

pub(super) fn key(root: &Path, request: &RequestEnvelope) -> Result<[u8; 32], ProviderFailure> {
    // Sibling of canonical admission, never a file inside its retained pool.
    // Exactly one fixed-size object; no temp/orphan accumulation on interruption.
    let root = root
        .parent()
        .ok_or_else(|| io_error(request))?
        .join("observation-auth-v1");
    crate::durable_fs::create_private_directories(&root).map_err(|_| io_error(request))?;
    let lock = File::open(&root).map_err(|_| io_error(request))?;
    fs2::FileExt::lock_exclusive(&lock).map_err(|_| io_error(request))?;
    let path = root.join("key");
    if !path.exists() {
        // Missing authority for a retained observation token is not a request
        // to rotate it. Existing tokens must fail closed, never be re-signed.
        if request.params["page_token"].as_str().is_some_and(is_token)
            || request.params["after_token"].as_str().is_some_and(is_token)
        {
            return Err(stale(request));
        }
        let mut bytes = [0u8; 32];
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(|_| io_error(request))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|_| io_error(request))?;
        file.write_all(&bytes).map_err(|_| io_error(request))?;
        file.sync_all().map_err(|_| io_error(request))?;
        lock.sync_all().map_err(|_| io_error(request))?;
    }
    // A short/interrupted or oversized key is an explicit error, not rotation.
    let bytes = crate::durable_fs::read_file_bounded(&path, 32).map_err(|_| io_error(request))?;
    // Retry durability even if a previous writer failed after publication.
    File::open(&path)
        .and_then(|file| file.sync_all())
        .map_err(|_| io_error(request))?;
    lock.sync_all().map_err(|_| io_error(request))?;
    bytes.try_into().map_err(|_| io_error(request))
}

pub(super) fn reconstruct(
    file: &mut File,
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
    if len == 0 || len >= MAX_RECORD_BYTES as u64 {
        return Err(stale(request));
    }
    file.seek(SeekFrom::Start(partial.start))
        .map_err(|_| io_error(request))?;
    let mut bytes = Vec::new();
    file.take(len)
        .read_to_end(&mut bytes)
        .map_err(|_| io_error(request))?;
    if bytes.len() as u64 != len || sha256_hex(&bytes) != partial.sha256 || bytes.contains(&b'\n') {
        return Err(stale(request));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_case_one() {
        let mut key = [0u8; 32];
        key[..20].fill(0x0b);
        let mac = authenticate(&key, b"Hi There");
        let hex = mac.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
}
