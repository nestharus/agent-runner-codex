//! Declared roles: orchestration, mapper, parser, formatter, validator

use crate::encoding::{bounded_text, decode_base64};
use crate::envelope::ProviderFailure;
use serde::Deserialize;
use serde_json::{json, Value};

const TERMINAL_SIGNAL_EVIDENCE_MAX_LEN: usize = 160;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcessStatus {
    Exited { code: i32 },
    SignalTerminated { signal: i32 },
    SpawnError { reason: String },
    ProlongedSilence { reason: String },
    Cancelled,
    Unknown,
}

pub fn classify_params(params: Value, request_id: &str) -> Result<Value, ProviderFailure> {
    let params = parse_classify_params(params, request_id)?;
    let _stdout = decode_stream(&params.stdout_base64, request_id, "stdout_base64")?;
    let stderr = decode_stream(&params.stderr_base64, request_id, "stderr_base64")?;
    // Launch stdout contains assistant prose, including arbitrary JSON. Only
    // native error events are forwarded to stderr by the exec adapter.
    let mut failure = None;
    for line in stderr.split(|byte| *byte == b'\n') {
        if let Ok(event) = serde_json::from_slice::<Value>(line) {
            if is_native_error(&event) {
                failure = NativeFailure::from_event(&event);
            }
        }
    }
    let signal = classify_with_failure(&params.status, params.observed_at_unix_ms, failure);
    Ok(classify_result(signal))
}

pub fn classify(status: &ProcessStatus, observed_at_unix_ms: u64) -> Value {
    classify_with_failure(status, observed_at_unix_ms, None)
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeFailure {
    Quota,
    RateLimit,
}

pub(crate) fn is_native_error(event: &Value) -> bool {
    matches!(event["type"].as_str(), Some("turn.failed" | "error"))
}

impl NativeFailure {
    pub(crate) fn from_event(event: &Value) -> Option<Self> {
        // Pinned upstream definitions: rust-v0.153.4 exec/src/exec_events.rs
        // ThreadErrorEvent and protocol/src/error.rs CodexErrorDetails/Display.
        // These events carry a message, not an API error code. Do not search
        // arbitrary text, nested tool errors, or server response bodies.
        let message = match event["type"].as_str()? {
            "turn.failed" => event.pointer("/error/message")?.as_str()?,
            "error" => event.get("message")?.as_str()?,
            _ => return None,
        };
        if message.starts_with("You've hit your usage limit.")
            || message.starts_with("You've hit your usage limit for ")
            || message == "Quota exceeded. Check your plan and billing details."
            || message.starts_with("Your workspace is out of credits.")
            || message.starts_with("You hit your spend cap set in your workspace.")
            || message.starts_with("You hit your spend cap set by the owner of your workspace.")
        {
            return Some(Self::Quota);
        }
        if message.starts_with("rate limit exceeded: ")
            || message.starts_with("unexpected status 429 Too Many Requests:")
            || message.starts_with("exceeded retry limit, last status: 429 Too Many Requests")
        {
            return Some(Self::RateLimit);
        }
        None
    }
}

pub(crate) fn classify_with_failure(
    status: &ProcessStatus,
    observed_at_unix_ms: u64,
    failure: Option<NativeFailure>,
) -> Value {
    // Explicit process outcomes take precedence. A recovered request that exits
    // successfully must not poison an account because an earlier retry failed.
    if matches!(status, ProcessStatus::Exited { code } if *code != 0)
        || matches!(status, ProcessStatus::Unknown)
    {
        if let Some(failure) = failure {
            let (kind, evidence) = match failure {
                NativeFailure::Quota => {
                    ("quota_exhausted_inband", "codex.exec: usage limit reached")
                }
                NativeFailure::RateLimit => ("rate_limited", "codex.exec: rate limit exceeded"),
            };
            // Fixed evidence avoids copying request IDs, URLs or credentials
            // from native diagnostics into terminal classification records.
            return terminal_signal(kind, evidence.into(), observed_at_unix_ms);
        }
    }
    terminal_signal(
        signal_kind(status),
        signal_evidence(status),
        observed_at_unix_ms,
    )
}

pub fn process_status_json(status: &ProcessStatus) -> Value {
    match status {
        ProcessStatus::Exited { code } => json!({ "kind": "exited", "code": code }),
        ProcessStatus::SignalTerminated { signal } => {
            json!({ "kind": "signal_terminated", "signal": signal })
        }
        ProcessStatus::SpawnError { reason } => {
            json!({ "kind": "spawn_error", "reason": reason })
        }
        ProcessStatus::ProlongedSilence { reason } => {
            json!({ "kind": "prolonged_silence", "reason": reason })
        }
        ProcessStatus::Cancelled => json!({ "kind": "cancelled" }),
        ProcessStatus::Unknown => json!({ "kind": "unknown" }),
    }
}

pub fn exit_code_for_status(status: &ProcessStatus) -> i32 {
    match status {
        ProcessStatus::Exited { code } => *code,
        ProcessStatus::SignalTerminated { signal } => 128 + *signal,
        ProcessStatus::ProlongedSilence { .. } => 124,
        ProcessStatus::Cancelled => 130,
        ProcessStatus::SpawnError { .. } | ProcessStatus::Unknown => 1,
    }
}

#[derive(Deserialize)]
struct TerminalClassifyParams {
    stdout_base64: String,
    stderr_base64: String,
    status: ProcessStatus,
    observed_at_unix_ms: u64,
}

fn parse_classify_params(
    params: Value,
    request_id: &str,
) -> Result<TerminalClassifyParams, ProviderFailure> {
    serde_json::from_value(params).map_err(|err| invalid_terminal_params_failure(request_id, err))
}

fn decode_stream(
    value: &str,
    request_id: &str,
    field: &'static str,
) -> Result<Vec<u8>, ProviderFailure> {
    decode_base64(value).map_err(|err| invalid_base64_failure(request_id, field, err))
}

fn signal_kind(status: &ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Exited { code: 0 } => "clean_exit",
        ProcessStatus::Exited { .. } => "nonzero_exit",
        ProcessStatus::SignalTerminated { .. } => "signal_exit",
        ProcessStatus::SpawnError { .. } => "spawn_error",
        ProcessStatus::ProlongedSilence { .. } => "prolonged_silence",
        ProcessStatus::Cancelled => "cancelled",
        ProcessStatus::Unknown => "unknown",
    }
}

fn signal_evidence(status: &ProcessStatus) -> String {
    match status {
        ProcessStatus::Exited { code } => format!("exit_code={code}"),
        ProcessStatus::SignalTerminated { signal } => format!("signal={signal}"),
        ProcessStatus::SpawnError { reason } | ProcessStatus::ProlongedSilence { reason } => {
            bounded_text(reason, TERMINAL_SIGNAL_EVIDENCE_MAX_LEN)
        }
        ProcessStatus::Cancelled => "cancelled".to_string(),
        ProcessStatus::Unknown => "unknown".to_string(),
    }
}

fn terminal_signal(kind: &str, evidence: String, observed_at_unix_ms: u64) -> Value {
    json!({
        "kind": kind,
        "evidence": evidence,
        "observed_at_unix_ms": observed_at_unix_ms,
    })
}

fn classify_result(signal: Value) -> Value {
    json!({ "terminal_signal": signal })
}

fn invalid_terminal_params_failure(request_id: &str, err: serde_json::Error) -> ProviderFailure {
    ProviderFailure::invalid_request(
        request_id,
        "invalid_terminal_params",
        format!("terminal.classify params are invalid: {err}"),
    )
}

fn invalid_base64_failure(request_id: &str, field: &'static str, err: String) -> ProviderFailure {
    ProviderFailure::invalid_request(
        request_id,
        "invalid_base64",
        format!("{field} is not valid base64: {err}"),
    )
}
