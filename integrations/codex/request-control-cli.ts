// This standalone controller never reads the model's stdio or signals a PID.
import { createInterface } from "node:readline"
import { cleanup, provider, readDescriptor, socketPath, verifySocket } from "./request-control"
import { connect, live } from "./request-control-unix"

async function run(args: string[]) {
  const [op, directory, flag] = args
  if (!["observe", "cancel", "cleanup"].includes(op) || !directory || args.length > 3
      || (flag !== undefined && !(op === "observe" && flag === "--raw"))) {
    throw new Error("Usage: request-control observe DIR [--raw] | cancel DIR | cleanup DIR")
  }
  if (op === "cleanup") { cleanup(directory); return }
  const descriptor = readDescriptor(directory)
  verifySocket(directory)
  if (!live(provider(descriptor))) throw new Error("Stale request-control provider")
  const channel = connect(socketPath(directory), descriptor.bridge)
  let challenge: string | undefined
  let sequence = 0
  let attached = false
  let done = false
  let cancelled = false
  let input: ReturnType<typeof createInterface> | undefined
  const send = (value: Record<string, unknown>) => {
    if (!live(provider(descriptor)) || !live(descriptor.bridge)) throw new Error("Stale control target")
    channel.write({ ...value, token: descriptor.token,
      generation: descriptor.generation, request_id: descriptor.binding.request_id,
      invocation_id: descriptor.binding.parent.id, challenge, seq: ++sequence })
  }
  const stop = () => { done = true }
  process.once("SIGTERM", stop)
  process.once("SIGINT", stop)
  process.stdout.on("error", stop)
  const started = Date.now()
  let drainDeadline: number | undefined
  try {
    while (!done) {
      if (!live(provider(descriptor)) || !live(descriptor.bridge)) drainDeadline ??= Date.now() + 2000
      if (drainDeadline !== undefined && Date.now() > drainDeadline) throw new Error("Control drain deadline")
      // The connected socket cannot be rebound to a recycled PID. Drain final
      // authenticated events after exit, but send() forbids any new controls.
      for (const value of channel.read(false)) {
        const message = value as any
        if (!challenge) {
          if (typeof message.challenge !== "string" || !/^[0-9a-f]{64}$/.test(message.challenge)) throw new Error("Invalid handshake")
          challenge = message.challenge
          if (op === "observe") send({ op, raw: flag === "--raw" })
          // Cancel selectors travel on stdin, never auth material or payloads in argv.
          input = createInterface({ input: process.stdin, crlfDelay: Infinity })
          input.on("line", (line) => {
            try {
              if (Buffer.byteLength(line) > 4096 || (op === "cancel" && cancelled)) throw new Error()
              const selector = JSON.parse(line)
              if (typeof selector.id !== "string" && !Number.isSafeInteger(selector.id)) throw new Error()
              if (!/^[0-9a-f]{64}$/.test(selector.request_generation)) throw new Error()
              send({ op: "cancel", id: selector.id, request_generation: selector.request_generation })
              cancelled = true
            } catch { process.exitCode = 2; done = true }
          })
          input.on("close", () => { if (op === "cancel" && !cancelled) { process.exitCode = 2; done = true } })
        } else {
          if (message.event === "attached") attached = true
          // Never print the handshake token/challenge or descriptor auth.
          if (["attached", "request", "session", "response", "retired", "cancel"].includes(message.event)) {
            if (!process.stdout.write(JSON.stringify(message) + "\n")) throw new Error("Observer output backpressure limit")
          } else throw new Error("Invalid control event")
          if (op === "cancel" && message.event === "cancel") { process.exitCode = message.accepted ? 0 : 1; done = true }
        }
      }
      if (channel.closed) {
        if (!attached && !done) throw new Error("Control endpoint closed before acknowledgement")
        done = true
      }
      if (!attached && !done && Date.now() - started > 5000) throw new Error("Control handshake deadline")
      if (!done) await Bun.sleep(10)
    }
  } finally { input?.close(); channel.close() }
}

try { await run(process.argv.slice(2)) }
catch { console.error("Request control rejected, unavailable, or disconnected"); process.exitCode = 2 }
