//! Versioned external-provider transport copied from the OpenCode contract.
use crate::{
    discovery,
    envelope::{self, ProviderFailure, RequestEnvelope, CONTRACT},
    launch, policy, session, terminal,
};
use serde_json::{json, Value};
use std::io::Write;

pub fn describe(host: &crate::envelope::HostContext) -> Value {
    let mut result = json!({"provider_id":"codex", "display_name":"Codex", "contract_versions":[CONTRACT], "preferred_contract":CONTRACT,
        "capabilities":{"launch":true,"policy":true,"quota":true,"session":true,"session_enumerate":true,
            "terminal":true,"rotation":false,"discovery":true,"settings":false,"setup_brain":false,"setup":true,"migration":false},
        "concurrency":{"safe_for_parallel_invocation":true,"stdout_protocol_only":true,
            "notes":"One codex exec process per invocation; account-isolated native sessions; durable request replay and per-session locks."}});
    if launch::host_requested_output(host) {
        result["capabilities"]["launch_output_v1"] = json!(true);
    }
    if host
        .env
        .as_ref()
        .and_then(|env| env.get("OULIPOLY_HOST_SESSION_TURN_PAGES_V1"))
        .map(String::as_str)
        == Some("1")
    {
        result["capabilities"]["session_turn_pages_v1"] = json!(true);
    }
    result
}

pub fn write_invocation<W: Write>(args: &[String], input: &[u8], writer: &mut W) -> i32 {
    if args.get(1).is_some_and(|v| v == "--version") {
        let _ = writeln!(writer, "agent-runner-codex {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let mut request_id = String::new();
    let result = (|| {
        if input.len() > envelope::MAX_REQUEST_ENVELOPE_BYTES {
            return Err(ProviderFailure::invalid_request(
                "",
                "request_too_large",
                "Request exceeds 4 MiB",
            ));
        }
        let request: RequestEnvelope = serde_json::from_slice(input).map_err(|_| {
            ProviderFailure::invalid_request(
                "",
                "invalid_request",
                "Expected a provider request JSON envelope",
            )
        })?;
        request_id.clone_from(&request.request_id);
        if request.contract != CONTRACT
            || request.request_id.is_empty()
            || request.request_id.len() > envelope::MAX_REQUEST_ID_BYTES
        {
            return Err(ProviderFailure::invalid_request(
                "",
                "invalid_envelope",
                "Contract or request ID is invalid",
            ));
        }
        let operation = args.get(1).map(String::as_str).unwrap_or("");
        if operation == "launch" {
            return launch::run(&request, writer);
        }
        let result = match operation {
            "describe" => describe(&request.host),
            "discovery.models" => discovery::models(),
            "discovery.accounts" => discovery::accounts(),
            "policy.evaluate" => policy::evaluate(&request)?,
            op if op.starts_with("quota.") => crate::quota::handle(op, &request)?,
            "terminal.classify" => terminal::classify_params(request.params.clone(), &request_id)?,
            op if op.starts_with("session.") => session::handle(op, &request)?,
            "setup.detect" => {
                let validation =
                    policy::RuntimeConfig::load(&request.host).and_then(|config| config.validate());
                json!({"installed":validation.is_ok(), "profiles":[], "warnings": validation.err().map(|e| json!({"severity":"error","code":e.code,"message":e.message})).into_iter().collect::<Vec<_>>()})
            }
            "setup.install_plan" => json!({"steps":[
                {"kind":"manual","description":"Build this repository with cargo build --release, then run python3 scripts/install-provider.py."},
                {"kind":"manual","description":"Stage and activate the Codex labels with scripts/install-labels.py after validating the provider."}
            ]}),
            "setup.sync_plan" => {
                let validation =
                    policy::RuntimeConfig::load(&request.host).and_then(|config| config.validate());
                json!({"operations":[],"diagnostics":validation.err().map(|e| json!({"severity":"error","code":e.code,"message":e.message})).into_iter().collect::<Vec<_>>()})
            }
            _ => {
                return Err(ProviderFailure::unsupported(
                    "",
                    "unsupported_operation",
                    format!("Codex temporary provider does not implement {operation}"),
                ))
            }
        };
        serde_json::to_writer(
            &mut *writer,
            &envelope::success_response(&request_id, result),
        )
        .map_err(|e| ProviderFailure::internal("", "output_failed", e.to_string()))?;
        writer
            .write_all(b"\n")
            .map_err(|e| ProviderFailure::internal("", "output_failed", e.to_string()))?;
        Ok(0)
    })();
    match result {
        Ok(code) => code,
        Err(mut error) => {
            error.request_id = request_id;
            let _ = serde_json::to_writer(&mut *writer, &envelope::failure_response(&error));
            let _ = writer.write_all(b"\n");
            error.exit_code
        }
    }
}
