//! Codex native facts for the SDK's bounded session-page engine.
//!
//! Projection constants and byte-offset IDs are preserved from the prior
//! provider implementation, provisionally unqualified: they are not native
//! format truth or stable logical-identity evidence.
use crate::envelope::{ProviderFailure, RequestEnvelope};
use agent_provider_contract::generated::{ErrorCategory, ErrorObject, SessionTurnProjection};
use agent_provider_execution::session_pages::{
    self, NativeFact, NativeRecord, NativeTurn, PageAdapter, PageFact, SnapshotFacts, SourceFailure,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

struct CodexPages;

impl PageAdapter for CodexPages {
    fn namespace(&self) -> &str {
        // Separate tokens and state from the retired private engine. No legacy
        // cursor decoding, migration, or cleanup is selected.
        "codex-shared"
    }

    fn default_data_root(&self, request: &RequestEnvelope) -> Result<PathBuf, ErrorObject> {
        crate::account::user_home(&request.host)
            .map(|home| home.join(".local/share/agent-runner-codex"))
            .map_err(native_error)
    }

    fn account(
        &self,
        request: &RequestEnvelope,
        settings_id: &str,
    ) -> Result<PathBuf, ErrorObject> {
        crate::session::account_home(request, settings_id).map_err(native_error)
    }

    fn validate_session_id(&self, id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 256
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }

    fn source_candidates(
        &self,
        account: &Path,
        id: &str,
        bound: bool,
        request: &RequestEnvelope,
    ) -> Result<Vec<PathBuf>, ErrorObject> {
        let paths = crate::session::rollout_paths(account, request).map_err(native_error)?;
        let suffix = format!("-{id}.jsonl");
        let named: Vec<_> = paths
            .iter()
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(&suffix))
            })
            .cloned()
            .collect();
        // The engine selects a bound device/inode before opening any header.
        // Otherwise use native filenames, with nonstandard/fork-name fallback.
        Ok(if bound || named.is_empty() {
            paths
        } else {
            named
        })
    }

    fn first_record_session(
        &self,
        bytes: &[u8],
        request: &RequestEnvelope,
    ) -> Result<String, ErrorObject> {
        crate::session::page_session_id(bytes, request).map_err(native_error)
    }

    fn source_error(&self, failure: SourceFailure, request: &RequestEnvelope) -> ErrorObject {
        native_error(match failure {
            SourceFailure::NotFound => crate::session::not_found(request),
            SourceFailure::Io => crate::session::io_failure(request),
            SourceFailure::Ambiguous => ProviderFailure::conflict(
                &request.request_id,
                "codex_session_ambiguous",
                "More than one native rollout claims this session",
                json!({}),
            ),
        })
    }

    fn project(
        &self,
        record: &NativeRecord<'_>,
        request: &RequestEnvelope,
    ) -> Result<Option<NativeTurn>, ErrorObject> {
        let value: Value = serde_json::from_slice(record.bytes)
            .map_err(|_| native_error(crate::session::corrupt(request)))?;
        let payload = &value["payload"];
        if value["type"] != "response_item" || payload["type"] != "message" {
            return Ok(None);
        }
        let role = payload["role"].as_str().unwrap_or("");
        let observation = *record.projection == SessionTurnProjection::UserObservation;
        if !matches!(role, "user" | "assistant")
            || (observation && (role != "user" || !observable_user_text(payload)))
        {
            return Ok(None);
        }
        let timestamp = value["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .ok_or_else(|| {
                native_error(ProviderFailure::invalid_request(
                    &request.request_id,
                    "invalid_session_read_turns_params",
                    "Rollout message timestamp is invalid",
                ))
            })?
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::AutoSi, true);
        let text = crate::session::content_chunks(&payload["content"])
            .iter()
            .filter_map(|chunk| chunk["text"].as_str().map(str::to_owned))
            .collect();
        Ok(Some(NativeTurn {
            turn_id: format!("{}:byte:{}", record.session_id, record.start),
            timestamp,
            role: role.into(),
            text,
            // Existing projection claims only; parallel native-fact inquiry
            // and ROOT disposition are required before reliance.
            parent_turn_id: NativeFact::Known(None),
            is_sidechain: NativeFact::Known(false),
            is_compaction_boundary: NativeFact::Known(false),
        }))
    }

    fn source_final(&self, _facts: &SnapshotFacts<'_>) -> NativeFact<bool> {
        NativeFact::Known(false)
    }

    fn unavailable_fact(&self, _fact: PageFact, request: &RequestEnvelope) -> ErrorObject {
        native_error(ProviderFailure::unsupported(
            &request.request_id,
            "codex_session_fact_unavailable",
            "A required Codex session-page fact is unavailable",
        ))
    }
}

// Observation binds the entire textual input, not a lossy text extraction from
// a multimodal item. Missing native classifications remain usable under the
// trusted-writer premise; present classifications must agree with the content.
// These exclusions are not proof of an exclusive user-input entry point.
fn observable_user_text(payload: &Value) -> bool {
    let Some(content) = payload["content"].as_array() else {
        return false;
    };
    if content.is_empty()
        || !content.iter().all(|part| {
            matches!(part["type"].as_str(), Some("input_text" | "output_text"))
                && part["text"].is_string()
        })
    {
        return false;
    }
    let metadata = &payload["internal_chat_message_metadata_passthrough"];
    if metadata.is_null() {
        return true;
    }
    if !metadata.is_object() {
        return false;
    }
    match metadata.get("content_item_kinds") {
        None | Some(Value::Null) => true,
        Some(Value::Array(kinds)) => {
            kinds.len() == content.len() && kinds.iter().all(|kind| kind == "user.text")
        }
        Some(_) => false,
    }
}

// Native errors contain fixed provider messages, never parsed transcript text.
fn native_error(failure: ProviderFailure) -> ErrorObject {
    let category = match failure.category {
        "unsupported" => ErrorCategory::Unsupported,
        "invalid_request" => ErrorCategory::InvalidRequest,
        "invalid_settings" => ErrorCategory::InvalidSettings,
        "conflict" => ErrorCategory::Conflict,
        _ => ErrorCategory::Failed,
    };
    ErrorObject {
        code: failure.code,
        category,
        message: failure.message,
        retryable: failure.retryable,
        details: failure.details.as_object().map(|details| {
            details
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }),
        diagnostics: Vec::new(),
    }
}

fn page_failure(request: &RequestEnvelope, error: ErrorObject) -> ProviderFailure {
    let (category, exit_code) = match error.category {
        ErrorCategory::Unsupported => ("unsupported", 3),
        ErrorCategory::InvalidRequest => ("invalid_request", 2),
        ErrorCategory::InvalidSettings => ("invalid_settings", 2),
        ErrorCategory::Unavailable => ("unavailable", 1),
        ErrorCategory::Timeout => ("timeout", 1),
        ErrorCategory::Conflict => ("conflict", 2),
        ErrorCategory::Failed => ("failed", 1),
    };
    ProviderFailure {
        request_id: request.request_id.clone(),
        category,
        code: error.code,
        message: error.message,
        details: error
            .details
            .map(|details| Value::Object(details.into_iter().collect()))
            .unwrap_or_else(|| json!({})),
        retryable: error.retryable,
        exit_code,
    }
}

pub(crate) fn read_turns(request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    let result = session_pages::read_turns(&CodexPages, request)
        .map_err(|error| page_failure(request, error))?;
    serde_json::to_value(result).map_err(|_| {
        ProviderFailure::internal(
            &request.request_id,
            "session_turn_page_io",
            "Could not encode bounded session page",
        )
    })
}
