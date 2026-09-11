//! Provider-owned global registration and per-launch release-payload publication.
use crate::{envelope::ProviderFailure, policy::RuntimeConfig};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

const ASSETS: &[(&str, &[u8])] = &[
    (
        "codex/agent-bash-mcp.ts",
        include_bytes!("../integrations/codex/agent-bash-mcp.ts"),
    ),
    (
        "codex/session-registration.ts",
        include_bytes!("../integrations/codex/session-registration.ts"),
    ),
    (
        "codex/opencode-tool-shim.ts",
        include_bytes!("../integrations/codex/opencode-tool-shim.ts"),
    ),
    (
        "codex/models.json",
        include_bytes!("../integrations/codex/models.json"),
    ),
    (
        "opencode/tools/bash.ts",
        include_bytes!("../integrations/opencode/tools/bash.ts"),
    ),
];

fn failure() -> ProviderFailure {
    ProviderFailure::invalid_settings("", "registration_integration_unavailable",
        "Cannot safely prepare Codex registration/Bash integration. Run the selected release's scripts/install-provider.py with its built --binary and explicit dependency paths; do not upgrade native Codex or edit hook trust globally.", json!({}))
}

/// The running release, not an editable adjacent manifest, owns these bytes.
/// Missing/stale default installation assets are repaired into a NEW private
/// launch generation; no existing generation or user-custom file is rewritten.
pub(crate) fn stage(
    config: &mut RuntimeConfig,
    config_root: &Path,
    home: &Path,
) -> Result<(), ProviderFailure> {
    let default = config_root.join("agent-runner-codex/integrations/codex/agent-bash-mcp.ts");
    if config.bash_mcp_path != default {
        // Custom installs are read-only inputs, accepted only if release-coherent.
        let parent = config
            .bash_mcp_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(failure)?;
        if config.bash_mcp_path != parent.join("codex/agent-bash-mcp.ts") {
            return Err(failure());
        }
        for (name, bytes) in ASSETS {
            let path = parent.join(name);
            let meta = fs::symlink_metadata(&path).map_err(|_| failure())?;
            if !meta.is_file()
                || crate::durable_fs::read_file_bounded(&path, bytes.len())
                    .map_err(|_| failure())?
                    != *bytes
            {
                return Err(failure());
            }
        }
    }
    for path in [
        &config.codex_bin,
        &config.bun_bin,
        &config.agent_bash_bin,
        &config.agent_runner_bin,
        &std::path::PathBuf::from("/bin/sh"),
    ] {
        if !path.is_absolute() || !crate::durable_fs::is_executable_file(path).unwrap_or(false) {
            return Err(failure());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let m = fs::metadata(path).map_err(|_| failure())?;
            if m.mode() & 0o022 != 0 || (m.uid() != unsafe { libc::geteuid() } && m.uid() != 0) {
                return Err(failure());
            }
        }
    }
    let pending = tempfile::Builder::new()
        .prefix(".integration-")
        .tempdir_in(home)
        .map_err(|_| failure())?;
    for (name, bytes) in ASSETS {
        let path = pending.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).map_err(|_| failure())?;
        fs::write(&path, bytes).map_err(|_| failure())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).map_err(|_| failure())?;
        }
        if fs::read(&path).map_err(|_| failure())? != *bytes {
            return Err(failure());
        }
        fs::File::open(&path)
            .and_then(|f| f.sync_all())
            .map_err(|_| failure())?;
    }
    let published = home.join("integrations");
    fs::rename(pending.path(), &published).map_err(|_| failure())?;
    fs::File::open(home)
        .and_then(|f| f.sync_all())
        .map_err(|_| failure())?;
    config.bash_mcp_path = published.join("codex/agent-bash-mcp.ts");
    Ok(())
}

pub(crate) fn declaration(home: &Path, config: &RuntimeConfig) -> Result<(), ProviderFailure> {
    let script = config
        .bash_mcp_path
        .parent()
        .unwrap()
        .join("session-registration.ts");
    let stop = r#"{"continue":false,"stopReason":"Codex session registration helper failed; exit and relaunch with Agent Runner"}"#;
    // Preserve empty success stdout and turn helper launch failure into explicit
    // native structured stop. Native timeout/SIGKILL can still bypass this shell.
    let command = format!("if result=$({} --no-install {}); then [ -z \"$result\" ] || printf '%s\\n' \"$result\"; else printf '%s\\n' {}; fi",
        quote(&config.bun_bin)?, quote(&script)?, quote(Path::new(stop))?);
    // The outer native shell need only launch a quoted executable and arguments;
    // provider control syntax does not depend on the user's Bash/Zsh/Fish choice.
    let command = format!("'/bin/sh' '-c' {}", quote(Path::new(&command))?);
    let handler = json!({"type":"command", "command":command, "timeout":15, "async":false});
    let mut hooks = serde_json::Map::new();
    let mut states = serde_json::Map::new();
    for (event, label) in [
        ("SessionStart", "session_start"),
        ("UserPromptSubmit", "user_prompt_submit"),
    ] {
        // Native normalizes timeout and omits None-valued TOML fields before
        // canonical JSON hashing; this is declaration trust, NOT artifact trust.
        let identity = json!({"event_name":label, "hooks":[handler.clone()]});
        let hash = format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&identity).unwrap())
        );
        hooks.insert(event.into(), json!([{"hooks":[handler.clone()]}]));
        states.insert(
            format!("{}:{label}:0:0", home.join("config.toml").display()),
            json!({"enabled":true,"trusted_hash":hash}),
        );
    }
    hooks.insert("state".into(), states.into());
    let value: toml::Value =
        serde_json::from_value(json!({"hooks":hooks})).map_err(|_| failure())?;
    let text = toml::to_string(&value).map_err(|_| failure())?;
    let mut pending = tempfile::NamedTempFile::new_in(home).map_err(|_| failure())?;
    use std::io::Write;
    pending
        .write_all(text.as_bytes())
        .and_then(|_| pending.as_file().sync_all())
        .map_err(|_| failure())?;
    pending
        .persist(home.join("config.toml"))
        .map_err(|_| failure())?;
    Ok(())
}

fn quote(path: &Path) -> Result<String, ProviderFailure> {
    let text = path
        .to_str()
        .filter(|s| !s.contains(['\0', '\n', '\r']))
        .ok_or_else(failure)?;
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}
