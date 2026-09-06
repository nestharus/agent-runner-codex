import { expect, test } from "bun:test"
import { identity, live, requirePeerUid } from "./request-control-unix"

test("peer UID validation rejects a different UID (production SO_PEERCRED validator)", () => {
  expect(() => requirePeerUid(process.getuid!())).not.toThrow()
  expect(() => requirePeerUid(process.getuid!() + 1)).toThrow()
})

test("live process identity rejects recycled start time and boot identity", () => {
  const current = identity(process.pid)
  expect(live(current)).toBe(true)
  expect(live({ ...current, incarnation: current.incarnation + "0" })).toBe(false)
  expect(live({ ...current, incarnation: "linux:00000000-0000-0000-0000-000000000000:0" })).toBe(false)
})
