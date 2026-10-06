//! Codex identity for the shared native effect gate. Process-group custody and
//! recovery run inside the SDK launch lifecycle.

use agent_provider_execution::process::{locate_provider_executable, EffectGate, GatedCommand};
use std::ffi::OsStr;
use std::io;

pub const NATIVE_EFFECT_GATE_ARG: &str = "__native_effect_gate";
pub const NATIVE_EFFECT_GATE_FD_ENV: &str = "AGENT_RUNNER_CODEX_NATIVE_EFFECT_GATE_FD";
const PROVIDER_BINARY: &str = "agent-runner-codex";

pub(crate) fn gated_command<I, S>(program: impl AsRef<OsStr>, args: I) -> io::Result<GatedCommand>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let executable = locate_provider_executable(PROVIDER_BINARY)?;
    GatedCommand::new(
        &EffectGate {
            executable: &executable,
            argument: NATIVE_EFFECT_GATE_ARG,
            descriptor_env: NATIVE_EFFECT_GATE_FD_ENV,
        },
        program,
        args,
    )
}

pub fn run_native_effect_gate(args: &[String]) -> i32 {
    agent_provider_execution::process::run_effect_gate(args, NATIVE_EFFECT_GATE_FD_ENV)
}
