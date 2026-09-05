//! Account-pinned quota projection through the installed ChatGPT usage adapter.

use crate::{
    account,
    envelope::{ProviderFailure, RequestEnvelope},
};
use chrono::DateTime;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs::File,
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const MAX_OUTPUT_BYTES: u64 = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    settings_id: String,
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    context: Option<Value>,
}

#[derive(Deserialize)]
struct Usage {
    windows: Vec<UsageWindow>,
}

#[derive(Deserialize)]
struct UsageWindow {
    used_percent: f64,
    resets_at: String,
}

pub fn handle(operation: &str, request: &RequestEnvelope) -> Result<Value, ProviderFailure> {
    if operation == "quota.refresh_auth" {
        return Err(ProviderFailure::unsupported(&request.request_id, "auth_refresh_unsupported", "Codex refreshes its authentication during native execution; standalone refresh is unavailable"));
    }
    let params: Params = serde_json::from_value(request.params.clone()).map_err(|_| {
        ProviderFailure::invalid_request(
            &request.request_id,
            "invalid_quota_params",
            "Expected quota settings_id and optional model_name/context",
        )
    })?;
    let _ = (&params.model_name, &params.context);
    let home = account::home(&request.host, &params.settings_id)?;
    let auth = home.join("auth.json");
    let helper = account::user_home(&request.host)?.join(".local/bin/chatgpt-usage");
    match operation {
        "quota.source" => Ok(json!({
            "has_source": helper.is_file() && auth.is_file(),
            "source_id": format!("{}.native_auth", params.settings_id),
            "freshness": "live_probe",
        })),
        "quota.probe" => {
            if !helper.is_file() || !auth.is_file() {
                return Ok(unavailable("Native auth or quota adapter is unavailable"));
            }
            let windows = probe(request, &helper, &auth, &home).and_then(|bytes| project(&bytes));
            match windows {
                Ok(windows) => {
                    Ok(json!({"available":true, "checked_at_unix_ms":now_ms(), "windows":windows}))
                }
                Err(message) => Ok(unavailable(message)),
            }
        }
        _ => Err(ProviderFailure::unsupported(
            &request.request_id,
            "unsupported_quota_operation",
            "Unknown quota operation",
        )),
    }
}

fn unavailable(detail: &str) -> Value {
    json!({"available":false, "checked_at_unix_ms":now_ms(), "windows":[], "detail":detail})
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

fn probe(
    request: &RequestEnvelope,
    helper: &Path,
    auth: &Path,
    home: &Path,
) -> Result<Vec<u8>, &'static str> {
    let remaining = request
        .host
        .deadline_unix_ms
        .map(|deadline| Duration::from_millis(deadline.saturating_sub(now_ms())))
        .unwrap_or(PROBE_TIMEOUT)
        .min(PROBE_TIMEOUT);
    if remaining.is_zero() {
        return Err("Quota probe deadline expired");
    }
    let started = Instant::now();
    let mut output = tempfile::tempfile().map_err(|_| "Quota output capture unavailable")?;
    let mut command = Command::new(helper);
    command
        .arg(auth)
        .stdin(Stdio::null())
        .stdout(
            output
                .try_clone()
                .map_err(|_| "Quota output capture unavailable")?,
        )
        .stderr(Stdio::null());
    if let Some(env) = &request.host.env {
        command.envs(env);
    }
    command.env("CODEX_HOME", home);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut process = ProbeChild {
        child: command
            .spawn()
            .map_err(|_| "Quota adapter could not start")?,
        completed: false,
    };
    loop {
        if output
            .metadata()
            .map_err(|_| "Quota output capture unavailable")?
            .len()
            > MAX_OUTPUT_BYTES
        {
            return Err("Quota adapter exceeded output limit");
        }
        if let Some(status) = process
            .child
            .try_wait()
            .map_err(|_| "Quota adapter status unavailable")?
        {
            process.completed = true;
            if !status.success() {
                return Err("Quota adapter failed; authenticate the selected Codex account");
            }
            break;
        }
        if started.elapsed() >= remaining {
            return Err("Quota probe deadline expired");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    use std::io::{Seek, SeekFrom};
    output
        .seek(SeekFrom::Start(0))
        .map_err(|_| "Quota output capture unavailable")?;
    read_output(output)
}

struct ProbeChild {
    child: Child,
    completed: bool,
}
impl Drop for ProbeChild {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_output(output: File) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::new();
    output
        .take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Quota output capture unavailable")?;
    if bytes.len() as u64 > MAX_OUTPUT_BYTES {
        return Err("Quota adapter exceeded output limit");
    }
    Ok(bytes)
}

fn project(bytes: &[u8]) -> Result<Vec<Value>, &'static str> {
    let usage: Usage =
        serde_json::from_slice(bytes).map_err(|_| "Quota adapter returned invalid usage data")?;
    if usage.windows.is_empty() || usage.windows.len() > 16 {
        return Err("Quota adapter returned no usable quota windows");
    }
    usage.windows.into_iter().map(|window| {
        if !window.used_percent.is_finite() || !(0.0..=100.0).contains(&window.used_percent) { return Err("Quota adapter returned an invalid usage percentage"); }
        let reset = DateTime::parse_from_rfc3339(&window.resets_at).map_err(|_| "Quota adapter returned an invalid reset timestamp")?.timestamp_millis();
        if reset < 0 { return Err("Quota adapter returned an invalid reset timestamp"); }
        Ok(json!({"remaining_ratio":(100.0-window.used_percent)/100.0,"resets_at_unix_ms":reset as u64}))
    }).collect()
}
