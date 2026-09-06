// Linux/glibc boundary. Explicit nonblocking FDs avoid undocumented Bun socket
// internals: both peers are checked with SO_PEERCRED before any protocol bytes.
import { dlopen, ptr, toArrayBuffer } from "bun:ffi"
import { readFileSync, statSync } from "node:fs"

export const LIMIT = 1024 * 1024
const fail = () => new Error("Request-control Unix boundary unavailable")
let api: ReturnType<typeof load> | undefined
function load() {
  if (process.platform !== "linux" || !process.getuid || process.getuid() !== process.geteuid!()) throw fail()
  return dlopen("libc.so.6", {
    socket: { args: ["i32", "i32", "i32"], returns: "i32" },
    bind: { args: ["i32", "ptr", "u32"], returns: "i32" },
    connect: { args: ["i32", "ptr", "u32"], returns: "i32" },
    listen: { args: ["i32", "i32"], returns: "i32" },
    accept4: { args: ["i32", "ptr", "ptr", "i32"], returns: "i32" },
    getsockopt: { args: ["i32", "i32", "i32", "ptr", "ptr"], returns: "i32" },
    recv: { args: ["i32", "ptr", "u64", "i32"], returns: "i64" },
    send: { args: ["i32", "ptr", "u64", "i32"], returns: "i64" },
    close: { args: ["i32"], returns: "i32" },
    __errno_location: { args: [], returns: "ptr" },
  })
}
function libc() { return (api ??= load()).symbols }
function errno() { return new Int32Array(toArrayBuffer(libc().__errno_location()!, 0, 4))[0] }
export type Identity = { pid: number; incarnation: string }
export function identity(pid: number): Identity {
  if (!Number.isSafeInteger(pid) || pid < 1) throw fail()
  if (statSync(`/proc/${pid}`).uid !== process.getuid!()) throw fail()
  const stat = readFileSync(`/proc/${pid}/stat`, "utf8")
  const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ")
  if (["Z", "X"].includes(fields[0]) || !/^\d+$/.test(fields[19])) throw fail()
  const boot = readFileSync("/proc/sys/kernel/random/boot_id", "utf8").trim()
  if (!/^[0-9a-f-]{36}$/.test(boot)) throw fail()
  return { pid, incarnation: `linux:${boot}:${fields[19]}` }
}
export function live(expected: Identity) {
  try { return identity(expected.pid).incarnation === expected.incarnation } catch { return false }
}
export function descendant(child: number, ancestor: Identity) {
  for (let depth = 0; depth < 64 && child > 1; depth++) {
    if (child === ancestor.pid) return live(ancestor)
    identity(child)
    const stat = readFileSync(`/proc/${child}/stat`, "utf8")
    child = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1])
  }
  return false
}
function address(path: string) {
  const bytes = Buffer.from(path)
  if (!path.startsWith("/") || bytes.length > 103 || path.includes("\0")) throw fail()
  const addr = Buffer.alloc(110)
  addr.writeUInt16LE(1) // AF_UNIX
  bytes.copy(addr, 2)
  return addr
}
function peer(fd: number) {
  const credentials = new Int32Array(3)
  const length = new Uint32Array([12])
  if (libc().getsockopt(fd, 1, 17, ptr(credentials), ptr(length)) !== 0 || length[0] !== 12) throw fail()
  requirePeerUid(credentials[1])
  return identity(credentials[0])
}
export function requirePeerUid(uid: number) {
  if (uid !== process.getuid!()) throw fail()
}
const FLAGS = 0x800 | 0x80000 // SOCK_NONBLOCK | SOCK_CLOEXEC
export class Channel {
  readonly peer: Identity
  private input = Buffer.alloc(0)
  private output = Buffer.alloc(0)
  closed = false
  constructor(readonly fd: number) {
    try { this.peer = peer(fd) } catch { libc().close(fd); throw fail() }
  }
  write(value: unknown) {
    const bytes = Buffer.from(JSON.stringify(value) + "\n")
    if (this.closed || this.output.length + bytes.length > LIMIT) { this.close(); return }
    this.output = Buffer.concat([this.output, bytes])
    this.flush()
  }
  flush() {
    if (this.closed || !this.output.length) return
    const sent = Number(libc().send(this.fd, ptr(this.output), this.output.length, 0x4000)) // MSG_NOSIGNAL
    if (sent > 0) this.output = this.output.subarray(sent)
    else if (![4, 11].includes(errno())) this.close()
  }
  read(verifyPeer = true): unknown[] {
    if (this.closed) return []
    if (verifyPeer && !live(this.peer)) { this.close(); return [] }
    this.flush()
    if (this.input.indexOf(10) < 0) {
      const chunk = Buffer.alloc(16384)
      const count = Number(libc().recv(this.fd, ptr(chunk), chunk.length, 0))
      if (count === 0 || (count < 0 && ![4, 11].includes(errno()))) this.close()
      if (count > 0) this.input = Buffer.concat([this.input, chunk.subarray(0, count)])
    }
    if (this.input.length > LIMIT) { this.close(); return [] }
    const messages: unknown[] = []
    for (let n = 0; n < 32; n++) {
      const end = this.input.indexOf(10)
      if (end < 0) break
      try { messages.push(JSON.parse(this.input.subarray(0, end).toString("utf8"))) }
      catch { this.close(); return [] }
      this.input = this.input.subarray(end + 1)
    }
    return messages
  }
  close() {
    if (!this.closed) libc().close(this.fd)
    this.closed = true
    this.input = this.output = Buffer.alloc(0)
  }
}
export function listen(path: string) {
  const fd = libc().socket(1, 1 | FLAGS, 0)
  if (fd < 0) throw fail()
  try {
    const addr = address(path)
    if (libc().bind(fd, ptr(addr), addr.length) !== 0 || libc().listen(fd, 16) !== 0) throw fail()
  } catch { libc().close(fd); throw fail() }
  return {
    accept(): Channel | undefined {
      const client = libc().accept4(fd, null, null, FLAGS)
      if (client < 0) return undefined
      try { return new Channel(client) } catch { return undefined }
    },
    close() { libc().close(fd) },
  }
}
export function connect(path: string, expected: Identity) {
  const fd = libc().socket(1, 1 | FLAGS, 0)
  if (fd < 0) throw fail()
  const addr = address(path)
  if (libc().connect(fd, ptr(addr), addr.length) !== 0) { libc().close(fd); throw fail() }
  const channel = new Channel(fd)
  if (channel.peer.pid !== expected.pid || channel.peer.incarnation !== expected.incarnation) {
    channel.close(); throw fail()
  }
  return channel
}
