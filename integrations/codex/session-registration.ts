// Provider-owned metadata registration; deliberately independent of the Bash tool.
import { createConnection } from "node:net"

const nativeId = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/
const failureMessage = "Codex session registration failed: no exact authorized acknowledgement. Exit and relaunch with Agent Runner; check runner binding diagnostics and selected account metadata/cwd/resume identity. Integration validation does not establish effective native policy: ask its administrator about hook exclusions or redirected settings; do not bypass trust or adopt another store."
const failure = () => new Error(failureMessage)

export async function registerSession(id: string, cwd: string, timeoutMs = 12000): Promise<void> {
  if (!nativeId.test(id) || cwd !== process.env.AGENT_RUNNER_CODEX_REGISTRATION_CWD) throw failure()
  const resumed = process.env.AGENT_RUNNER_CODEX_SESSION_ID
  if (resumed && resumed !== id) throw failure()
  let invocation: string
  try { invocation = JSON.parse(process.env.OULIPOLY_PARENT_INVOCATION || "").id } catch { throw failure() }
  const token = process.env.OULIPOLY_LIVE_SESSION_BIND_TOKEN
  const path = process.env.OULIPOLY_LIVE_SESSION_BIND_SOCKET
  if (!invocation || !nativeId.test(invocation) || !token || !path) throw failure()
  const deadline = Date.now() + timeoutMs
  do {
    // Every retry repeats the identical authenticated report and provider capture.
    // No missing/refused socket is interpreted as an earlier acknowledgement.
    if (await exchange(path, { schema_version: 1, token, invocation_uuid: invocation, provider_session_id: id }, deadline)) return
    if (Date.now() >= deadline) break
    await Bun.sleep(Math.min(100, deadline - Date.now()))
  } while (Date.now() < deadline)
  throw failure()
}

function exchange(path: string, report: { invocation_uuid: string; provider_session_id: string; [key: string]: unknown }, deadline: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = createConnection({ path })
    let bytes = Buffer.alloc(0)
    let done = false
    const finish = (ok: boolean) => { if (done) return; done = true; clearTimeout(timer); socket.destroy(); resolve(ok) }
    const timer = setTimeout(() => finish(false), Math.max(1, deadline - Date.now()))
    socket.on("connect", () => socket.write(JSON.stringify(report) + "\n"))
    socket.on("data", (chunk) => {
      bytes = Buffer.concat([bytes, chunk])
      if (bytes.length > 16384) return finish(false)
      const end = bytes.indexOf(10)
      if (end < 0) return
      try {
        const ack = JSON.parse(bytes.subarray(0, end).toString("utf8"))
        finish(ack.ok === true && ack.session_id === report.provider_session_id &&
          ack.provider_session_id === report.provider_session_id && ack.agent_runner_invocation_id === report.invocation_uuid)
      } catch { finish(false) }
    })
    socket.on("error", () => finish(false))
    socket.on("end", () => finish(false))
  })
}

async function hook(): Promise<void> {
  // UserPromptSubmit includes prompt text. Bound input, extract identity only,
  // never persist/log the payload or use the supplied transcript path as authority.
  let bytes = Buffer.alloc(0)
  for await (const chunk of process.stdin) {
    bytes = Buffer.concat([bytes, Buffer.from(chunk)])
    if (bytes.length > 4 * 1024 * 1024) throw failure()
  }
  const input = JSON.parse(bytes.toString("utf8"))
  if (!["SessionStart", "UserPromptSubmit"].includes(input.hook_event_name) || typeof input.session_id !== "string" || typeof input.cwd !== "string") throw failure()
  await registerSession(input.session_id, input.cwd)
}

if (import.meta.main) {
  try { await hook() } catch {
    // Native only honors this structured stop on successful command completion.
    // Crashes/timeouts remain native advisory failures, not guaranteed stops.
    process.stdout.write(JSON.stringify({ continue: false, stopReason: failureMessage }) + "\n")
  }
}
