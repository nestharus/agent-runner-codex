// Test harness against unmodified OpenAI Codex rust-v0.153.4 public crates.
// Read-only filesystem fixture adapted from upstream config/src/loader/tests.rs
// (OpenAI, Apache-2.0); constrained to the supplied private physical fixture root.
use codex_config::{AbsolutePathBuf, ConfigLoadOptions, LoaderOverrides, NoopThreadConfigLoader};
use codex_file_system::*;
use codex_hooks::*;
use codex_utils_path_uri::PathUri;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
struct TestFileSystem {
    root: PathBuf,
}
impl TestFileSystem {
    fn check(&self, path: &PathUri) -> std::io::Result<AbsolutePathBuf> {
        let p = path.to_abs_path()?;
        if !p.as_path().starts_with(&self.root) {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        Ok(p)
    }
}
impl ExecutorFileSystem for TestFileSystem {
    fn canonicalize<'a>(
        &'a self,
        path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        Box::pin(async move {
            let path = self.check(path)?;
            let canonicalized = path.canonicalize()?;
            Ok(PathUri::from_abs_path(&canonicalized))
        })
    }

    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        _options: ReadFileOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let path = self.check(path)?;
            tokio::fs::read(path.as_path()).await
        })
    }

    fn read_file_stream<'a>(
        &'a self,
        _path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "test filesystem does not support streaming reads",
            ))
        })
    }

    fn write_file<'a>(
        &'a self,
        _path: &'a PathUri,
        _contents: Vec<u8>,
        _options: WriteFileOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn create_directory<'a>(
        &'a self,
        _path: &'a PathUri,
        _create_directory_options: CreateDirectoryOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn get_metadata<'a>(
        &'a self,
        path: &'a PathUri,
        _options: GetMetadataOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        Box::pin(async move {
            let path = self.check(path)?;
            let metadata = tokio::fs::symlink_metadata(path.as_path()).await?;
            Ok(FileMetadata {
                is_directory: metadata.is_dir(),
                is_file: metadata.is_file(),
                is_symlink: metadata.file_type().is_symlink(),
                size: metadata.len(),
                created_at_ms: 0,
                modified_at_ms: 0,
            })
        })
    }

    fn read_directory<'a>(
        &'a self,
        _path: &'a PathUri,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn walk<'a>(
        &'a self,
        _path: &'a PathUri,
        _options: WalkOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, WalkOutcome> {
        unimplemented!()
    }

    fn remove<'a>(
        &'a self,
        _path: &'a PathUri,
        _remove_options: RemoveOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }

    fn copy<'a>(
        &'a self,
        _source_path: &'a PathUri,
        _destination_path: &'a PathUri,
        _copy_options: CopyOptions,
        _sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { unimplemented!("test filesystem only supports reads") })
    }
}

struct NoMcp;
impl HookMcpExecutor for NoMcp {
    fn execute(&self, _: HookMcpCall) -> futures::future::BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async { anyhow::bail!("no MCP in fixture") })
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let root = Path::new(&args[1]);
    let home = Path::new(&args[2]);
    let cwd = AbsolutePathBuf::try_from(PathBuf::from(&args[3]))?;
    let options = ConfigLoadOptions {
        loader_overrides: LoaderOverrides {
            system_config_path: Some(root.join("system/config.toml")),
            managed_config_path: Some(root.join("managed/managed_config.toml")),
            system_requirements_path: Some(root.join("system/requirements.toml")),
            ..LoaderOverrides::without_managed_config_for_tests()
        },
        ..Default::default()
    };
    let mut overrides = vec![(
        "projects".into(),
        toml::Value::try_from(
            json!({cwd.as_path().display().to_string(): {"trust_level":"untrusted"}}),
        )?,
    )];
    if let Ok(args) = std::env::var("FIXTURE_NATIVE_ARGS") {
        let args: Vec<String> = serde_json::from_str(&args)?;
        for pair in args.windows(2).filter(|p| p[0] == "-c") {
            let (key, raw) = pair[1]
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("invalid fixture override"))?;
            let value: toml::Value = toml::from_str(&format!("value={raw}"))?;
            overrides.push((key.into(), value["value"].clone()));
        }
    }
    let stack = codex_config::loader::load_config_layers_state(
        &TestFileSystem { root: root.into() },
        home,
        Some(cwd.clone()),
        &overrides,
        options,
        &NoopThreadConfigLoader,
    )
    .await?;
    // The same public typed conversion used by native ConfigManager::read.
    // This exercises emitted feature representation, not app-server transport.
    let mut typed: codex_config::config_toml::ConfigToml = stack.effective_config().try_into()?;
    stack.requirements_toml().apply_exact_to_config(&mut typed);
    let typed_json = serde_json::to_value(&typed)?;
    let effective_features = typed_json["features"].clone();
    let effective_owned_settings = json!({
        "model_instructions_file":typed_json["model_instructions_file"],
        "model_catalog_json":typed_json["model_catalog_json"],
        "sqlite_home":typed_json["sqlite_home"],
        "cli_auth_credentials_store":typed_json["cli_auth_credentials_store"],
    });
    // A library-only alternative, NOT the production app-server preflight.
    // Return before hook construction/execution and without native Core, auth,
    // cloud transport, or SQLite runtime startup. Fixture policy sources only.
    if args[4] == "config-only" {
        println!("{}", json!({
            "effective_owned_settings": effective_owned_settings,
            "effective_features": effective_features,
            "requirements": {
                "allow_managed_hooks_only": stack.requirements_toml().allow_managed_hooks_only,
                "features": stack.requirements_toml().feature_requirements.as_ref().map(|f| &f.entries),
            },
        }));
        return Ok(());
    }
    let config = HooksConfig {
        feature_enabled: args[4] != "off",
        config_layer_stack: Some(stack.clone()),
        shell_program: Some("/bin/sh".into()),
        shell_args: vec!["-c".into()],
        ..Default::default()
    };
    let entries = list_hooks(config.clone());
    let list: Vec<_> = entries.hooks.iter().map(|h| json!({"key":h.key,"hash":h.current_hash,"trust":format!("{:?}",h.trust_status),"source":format!("{:?}",h.source),"event":format!("{:?}",h.event_name)})).collect();
    let id = codex_protocol::ThreadId::from_string(&std::env::var("FIXTURE_SESSION")?)?;
    let (hooks, _) = Hooks::new(config, id, Arc::new(NoMcp))?;
    let start = hooks
        .run_session_start(
            SessionStartRequest {
                session_id: id,
                cwd: cwd.clone(),
                transcript_path: None,
                model: "fixture-no-model".into(),
                permission_mode: "bypassPermissions".into(),
                target: StartHookTarget::SessionStart {
                    source: SessionStartSource::Startup,
                },
            },
            Some("fixture-turn".into()),
        )
        .await;
    let submit = hooks
        .run_user_prompt_submit(UserPromptSubmitRequest {
            session_id: id,
            cwd,
            transcript_path: None,
            model: "fixture-no-model".into(),
            permission_mode: "bypassPermissions".into(),
            turn_id: "fixture-next-turn".into(),
            subagent: None,
            prompt: "PRIVATE FIXTURE PROMPT MUST NOT LEAK".into(),
        })
        .await;
    println!(
        "{}",
        json!({"effective_owned_settings":effective_owned_settings,"effective_features":effective_features,"list":list,"warnings":entries.warnings.len(), "start_runs":start.hook_events.len(),"start_stop":start.should_stop,"start_contexts":start.additional_contexts.len(),"submit_runs":submit.hook_events.len(),"submit_stop":submit.should_stop,"submit_contexts":submit.additional_contexts.len(),"managed_only":stack.requirements_toml().allow_managed_hooks_only})
    );
    Ok(())
}
