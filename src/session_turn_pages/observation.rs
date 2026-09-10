//! Authenticated, source-backed observation cursors; no per-page storage.
use super::*;
use crate::encoding::{decode_base64, encode_base64};
use sha2::{Digest, Sha256};
use std::fs;

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

// One reserved preparation slot, never used to sign tokens. All cooperating
// initializers hold the *directory inode* lock until publication is durable.
const PREPARATION: &str = "key.preparing";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationPoint {
    Created,
    Written,
    CandidateSynced,
    Published,
    FinalSynced,
    DirectorySynced,
}

pub(super) fn key(root: &Path, request: &RequestEnvelope) -> Result<[u8; 32], ProviderFailure> {
    key_with_observer(root, request, |_| Ok(()))
}

fn key_with_observer(
    root: &Path,
    request: &RequestEnvelope,
    mut observe: impl FnMut(PublicationPoint) -> std::io::Result<()>,
) -> Result<[u8; 32], ProviderFailure> {
    // Sibling of canonical admission, never inside its retained pool.
    let root = root
        .parent()
        .ok_or_else(|| io_error(request))?
        .join("observation-auth-v1");
    match fs::symlink_metadata(&root) {
        Ok(metadata) if !metadata.file_type().is_dir() => return Err(io_error(request)),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(io_error(request));
        }
        _ => {}
    }
    crate::durable_fs::create_private_directories(&root).map_err(|_| io_error(request))?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    }
    let lock = options.open(&root).map_err(|_| io_error(request))?;
    fs2::FileExt::lock_exclusive(&lock).map_err(|_| io_error(request))?;
    let path = root.join("key");
    let published = match fs::symlink_metadata(&path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(io_error(request)),
    };
    if !published {
        // Missing authority for an issued token never authorizes initialization
        // or preparation recovery, even if a complete candidate is present.
        if request.params["page_token"].as_str().is_some_and(is_token)
            || request.params["after_token"].as_str().is_some_and(is_token)
        {
            return Err(stale(request));
        }
        prepare_and_publish(&root, &mut observe).map_err(|_| io_error(request))?;
    }
    // Existing final authority is immutable. A malformed final may be damaged
    // issued authority (including an old initializer's short publication), not
    // demonstrably unissued preparation. Do not repair or remove it.
    let mut file = open_private_key_file(&path).map_err(|_| malformed_key(request))?;
    let mut bytes = [0u8; 32];
    if file.metadata().map_err(|_| io_error(request))?.len() != 32 {
        return Err(malformed_key(request));
    }
    file.read_exact(&mut bytes).map_err(|_| io_error(request))?;
    // Also finish durability after interruption following rename. No signing
    // identity leaves this function before both file and directory sync succeed.
    file.sync_all().map_err(|_| io_error(request))?;
    observe(PublicationPoint::FinalSynced).map_err(|_| io_error(request))?;
    lock.sync_all().map_err(|_| io_error(request))?;
    observe(PublicationPoint::DirectorySynced).map_err(|_| io_error(request))?;
    Ok(bytes)
}

fn malformed_key(request: &RequestEnvelope) -> ProviderFailure {
    ProviderFailure::internal(
        &request.request_id,
        "session_turn_page_io",
        "Published observation authentication key is malformed or unsafe; refusing replacement of potentially issued authority",
    )
}

fn open_private_key_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    let mut safe = metadata.is_file();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        safe &= metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1
            && metadata.mode() & 0o077 == 0;
    }
    if !safe {
        return Err(std::io::Error::other("unsafe key file"));
    }
    Ok(file)
}

fn prepare_and_publish(
    root: &Path,
    observe: &mut impl FnMut(PublicationPoint) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let preparation = root.join(PREPARATION);
    // Under the same lock, absent final authority proves this reserved slot
    // could not have issued a token. Discard even a complete candidate: its
    // bytes were never returned. Only one bounded, private regular file can be
    // recovered; no scans, wildcard cleanup, link following or broad GC.
    match fs::symlink_metadata(&preparation) {
        Ok(_) => {
            let file = open_private_key_file(&preparation)?;
            if file.metadata()?.len() > 32 {
                return Err(std::io::Error::other("oversized key preparation"));
            }
            fs::remove_file(&preparation)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&preparation)?;
    observe(PublicationPoint::Created)?;
    file.write_all(&bytes)?;
    observe(PublicationPoint::Written)?;
    file.sync_all()?;
    observe(PublicationPoint::CandidateSynced)?;
    // Same-directory rename publishes only a complete synced file. The shared
    // directory lock excludes old cooperating initializers too, but cannot
    // prevent an old initializer from stranding a short final before cutover.
    fs::rename(&preparation, root.join("key"))?;
    observe(PublicationPoint::Published)?;
    Ok(())
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

    fn request() -> RequestEnvelope {
        serde_json::from_value(json!({
            "contract": "oulipoly.provider/v1", "request_id": "key-test",
            "host": {"app": "test"}, "params": {}
        }))
        .unwrap()
    }

    fn auth(root: &Path) -> PathBuf {
        root.join("observation-auth-v1")
    }

    fn child(root: &Path, mode: &str) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "session_turn_pages::observation::tests::key_publication_subprocess",
                "--ignored",
                "--nocapture",
            ])
            .env("KEY_TEST_ROOT", root)
            .env("KEY_TEST_MODE", mode);
        command
    }

    // Executed only by the bounded parent fixtures, never against host state.
    #[test]
    #[ignore]
    fn key_publication_subprocess() {
        let root = PathBuf::from(std::env::var_os("KEY_TEST_ROOT").unwrap());
        let mode = std::env::var("KEY_TEST_MODE").unwrap();
        if mode == "concurrent" {
            let id = std::env::var("KEY_TEST_ID").unwrap();
            fs::write(root.join(format!("ready-{id}")), b"").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !root.join("go").exists() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let bytes = key(&root.join("session-pages-v1"), &request()).unwrap();
            fs::write(root.join(format!("result-{id}")), sha256_hex(&bytes)).unwrap();
            return;
        }
        let result = key_with_observer(&root.join("session-pages-v1"), &request(), |point| {
            if mode == "partial" && point == PublicationPoint::Created {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(auth(&root).join(PREPARATION))
                    .unwrap()
                    .write_all(&[1u8; 16])
                    .unwrap();
                std::process::exit(86);
            }
            if format!("{point:?}") == mode {
                // No destructors or cleanup: leave exactly the process-exit
                // filesystem state at this production publication boundary.
                std::process::exit(86);
            }
            Ok(())
        });
        panic!("interruption boundary was not reached: {result:?}");
    }

    #[test]
    fn interrupted_first_publication_recovers_without_changing_published_identity() {
        for (boundary, published, candidate_len) in [
            ("Created", false, 0),
            ("partial", false, 16),
            ("Written", false, 32),
            ("CandidateSynced", false, 32),
            ("Published", true, 0),
            ("FinalSynced", true, 0),
            ("DirectorySynced", true, 0),
        ] {
            let root = tempfile::tempdir().unwrap();
            let status = child(root.path(), boundary).status().unwrap();
            assert_eq!(status.code(), Some(86), "{boundary}");
            let final_path = auth(root.path()).join("key");
            assert_eq!(final_path.exists(), published, "{boundary}");
            let published_digest = published.then(|| sha256_hex(&fs::read(&final_path).unwrap()));
            if !published {
                assert_eq!(
                    fs::metadata(auth(root.path()).join(PREPARATION))
                        .unwrap()
                        .len(),
                    candidate_len,
                    "{boundary}"
                );
            }
            let bytes = key(&root.path().join("session-pages-v1"), &request()).unwrap();
            if let Some(digest) = published_digest {
                assert_eq!(sha256_hex(&bytes), digest, "{boundary}");
            }
            assert_eq!(fs::read_dir(auth(root.path())).unwrap().count(), 1);
            assert_eq!(fs::metadata(&final_path).unwrap().len(), 32);
            assert_eq!(
                bytes,
                key(&root.path().join("session-pages-v1"), &request()).unwrap()
            );
        }
    }

    #[test]
    fn concurrent_process_first_creators_return_one_immutable_key() {
        let root = tempfile::tempdir().unwrap();
        // Start from interrupted preparation, combining recovery and exclusion.
        assert_eq!(
            child(root.path(), "partial").status().unwrap().code(),
            Some(86)
        );
        let mut children: Vec<_> = (0..8)
            .map(|id| {
                child(root.path(), "concurrent")
                    .env("KEY_TEST_ID", id.to_string())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !(0..8).all(|id| root.path().join(format!("ready-{id}")).exists()) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        fs::write(root.path().join("go"), b"").unwrap();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let digest = sha256_hex(&key(&root.path().join("session-pages-v1"), &request()).unwrap());
        for id in 0..8 {
            assert_eq!(
                fs::read_to_string(root.path().join(format!("result-{id}"))).unwrap(),
                digest
            );
        }
        assert_eq!(fs::read_dir(auth(root.path())).unwrap().count(), 1);
    }

    #[test]
    fn publication_errors_never_return_authority_and_visible_final_is_resynced() {
        for failed in [
            PublicationPoint::CandidateSynced,
            PublicationPoint::Published,
            PublicationPoint::FinalSynced,
            PublicationPoint::DirectorySynced,
        ] {
            let root = tempfile::tempdir().unwrap();
            let paging = root.path().join("session-pages-v1");
            let result = key_with_observer(&paging, &request(), |point| {
                if point == failed {
                    Err(std::io::Error::other("injected publication failure"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            let path = auth(root.path()).join("key");
            let prior = path.exists().then(|| sha256_hex(&fs::read(&path).unwrap()));
            let mut points = Vec::new();
            let bytes = key_with_observer(&paging, &request(), |point| {
                points.push(point);
                Ok(())
            })
            .unwrap();
            assert!(points.ends_with(&[
                PublicationPoint::FinalSynced,
                PublicationPoint::DirectorySynced
            ]));
            if let Some(prior) = prior {
                assert_eq!(sha256_hex(&bytes), prior);
                assert_eq!(
                    points,
                    vec![
                        PublicationPoint::FinalSynced,
                        PublicationPoint::DirectorySynced
                    ]
                );
            }
        }
    }

    #[test]
    fn malformed_published_key_and_missing_issued_authority_are_never_repaired() {
        for len in [0, 16, 33] {
            let root = tempfile::tempdir().unwrap();
            let paging = root.path().join("session-pages-v1");
            key(&paging, &request()).unwrap();
            let path = auth(root.path()).join("key");
            fs::write(&path, vec![1u8; len]).unwrap();
            let error = key(&paging, &request()).unwrap_err();
            assert_eq!(error.code, "session_turn_page_io");
            assert!(error.message.contains("refusing replacement"));
            assert_eq!(fs::read(&path).unwrap(), vec![1u8; len]);
            assert_eq!(fs::read_dir(auth(root.path())).unwrap().count(), 1);
        }
        for field in ["page_token", "after_token"] {
            let root = tempfile::tempdir().unwrap();
            assert_eq!(
                child(root.path(), "CandidateSynced")
                    .status()
                    .unwrap()
                    .code(),
                Some(86)
            );
            let preparation = auth(root.path()).join(PREPARATION);
            let original = fs::read(&preparation).unwrap();
            let mut request = request();
            request.params[field] = json!("codex-obs1-issued-authority-missing");
            assert_eq!(
                key(&root.path().join("session-pages-v1"), &request)
                    .unwrap_err()
                    .code,
                "session_turn_page_token_stale"
            );
            assert_eq!(fs::read(&preparation).unwrap(), original);
            assert!(!auth(root.path()).join("key").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_paths_and_unbounded_residue_are_refused_without_cleanup() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        for name in ["key", PREPARATION] {
            for kind in ["symlink", "directory", "oversized", "hardlink", "public"] {
                let root = tempfile::tempdir().unwrap();
                crate::durable_fs::create_private_directories(&auth(root.path())).unwrap();
                let outside = root.path().join("unrelated");
                fs::write(&outside, [2u8; 32]).unwrap();
                fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
                let path = auth(root.path()).join(name);
                match kind {
                    "symlink" => symlink(&outside, &path).unwrap(),
                    "directory" => fs::create_dir(&path).unwrap(),
                    "hardlink" => fs::hard_link(&outside, &path).unwrap(),
                    _ => {
                        fs::write(&path, vec![3u8; if kind == "oversized" { 33 } else { 32 }])
                            .unwrap();
                        fs::set_permissions(
                            &path,
                            fs::Permissions::from_mode(if kind == "public" {
                                0o644
                            } else {
                                0o600
                            }),
                        )
                        .unwrap();
                    }
                }
                assert!(
                    key(&root.path().join("session-pages-v1"), &request()).is_err(),
                    "{name}/{kind}"
                );
                assert!(fs::symlink_metadata(&path).is_ok());
                assert_eq!(fs::read(&outside).unwrap(), [2u8; 32]);
                assert_eq!(fs::read_dir(auth(root.path())).unwrap().count(), 1);
            }
        }
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("unrelated-dir");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&outside, auth(root.path())).unwrap();
        assert!(key(&root.path().join("session-pages-v1"), &request()).is_err());
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

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
