#!/usr/bin/env bun
import { readFile } from "node:fs/promises"
import { createInterface } from "node:readline"
import { createRequire } from "node:module"
import type { StringArgument } from "./opencode-tool-shim"

// Do not edit or translate this implementation: keep its bytes equal to OpenCode.
const overridePath = new URL("../opencode/tools/bash.ts", import.meta.url).pathname
const bundled = await Bun.build({
  entrypoints: [overridePath], target: "node", format: "cjs", write: false,
  plugins: [{
    name: "opencode-tool-registration",
    setup(build) {
      build.onResolve({ filter: /^@opencode-ai\/plugin$/ }, () => ({
        path: new URL("./opencode-tool-shim.ts", import.meta.url).pathname,
      }))
    },
  }],
})
if (!bundled.success) throw new Error("Could not load the pinned Bash implementation")
const loaded = { exports: {} as { default?: any } }
// MCP transport always uses pipes. The vendored tool's process context reflects
// the managed parent TUI so its unchanged interactive delivery/lease policy is
// preserved without changing stdin used by this JSON-RPC server.
const toolProcess = Object.create(process)
if (process.env.AGENT_RUNNER_CODEX_INTERACTIVE === "1") {
  Object.defineProperty(toolProcess, "stdin", { value: { isTTY: true } })
}
new Function("require", "module", "exports", "process", await bundled.outputs[0].text())(
  createRequire(import.meta.url), loaded, loaded.exports, toolProcess,
)
const bash = loaded.exports.default
type RpcId = string | number
type BashArgs = { command?: string; handle?: string; delivery?: string; workdir?: string }
const validSession = (value: string) => /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,255}$/.test(value)
const validNativeSession = (value: string) => /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/.test(value)
let boundMetadataSession: string | undefined

export async function sessionId(timeoutMs = 5000, metadata?: unknown): Promise<string> {
  if (process.env.AGENT_RUNNER_CODEX_SESSION_BINDING === "tool_metadata") {
    // Pinned Codex 0.153.4 core/src/mcp_tool_call.rs injects this field into
    // every MCP tools/call; it is transport metadata, never model arguments.
    const id = metadata && typeof metadata === "object" && !Array.isArray(metadata)
      ? (metadata as Record<string, unknown>).threadId : undefined
    if (typeof id !== "string" || !validNativeSession(id)) throw new Error("Native Codex MCP threadId metadata is required")
    const resumed = process.env.AGENT_RUNNER_CODEX_SESSION_ID
    if ((resumed && resumed !== id) || (boundMetadataSession && boundMetadataSession !== id)) {
      throw new Error("Native Codex MCP session identity changed")
    }
    boundMetadataSession = id
    return id
  }
  const explicit = process.env.AGENT_RUNNER_CODEX_SESSION_ID
  if (explicit) {
    if (!validSession(explicit)) throw new Error("Invalid Codex session binding")
    return explicit
  }
  const path = process.env.AGENT_RUNNER_CODEX_SESSION_FILE
  if (!path) throw new Error("Codex session binding is required before using Bash")
  const deadline = Date.now() + timeoutMs
  do {
    try {
      const value = (await readFile(path, "utf8")).trim()
      if (value) {
        if (!validSession(value)) throw new Error("Invalid Codex session binding")
        return value
      }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error
    }
    if (Date.now() >= deadline) break
    await Bun.sleep(20)
  } while (true)
  throw new Error("Codex session binding was not ready within the startup deadline")
}

export function toolDefinition() {
  const properties: Record<string, { type: string; description: string }> = {}
  const required: string[] = []
  for (const [name, argument] of Object.entries(bash.args) as [string, StringArgument][]) {
    properties[name] = { type: "string", description: argument.description }
    if (!argument.isOptional) required.push(name)
  }
  return {
    name: "bash",
    description: bash.description,
    inputSchema: { type: "object", properties, required, additionalProperties: false },
    annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: true },
  }
}

function parseArguments(value: unknown): BashArgs {
  if (value === undefined) return {}
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("Bash arguments must be an object")
  for (const [key, item] of Object.entries(value)) {
    if (!Object.hasOwn(bash.args, key) || typeof item !== "string") throw new Error(`Invalid Bash argument: ${key}`)
  }
  return value as BashArgs
}

export async function callBash(args: unknown, abort: AbortSignal, metadata?: unknown): Promise<string> {
  const parsed = parseArguments(args)
  const owner = await sessionId(5000, metadata)
  return bash.execute(parsed, { sessionID: owner, abort } as Parameters<typeof bash.execute>[1])
}

export async function serve() {
  const active = new Map<RpcId, { controller: AbortController; promise: Promise<void> }>()
  let closing = false
  const send = (value: unknown) => {
    if (!closing) process.stdout.write(`${JSON.stringify(value)}\n`)
  }
  const error = (id: RpcId | null, code: number, message: string) => send({ jsonrpc: "2.0", id, error: { code, message } })
  const result = (id: RpcId, value: unknown) => send({ jsonrpc: "2.0", id, result: value })
  const close = async () => {
    if (closing) return
    closing = true
    for (const pending of active.values()) pending.controller.abort()
    // Synchronous supervised commands cancel through the unchanged adapter. Async
    // headless dispatches have no owner lease and survive the normal turn exit.
    await Promise.race([
      Promise.allSettled([...active.values()].map((value) => value.promise)),
      Bun.sleep(1500),
    ])
    process.exit(0)
  }
  process.once("SIGTERM", close)
  process.once("SIGINT", close)
  process.stdout.on("error", close)
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity })
  input.on("close", close)
  for await (const line of input) {
    let message: any
    try { message = JSON.parse(line) } catch { error(null, -32700, "Invalid JSON"); continue }
    if (!message || typeof message !== "object" || message.jsonrpc !== "2.0" || typeof message.method !== "string") {
      error(null, -32600, "Invalid JSON-RPC request"); continue
    }
    if (message.method === "notifications/cancelled") {
      active.get(message.params?.requestId)?.controller.abort()
      continue
    }
    if (message.id === undefined) continue
    const id = message.id
    if (typeof id !== "string" && typeof id !== "number") { error(null, -32600, "Invalid request ID"); continue }
    if (active.has(id)) { error(id, -32600, "Request ID is already active"); continue }
    switch (message.method) {
      case "initialize":
        result(id, { protocolVersion: "2024-11-05", capabilities: { tools: {} }, serverInfo: { name: "agent-bash", version: "1.0.0" } })
        break
      case "ping": result(id, {}); break
      case "tools/list": result(id, { tools: [toolDefinition()] }); break
      case "tools/call": {
        if (message.params?.name !== "bash") { error(id, -32602, "Unknown tool"); break }
        const controller = new AbortController()
        const promise = callBash(message.params.arguments, controller.signal, message.params._meta).then(
          (text) => result(id, { content: [{ type: "text", text }] }),
          (failure) => result(id, { content: [{ type: "text", text: String(failure instanceof Error ? failure.message : failure) }], isError: true }),
        ).finally(() => active.delete(id))
        active.set(id, { controller, promise })
        break
      }
      case "resources/list": result(id, { resources: [] }); break
      case "resources/templates/list": result(id, { resourceTemplates: [] }); break
      case "prompts/list": result(id, { prompts: [] }); break
      default: error(id, -32601, "Method not found")
    }
  }
}

if (import.meta.main) await serve()
