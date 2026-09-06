//! Declared roles: orchestration, formatter

#[cfg(unix)]
mod cli_output;

use std::io::Read;

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) == Some(agent_runner_codex::NATIVE_EFFECT_GATE_ARG) {
        std::process::exit(agent_runner_codex::run_native_effect_gate(&args));
    }
    if args.get(1).map(String::as_str) == Some("interactive") {
        std::process::exit(agent_runner_codex::interactive::run(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some("--version") {
        std::process::exit(agent_runner_codex::write_invocation(
            &args,
            &[],
            &mut std::io::stdout(),
        ));
    }
    let stdin = read_stdin_or_exit();
    let exit_code = write_request(&args, &stdin);
    std::process::exit(exit_code);
}

fn write_request(args: &[String], stdin: &[u8]) -> i32 {
    #[cfg(unix)]
    if args.get(1).map(String::as_str) == Some("launch") {
        let mut output = match cli_output::LaunchStdout::new() {
            Ok(output) => output,
            Err(_) => return 1,
        };
        // The invocation owns its output descriptor until custody returns.
        return agent_runner_codex::write_invocation(args, stdin, &mut output);
    }
    agent_runner_codex::write_invocation(args, stdin, &mut std::io::stdout())
}

fn read_stdin_or_exit() -> Vec<u8> {
    let mut stdin = Vec::new();
    if let Err(err) = std::io::stdin()
        .take(agent_runner_codex::envelope::MAX_REQUEST_ENVELOPE_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut stdin)
    {
        exit_stdin_read_failure(&stdin_read_failure_message(&err));
    }
    stdin
}

fn stdin_read_failure_message(err: &std::io::Error) -> String {
    format!("failed to read stdin: {err}")
}

fn exit_stdin_read_failure(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
