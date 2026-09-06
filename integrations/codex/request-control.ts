// Per-disposable-launch control. No RPC input/output is persisted by this module.
import { constants, closeSync, fstatSync, lstatSync, mkdirSync, openSync, readFileSync, readSync, realpathSync, readdirSync, rmdirSync, unlinkSync, writeFileSync, chmodSync } from "node:fs"
import { dirname, resolve } from "node:path"
import { randomBytes, timingSafeEqual } from "node:crypto"
import { Database } from "bun:sqlite"
import { Channel, descendant, identity, listen, live, type Identity } from "./request-control-unix"

export const DIRECTORY_ENV = "AGENT_RUNNER_CODEX_REQUEST_CONTROL_DIR"
export const BINDING_ENV = "AGENT_RUNNER_CODEX_REQUEST_CONTROL_BINDING"
export const secret = () => randomBytes(32).toString("hex")
const reject = () => new Error("Request-control identity or capability rejected")
export type RpcId = string | number
type LaunchIdentity = { version: number; provider_pid: number; provider_incarnation: string; request_id: string; provider_instance_id: string; parent: { id: string; source: string } }
export type Binding = LaunchIdentity & { identity_database: string }
export type Descriptor = { version: number; generation: string; token: string; bridge: Identity; binding: LaunchIdentity }
export type Pending = { controller: AbortController; promise: Promise<void>; generation: string; session?: string; request?: unknown }
const hex = (value: unknown) => typeof value === "string" && /^[0-9a-f]{64}$/.test(value)
function equalSecret(a: unknown, b: string) {
  return hex(a) && timingSafeEqual(Buffer.from(a as string), Buffer.from(b))
}
export function privateDirectory(path: string) {
  const stat = lstatSync(path)
  if (!stat.isDirectory() || stat.uid !== process.getuid!() || (stat.mode & 0o777) !== 0o700
      || realpathSync(path) !== path || resolve(path) !== path) throw reject()
  return stat
}
function privateRead(path: string) {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  try {
    const stat = fstatSync(fd)
    if (!stat.isFile() || stat.uid !== process.getuid!() || (stat.mode & 0o777) !== 0o600 || stat.nlink !== 1 || stat.size > 16384) throw reject()
    const bytes = Buffer.alloc(16385)
    const size = readSync(fd, bytes, 0, bytes.length, 0)
    if (size > 16384) throw reject()
    return bytes.subarray(0, size).toString("utf8")
  } finally { closeSync(fd) }
}
function validBinding(binding: LaunchIdentity) {
  if (binding?.version !== 1 || !Number.isSafeInteger(binding.provider_pid) || binding.provider_pid < 1
      || typeof binding.provider_incarnation !== "string" || !/^linux:[0-9a-f-]{36}:\d+$/.test(binding.provider_incarnation)
      || typeof binding.request_id !== "string" || !binding.request_id || binding.request_id.length > 256
      || typeof binding.provider_instance_id !== "string" || !binding.provider_instance_id || binding.provider_instance_id.length > 256
      || typeof binding.parent?.id !== "string" || !/^[0-9a-fA-F-]{36}$/.test(binding.parent.id)
      || typeof binding.parent.source !== "string" || !binding.parent.source.trim() || binding.parent.source.length > 256) throw reject()
}
export function readDescriptor(directory: string): Descriptor {
  privateDirectory(dirname(directory))
  privateDirectory(directory)
  const descriptor = JSON.parse(privateRead(`${directory}/control.json`)) as Descriptor
  validBinding(descriptor.binding)
  if (descriptor.version !== 1 || !hex(descriptor.generation) || !hex(descriptor.token)
      || !Number.isSafeInteger(descriptor.bridge?.pid) || descriptor.bridge.pid < 1
      || !/^linux:[0-9a-f-]{36}:\d+$/.test(descriptor.bridge.incarnation)) throw reject()
  return descriptor
}
export function provider(descriptor: Descriptor): Identity {
  return { pid: descriptor.binding.provider_pid, incarnation: descriptor.binding.provider_incarnation }
}
export function socketPath(directory: string) {
  const path = `${directory}/control.sock`
  if (Buffer.byteLength(path) > 103) throw reject()
  return path
}
export function verifySocket(directory: string) {
  const stat = lstatSync(socketPath(directory))
  if (!stat.isSocket() || stat.uid !== process.getuid!() || (stat.mode & 0o777) !== 0o600) throw reject()
}
async function verifyRunner(binding: Binding) {
  validBinding(binding)
  if (typeof binding.identity_database !== "string" || !binding.identity_database.startsWith("/")) throw reject()
  const owner = { pid: binding.provider_pid, incarnation: binding.provider_incarnation }
  if (!descendant(process.pid, owner) || owner.pid === process.pid) throw reject()
  const deadline = Date.now() + 5000
  do {
    if (!live(owner)) throw reject()
    let db: Database | undefined
    try {
      const stat = lstatSync(binding.identity_database)
      if (!stat.isFile() || stat.uid !== process.getuid!() || stat.nlink !== 1
          || realpathSync(binding.identity_database) !== binding.identity_database) throw reject()
      db = new Database(binding.identity_database, { readonly: true, create: false })
      db.exec("PRAGMA query_only=ON")
      const [, boot, ticks] = binding.provider_incarnation.split(":")
      const row = db.query("SELECT invocation_uuid, provider_name FROM pid_identity WHERE os_pid=? AND os_boot_id=? AND os_pid_starttime_ticks=?")
        .get(binding.provider_pid, boot, Number(ticks)) as { invocation_uuid: string; provider_name: string } | null
      if (row) {
        if (row.invocation_uuid !== binding.parent.id || row.provider_name !== binding.provider_instance_id) throw reject()
        return
      }
    } catch { /* Startup identity publication may precede sidecar visibility. */ }
    finally { db?.close() }
    await Bun.sleep(20)
  } while (Date.now() < deadline)
  throw reject()
}

function removeFiles(directory: string, descriptor: Descriptor) {
  const current = readDescriptor(directory)
  if (JSON.stringify(current) !== JSON.stringify(descriptor)) throw reject()
  const entries = readdirSync(directory)
  if (entries.some(name => !["control.json", "control.sock"].includes(name))) throw reject()
  if (entries.includes("control.sock")) { verifySocket(directory); unlinkSync(socketPath(directory)) }
  unlinkSync(`${directory}/control.json`)
  rmdirSync(directory)
}
// Recovery never signals a PID. Unreadable identity is not evidence of death.
export function cleanup(directory: string) {
  privateDirectory(dirname(directory))
  privateDirectory(directory)
  if (!readdirSync(directory).length) { rmdirSync(directory); return }
  const descriptor = readDescriptor(directory)
  try {
    const stat = readFileSync(`/proc/${descriptor.bridge.pid}/stat`, "utf8")
    const state = stat.slice(stat.lastIndexOf(")") + 2).split(" ")[0]
    if (!["Z", "X"].includes(state) && live(descriptor.bridge)) throw reject()
    if (!["Z", "X"].includes(state)) identity(descriptor.bridge.pid) // unknown != recycled
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error
  }
  removeFiles(directory, descriptor)
}

type Attached = { channel: Channel; challenge: string; seq: number; raw: boolean; observing: boolean; deadline: number }
function payload(raw: unknown, authorized: boolean) {
  if (!authorized || raw === undefined) return {}
  const bytes = Buffer.byteLength(JSON.stringify(raw))
  return bytes <= 256 * 1024 ? { payload: raw } : { payload_omitted: "size_limit", payload_bytes: bytes }
}
export async function activate(active: Map<RpcId, Pending>, abort: (id: RpcId, generation?: string) => boolean) {
  const directory = process.env[DIRECTORY_ENV]
  const supplied = process.env[BINDING_ENV]
  // Not even Bash's copied process environment may inherit this capability.
  delete process.env[DIRECTORY_ENV]
  delete process.env[BINDING_ENV]
  if (directory === undefined && supplied === undefined) return undefined
  if (!directory || !supplied || process.platform !== "linux") throw reject()
  const binding = JSON.parse(supplied) as Binding
  await verifyRunner(binding)
  privateDirectory(dirname(directory))
  const path = socketPath(directory)
  const publicBinding: LaunchIdentity = { version: 1, provider_pid: binding.provider_pid, provider_incarnation: binding.provider_incarnation,
    request_id: binding.request_id, provider_instance_id: binding.provider_instance_id, parent: { id: binding.parent.id, source: binding.parent.source } }
  const descriptor: Descriptor = { version: 1, generation: secret(), token: secret(), bridge: identity(process.pid), binding: publicBinding }
  mkdirSync(directory, { mode: 0o700 }) // exclusive: stale and cross-launch slots never get reused
  privateDirectory(directory)
  let listener: ReturnType<typeof listen> | undefined
  try {
    writeFileSync(`${directory}/control.json`, JSON.stringify(descriptor), { flag: "wx", mode: 0o600 })
    listener = listen(path)
    chmodSync(path, 0o600)
  } catch {
    listener?.close()
    try { removeFiles(directory, descriptor) } catch { /* explicit recovery owns partial startup */ }
    throw reject()
  }
  const clients = new Set<Attached>()
  let closed = false
  let retired = false
  const retire = () => {
    if (retired) return
    retired = true
    clearInterval(timer)
    listener!.close()
    try { removeFiles(directory, descriptor) } catch { /* never delete changed/unowned paths */ }
    descriptor.token = ""
  }
  const close = () => {
    if (closed) return
    closed = true
    retire()
    for (const client of clients) client.channel.close()
    clients.clear()
  }
  const event = (kind: string, id: RpcId, pending: Pending, raw?: unknown) => {
    for (const client of clients) {
      if (!client.observing) continue
      client.channel.write({ event: kind, id, request_generation: pending.generation, session_id: pending.session, ...payload(raw, client.raw) })
    }
  }
  const tick = () => {
    if (!live(provider(descriptor))) { close(); return }
    const channel = listener!.accept()
    if (channel) {
      if (clients.size >= 16) channel.close()
      else {
        const client = { channel, challenge: secret(), seq: 0, raw: false, observing: false, deadline: Date.now() + 5000 }
        clients.add(client)
        channel.write({ challenge: client.challenge })
      }
    }
    for (const client of clients) {
      for (const value of client.channel.read()) {
        const m = value as any
        if (!m || !equalSecret(m.token, descriptor.token) || m.generation !== descriptor.generation
            || m.challenge !== client.challenge || m.seq !== client.seq + 1
            || m.request_id !== binding.request_id || m.invocation_id !== binding.parent.id) {
          client.channel.close(); break
        }
        client.seq++
        if (m.op === "observe" && !client.observing && typeof m.raw === "boolean") {
          client.observing = true; client.raw = m.raw
          client.channel.write({ event: "attached", generation: descriptor.generation, binding: publicBinding, bridge: descriptor.bridge,
            active: [...active].map(([id, p]) => ({ id, request_generation: p.generation, aborted: p.controller.signal.aborted,
              session_id: p.session, ...payload(p.request, client.raw) })) })
        } else if (m.op === "cancel" && hex(m.request_generation)) {
          client.channel.write({ event: "cancel", seq: m.seq, id: m.id, request_generation: m.request_generation,
            accepted: abort(m.id, m.request_generation) })
        } else { client.channel.close(); break }
      }
      if (!client.observing && Date.now() >= client.deadline) client.channel.close()
      if (client.channel.closed) clients.delete(client)
    }
  }
  const timer = setInterval(() => { try { tick() } catch { close() } }, 10)
  process.once("exit", close)
  return { close, retire, event }
}
