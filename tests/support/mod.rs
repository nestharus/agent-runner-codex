//! Optional, offline fixture custody for external verification runs.
use std::{fs, path::Path};

pub fn preserve_fixture(root: &Path) {
    let Some(destination) = std::env::var_os("CODEX_TEST_EVIDENCE_DIR") else {
        return;
    };
    let destination = Path::new(&destination).join(root.file_name().unwrap());
    fs::create_dir_all(&destination).unwrap();
    // Only files created by the fake fixture are selected, never ambient
    // environment or native account files. Large launch journals are excluded.
    for name in [
        "config/agent-runner-codex/config.toml",
        "config/providers.toml",
        "codex",
        "native",
        "bash",
        "bun",
        "agent-bash",
        "agents",
        "mcp.ts",
        "models.json",
        "ai/AGENTS.md",
        "prompt.md",
        "calls.jsonl",
        "calls.json",
    ] {
        preserve_file(root, &destination, name);
    }
    fs::write(
        destination.join("source-root.txt"),
        root.to_string_lossy().as_bytes(),
    )
    .unwrap();
}

fn preserve_file(root: &Path, destination: &Path, name: &str) {
    let source = root.join(name);
    if !source.is_file() {
        return;
    }
    let target = destination.join(name);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(source, target).unwrap();
}
