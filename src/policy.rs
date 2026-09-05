//! Codex owns model, prompt, tool, and account selection before native execution.
use crate::envelope::{HostContext, ProviderFailure, RequestEnvelope};
use crate::{account, models};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub codex_bin: PathBuf,
    pub bun_bin: PathBuf,
    pub bash_mcp_path: PathBuf,
    pub system_prompt_file: PathBuf,
    pub agent_bash_bin: PathBuf,
    pub agent_runner_bin: PathBuf,
}

impl RuntimeConfig {
    pub fn load(host: &HostContext) -> Result<Self, ProviderFailure> {
        let user = account::user_home(host)?;
        let root = host
            .config_root
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| user.join(".config/oulipoly-agent-runner"));
        let install = root.join("agent-runner-codex");
        let path = install.join("config.toml");
        let config = if path.is_file() {
            let bytes = crate::durable_fs::read_file_bounded(&path, 64 * 1024).map_err(|_| {
                invalid(
                    "runtime_config_unreadable",
                    "Cannot read bounded Codex provider configuration",
                )
            })?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                invalid(
                    "runtime_config_invalid",
                    "Codex provider configuration must be UTF-8",
                )
            })?;
            toml::from_str(text).map_err(|_| {
                invalid(
                    "runtime_config_invalid",
                    "Invalid Codex provider configuration",
                )
            })?
        } else {
            Self {
                codex_bin: user.join(".npm-global/bin/codex"),
                bun_bin: user.join(".bun/bin/bun"),
                bash_mcp_path: install.join("integrations/codex/agent-bash-mcp.ts"),
                system_prompt_file: user.join("ai/AGENTS.md"),
                agent_bash_bin: root.join("agent-bash/agent-bash"),
                agent_runner_bin: root.join("runner/oulipoly-agent-runner"),
            }
        };
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ProviderFailure> {
        for (name, path) in [
            ("codex_bin", &self.codex_bin),
            ("bun_bin", &self.bun_bin),
            ("bash_mcp_path", &self.bash_mcp_path),
            ("system_prompt_file", &self.system_prompt_file),
            ("agent_bash_bin", &self.agent_bash_bin),
            ("agent_runner_bin", &self.agent_runner_bin),
        ] {
            if !path.is_absolute() || !path.is_file() {
                return Err(invalid(
                    "runtime_dependency_missing",
                    &format!(
                        "{name} must name an existing absolute file: {}",
                        path.display()
                    ),
                ));
            }
        }
        let catalog = self.bash_mcp_path.parent().unwrap().join("models.json");
        let catalog_bytes = crate::durable_fs::read_file_bounded(&catalog, 4 * 1024 * 1024)
            .map_err(|_| {
                invalid(
                    "model_catalog_unreadable",
                    "Cannot read the required managed Codex model catalog",
                )
            })?;
        let catalog_json: Value = serde_json::from_slice(&catalog_bytes).map_err(|_| {
            invalid(
                "model_catalog_invalid",
                "The managed Codex model catalog must be valid JSON",
            )
        })?;
        let entries = catalog_json
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                invalid(
                    "model_catalog_invalid",
                    "The managed Codex model catalog must contain models",
                )
            })?;
        for slug in ["gpt-6-astra", "gpt-5.6-luna"] {
            let matching: Vec<_> = entries
                .iter()
                .filter(|entry| entry["slug"] == slug)
                .collect();
            if matching.len() != 1 {
                return Err(invalid(
                    "model_catalog_invalid",
                    "The managed Codex model catalog must uniquely define every supported model",
                ));
            }
            let entry = matching[0];
            if ["apply_patch_tool_type", "tool_mode", "multi_agent_version"]
                .iter()
                .any(|key| entry.get(*key) != Some(&Value::Null))
                || entry.get("node_repl_disabled") != Some(&Value::Bool(true))
                || entry.get("supports_search_tool") != Some(&Value::Bool(false))
                || !entry
                    .get("experimental_supported_tools")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
            {
                return Err(invalid("model_catalog_tools_unrestricted", "The managed Codex model catalog must disable native tool overrides for every supported model"));
            }
        }
        let prompt = crate::durable_fs::read_file_bounded(&self.system_prompt_file, 1024 * 1024)
            .map_err(|_| {
                invalid(
                    "system_prompt_unreadable",
                    "Cannot read the bounded configured system prompt",
                )
            })?;
        if prompt.is_empty() || prompt.len() > 1024 * 1024 || std::str::from_utf8(&prompt).is_err()
        {
            return Err(invalid(
                "system_prompt_invalid",
                "System prompt must be nonempty UTF-8, at most 1 MiB",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub settings_id: String,
    pub model: String,
    pub effort: String,
    pub prompt: String,
    pub env: BTreeMap<String, String>,
    pub argv: Vec<String>,
}

pub fn plan(request: &RequestEnvelope, is_policy: bool) -> Result<Plan, ProviderFailure> {
    let params = &request.params;
    let settings_id = params
        .get("settings_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing_settings_id", "settings_id is required"))?;
    let codex_home = account::home(&request.host, settings_id)?;
    let name = params
        .pointer("/model/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (model, effort) = models::route(name).ok_or_else(|| {
        invalid(
            "unknown_model",
            "Select a codex-gpt-* label or the separately named Codex benchmark",
        )
    })?;
    let provider_args: Vec<String> = serde_json::from_value(
        params
            .pointer("/model/provider_args")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|_| invalid("invalid_model_args", "model.provider_args must be an array"))?;
    if provider_args != models::args(model, effort) {
        return Err(invalid(
            "model_args_mismatch",
            "Model arguments do not match the selected catalog label",
        ));
    }
    if !matches!(
        params.get("mode").and_then(Value::as_str),
        Some("arg" | "stdin")
    ) {
        return Err(invalid(
            "unsupported_mode",
            "Codex provider supports noninteractive arg and stdin modes",
        ));
    }
    let launch = if is_policy { &params["launch"] } else { params };
    let prompt = params
        .pointer("/model/inputs/prompt")
        .and_then(Value::as_str)
        .or_else(|| launch.get("prompt").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    if prompt.is_empty() {
        return Err(invalid("missing_prompt", "A nonempty prompt is required"));
    }
    let supplied: Vec<String> =
        serde_json::from_value(launch.get("argv").cloned().unwrap_or(Value::Null))
            .map_err(|_| invalid("invalid_argv", "argv must be an array"))?;
    let mut canonical = vec![
        settings_id.to_string(),
        "exec".into(),
        "--dangerously-bypass-approvals-and-sandbox".into(),
    ];
    canonical.extend(provider_args);
    let mut with_prompt = canonical.clone();
    with_prompt.push(prompt.clone());
    if supplied != canonical && supplied != with_prompt {
        return Err(invalid("unmanaged_argv", "Launch arguments must match the selected Codex route; additional runtime, model, or tool overrides are not allowed"));
    }
    if let Some(restrictions) = launch.get("tool_restrictions").filter(|v| !v.is_null()) {
        if restrictions.get("kind").and_then(Value::as_str) != Some("codex")
            || restrictions.as_object().is_none_or(|m| {
                m.iter().any(|(k, v)| {
                    k != "kind"
                        && !v.as_object().is_some_and(|m| m.is_empty())
                        && !v.as_array().is_some_and(|a| a.is_empty())
                })
            })
        {
            return Err(invalid(
                "unsupported_tool_restrictions",
                "The managed Codex provider enforces its sole Agent Bash tool policy",
            ));
        }
    }
    let mut env: BTreeMap<String, String> = serde_json::from_value(
        launch
            .get("env")
            .cloned()
            .filter(|v| !v.is_null())
            .unwrap_or(json!({})),
    )
    .map_err(|_| invalid("invalid_env", "env must map names to strings"))?;
    for entries in [Some(&env), request.host.env.as_ref()]
        .into_iter()
        .flatten()
    {
        if entries
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
        {
            return Err(invalid(
                "invalid_env",
                "Environment names and values must be valid process environment entries",
            ));
        }
    }
    env.insert("CODEX_HOME".into(), codex_home.display().to_string());
    if is_policy {
        if let Some(instructions) = launch
            .get("system_prompt_override")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            env.insert(
                "AGENT_RUNNER_CODEX_DEVELOPER_INSTRUCTIONS".into(),
                instructions.into(),
            );
        }
    }
    Ok(Plan {
        settings_id: settings_id.into(),
        model: model.into(),
        effort: effort.into(),
        prompt,
        env,
        argv: canonical,
    })
}

pub fn evaluate(request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    match plan(request, true).and_then(|plan| {
        RuntimeConfig::load(&request.host)?.validate()?;
        Ok(plan)
    }) {
        Ok(plan) => Ok(json!({"accepted": true, "argv": plan.argv, "env": plan.env,
            "stdin": plan.prompt, "prompt": plan.prompt, "diagnostics": [],
            "markers": [{"name":"codex.route", "value":{"account":plan.settings_id,"model":plan.model,"effort":plan.effort}}]})),
        Err(error) => Ok(
            json!({"accepted": false, "diagnostics": [{"severity":"error", "code":error.code,"message":error.message}], "markers":[]}),
        ),
    }
}

fn invalid(code: &'static str, message: &str) -> ProviderFailure {
    ProviderFailure::invalid_settings("", code, message, json!({}))
}
