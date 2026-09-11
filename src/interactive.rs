//! Managed Codex TUI launcher. The runner keeps PTY/process ownership; this
//! executable validates the account and replaces itself with the pinned CLI.
use crate::{
    account,
    envelope::{ProviderFailure, RequestEnvelope},
    launch, models,
    policy::{self, RuntimeConfig},
};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, process::Command};

#[derive(Default)]
struct Options {
    settings_id: Option<String>,
    model: Option<String>,
    native_model: Option<String>,
    native_effort: Option<String>,
    config_root: Option<String>,
    resume: Option<String>,
    prompt: Option<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        let slot = match argument.as_str() {
            "--settings-id" => &mut options.settings_id,
            "--model" => &mut options.model,
            "-m" => &mut options.native_model,
            "-c" => &mut options.native_effort,
            "--config-root" => &mut options.config_root,
            "--resume" => &mut options.resume,
            "--prompt" => &mut options.prompt,
            "--" => {
                if options.prompt.is_some() {
                    return Err("Only one interactive prompt is allowed".into());
                }
                options.prompt = iter.next().cloned();
                if iter.next().is_some() {
                    return Err("Only one interactive prompt is allowed".into());
                }
                break;
            }
            text if !text.starts_with('-') => {
                if options.prompt.replace(text.into()).is_some() {
                    return Err("Only one interactive prompt is allowed".into());
                }
                continue;
            }
            _ => {
                return Err(format!(
                    "Unsupported managed interactive option: {argument}"
                ))
            }
        };
        if slot.is_some() {
            return Err(format!("Duplicate managed interactive option: {argument}"));
        }
        *slot = Some(
            iter.next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("{argument} requires a value"))?
                .clone(),
        );
    }
    if options.settings_id.is_none() {
        return Err("interactive requires --settings-id".into());
    }
    Ok(options)
}

fn invalid(message: impl Into<String>) -> ProviderFailure {
    ProviderFailure::invalid_settings("", "invalid_interactive_launch", message, json!({}))
}

fn isolated_home(account_home: &std::path::Path) -> Result<PathBuf, ProviderFailure> {
    #[cfg(unix)]
    {
        let root = account_home.join("agent-runner-managed/tui");
        crate::durable_fs::create_private_directories(&root)
            .map_err(|_| invalid("Cannot create private managed Codex configuration home"))?;
        let home = tempfile::Builder::new()
            .prefix("launch-")
            .tempdir_in(&root)
            .map_err(|_| invalid("Cannot create isolated Codex configuration home"))?;
        // Keep native auth and storage in their owning account. In pinned Codex
        // 0.153.4 file authentication refresh truncates through the auth symlink.
        // Sharing writer locks is mandatory when transcripts are shared.
        for name in ["sessions", "archived_sessions", "thread-writer-locks"] {
            std::fs::create_dir_all(account_home.join(name))
                .map_err(|_| invalid("Cannot prepare native Codex session storage"))?;
            std::os::unix::fs::symlink(account_home.join(name), home.path().join(name))
                .map_err(|_| invalid("Cannot bind native Codex session storage"))?;
        }
        std::os::unix::fs::symlink(
            account_home.join("auth.json"),
            home.path().join("auth.json"),
        )
        .map_err(|_| invalid("Cannot bind native Codex authentication"))?;
        std::fs::write(home.path().join("config.toml"), "")
            .map_err(|_| invalid("Cannot initialize isolated Codex configuration"))?;
        // Native SQLite stores absolute rollout paths through this home. Keep
        // the tiny profile directory so future resumes can resolve those paths.
        // Native resolves CODEX_HOME before assigning User-layer hook keys.
        // Bind declarations to the physical selected home, including symlinked accounts.
        std::fs::canonicalize(home.keep())
            .map_err(|_| invalid("Cannot resolve the physical managed Codex configuration home"))
    }
    #[cfg(not(unix))]
    {
        let _ = account_home;
        Err(invalid(
            "Managed interactive Codex currently requires Unix symlinks and PTY support",
        ))
    }
}

fn prepare(args: &[String]) -> Result<Command, ProviderFailure> {
    let options = parse(args).map_err(invalid)?;
    let settings_id = options.settings_id.as_deref().unwrap();
    let label = match (&options.model, &options.native_model, &options.native_effort) {
        (Some(label), None, None) => label.clone(),
        (None, None, None) => "gpt-xhigh".into(),
        (None, Some(model), Some(effort)) => models::catalog().into_iter()
            .find(|entry| entry["provider_model"] == *model && entry["provider_args"][3] == *effort)
            .and_then(|entry| entry["name"].as_str().map(str::to_owned))
            .ok_or_else(|| invalid("Legacy model arguments must exactly select a managed model and reasoning effort"))?,
        _ => return Err(invalid("Select --model LABEL or the exact legacy -m MODEL -c reasoning pair")),
    };
    let (model, effort) =
        models::route(&label).ok_or_else(|| invalid("Unknown managed model label"))?;
    let cwd = std::env::current_dir().map_err(|_| invalid("Cannot resolve working directory"))?;
    let config_root = options
        .config_root
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("OULIPOLY_CONFIG_HOME")
                .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
                .map(|root| PathBuf::from(root).join("oulipoly-agent-runner"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".config/oulipoly-agent-runner"))
        })
        .filter(|path| path.is_absolute())
        .ok_or_else(|| invalid("--config-root must be an absolute directory"))?;
    let mut argv = vec![
        settings_id.to_string(),
        "exec".into(),
        "--dangerously-bypass-approvals-and-sandbox".into(),
    ];
    argv.extend(models::args(model, effort));
    let mut request: RequestEnvelope = serde_json::from_value(json!({
        "contract":"oulipoly.provider/v1","request_id":"interactive-admission",
        "provider_instance_id":settings_id,
        "host":{"app":"managed-codex-interactive","config_root":config_root,"working_directory":cwd},
        "params":{"settings_id":settings_id,"mode":"arg",
            "model":{"name":label,"provider_args":models::args(model,effort),
                "inputs":{"prompt":"interactive admission","named":{}}},
            "launch":{"argv":argv,"env":{}}}
    })).unwrap();
    // The runner's generic PTY policy has no initial prompt to transform. Read
    // account instructions/restrictions from their source instead of dropping
    // them or maintaining a second provider-specific copy.
    let providers_path = config_root.join("providers.toml");
    if providers_path.exists() {
        let bytes = crate::durable_fs::read_file_bounded(&providers_path, 4 * 1024 * 1024)
            .map_err(|_| invalid("Cannot read bounded providers.toml"))?;
        let providers: toml::Value = toml::from_str(
            std::str::from_utf8(&bytes).map_err(|_| invalid("providers.toml must be UTF-8"))?,
        )
        .map_err(|_| invalid("Cannot parse providers.toml"))?;
        let account = providers
            .get(settings_id)
            .and_then(toml::Value::as_table)
            .ok_or_else(|| invalid("Selected account is missing from providers.toml"))?;
        for key in ["system_prompt_override", "tool_restrictions"] {
            if let Some(value) = account.get(key) {
                if key == "system_prompt_override" && !value.is_str() {
                    return Err(invalid("Account system_prompt_override must be a string"));
                }
                request.params["launch"][key] = serde_json::to_value(value).unwrap();
            }
        }
    }
    let plan = policy::plan(&request, true)?;
    let mut config = RuntimeConfig::load(&request.host)?;
    let account_home = account::home(&request.host, settings_id)?;
    if let Some(session) = options.resume.as_deref() {
        if !launch::valid_thread_id(session) {
            return Err(invalid("--resume requires a native Codex session UUID"));
        }
        let mut locate = request.clone();
        locate.params = json!({"settings_id":settings_id,"session_id":session});
        if crate::session::handle("session.locate_transcript", &locate)?["located"] != true {
            return Err(invalid(
                "The selected account does not own this Codex session",
            ));
        }
    }
    let mut env: BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    env.remove("AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS");
    env.extend(plan.env.clone());
    if env
        .get("AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS")
        .is_some_and(|value| value.trim().is_empty())
    {
        env.remove("AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS");
    }
    env.insert(
        "AGENT_BASH_BIN".into(),
        config.agent_bash_bin.display().to_string(),
    );
    env.insert(
        "AGENT_BASH_AGENT_RUNNER_BIN".into(),
        config.agent_runner_bin.display().to_string(),
    );
    env.insert(
        "AGENT_RUNNER_CODEX_SESSION_BINDING".into(),
        "tool_metadata".into(),
    );
    env.insert("AGENT_RUNNER_CODEX_INTERACTIVE".into(), "1".into());
    let invocation = env
        .get("OULIPOLY_PARENT_INVOCATION")
        .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok());
    if !invocation
        .as_ref()
        .and_then(|v| v["id"].as_str())
        .is_some_and(launch::valid_thread_id)
        || !env
            .get("OULIPOLY_LIVE_SESSION_BIND_SOCKET")
            .is_some_and(|v| std::path::Path::new(v).is_absolute())
        || !env
            .get("OULIPOLY_LIVE_SESSION_BIND_TOKEN")
            .is_some_and(|v| !v.is_empty())
    {
        return Err(invalid("Interactive registration requires authenticated Agent Runner launch authority; start this session through Agent Runner"));
    }

    // Inherited parent identity is never evidence for this newly created TUI.
    for key in [
        "AGENT_RUNNER_CODEX_SESSION_FILE",
        "AGENT_RUNNER_CODEX_SESSION_ID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
    ] {
        env.remove(key);
    }
    if let Some(id) = options.resume.as_deref() {
        env.insert("AGENT_RUNNER_CODEX_SESSION_ID".into(), id.into());
    }
    let home = isolated_home(&account_home)?;
    crate::registration::stage(&mut config, &config_root, &home)?;
    config.validate()?;
    launch::verify_version(&config, &env, &request)?;
    crate::registration::declaration(&home, &config)?;
    env.insert(
        "AGENT_RUNNER_CODEX_REGISTRATION_CWD".into(),
        cwd.display().to_string(),
    );
    env.insert("CODEX_HOME".into(), home.display().to_string());
    env.insert(
        "CODEX_SQLITE_HOME".into(),
        account_home.display().to_string(),
    );
    let mut native_args = launch::managed_native_args(&config, &plan, &env);
    // Only registration-enabled TUI permits native hooks. All other disabled
    // features remain unchanged; system/managed hooks are explicitly authorized.
    for arg in &mut native_args {
        if arg == "features.hooks=false" {
            *arg = "features.hooks=true".into();
        }
    }
    // TUI does not expose exec's --ignore-user-config/--ignore-rules. The
    // isolated home removes the user layer; explicit untrusted cwd disables
    // project layers without a trust prompt (pinned tui/src/lib.rs tests).
    for setting in [
        "project_root_markers=[]".to_string(),
        // Codex splits override keys on every dot without TOML quoted-key
        // parsing. Supply the entire table so paths with dots/quotes work.
        format!("projects={{{}={{trust_level=\"untrusted\"}}}}", json!(cwd)),
        format!("sqlite_home={}", json!(account_home)),
        "cli_auth_credentials_store=\"file\"".to_string(),
    ] {
        native_args.extend(["-c".into(), setting]);
    }
    crate::registration_compatibility::check(&config, &env, &cwd, &native_args)?;
    if let Some(id) = options.resume {
        native_args.extend(["resume".into(), id]);
    }
    if let Some(prompt) = options.prompt {
        native_args.extend(["--".into(), prompt]);
    }
    let mut command = Command::new(&config.codex_bin);
    for key in [
        "AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS",
        "AGENT_RUNNER_CODEX_SESSION_FILE",
        "AGENT_RUNNER_CODEX_SESSION_ID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
    ] {
        command.env_remove(key);
    }
    command.args(native_args).envs(env).current_dir(cwd);
    Ok(command)
}

pub fn run(args: &[String]) -> i32 {
    if args == ["--help"] || args == ["-h"] {
        println!("agent-runner-codex interactive --settings-id ACCOUNT [--config-root PATH] [--model LABEL] [--resume UUID] [--prompt TEXT]\n\nManaged Codex TUI with Agent Bash. The default model is gpt-xhigh.\nExact legacy -m MODEL -c 'model_reasoning_effort=\"EFFORT\"' selection is also supported.");
        return 0;
    }
    let mut command = match prepare(args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!(
                "Managed Codex interactive launch rejected: {}",
                error.message
            );
            return error.exit_code;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("Could not start managed Codex TUI: {error}");
        1
    }
    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(error) => {
                eprintln!("Could not start managed Codex TUI: {error}");
                1
            }
        }
    }
}
