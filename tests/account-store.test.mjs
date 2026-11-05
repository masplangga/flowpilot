import assert from "node:assert/strict"
import test from "node:test"
import {
  canPersistAccountList,
  isStoredAccount,
  loadAccountsWithBridge,
  normalizeStoredAccounts,
  saveAccountsWithBridge,
} from "../src/services/account-store-core.js"

const storageKey = "flowpilot-accounts"

function account(id, order = 0) {
  return {
    id,
    name: `Account ${id}`,
    provider: "google-flow",
    email: null,
    avatarUrl: null,
    avatar: "AC",
    favorite: false,
    order,
  }
}

function memoryStorage(initial = null) {
  let value = initial
  return {
    getItem(key) { assert.equal(key, storageKey); return value },
    removeItem(key) { assert.equal(key, storageKey); value = null },
    current() { return value },
  }
}

test("A: missing native file and no legacy data initializes empty accounts", async () => {
  const calls = []
  const result = await loadAccountsWithBridge(async (command) => { calls.push(command); return null }, memoryStorage())
  assert.deepEqual(result, [])
  assert.deepEqual(calls, ["load_accounts"])
})

test("B/C: valid native JSON preserves every existing account", async () => {
  const accounts = Array.from({ length: 40 }, (_, index) => account(`id-${index}`, index))
  const result = await loadAccountsWithBridge(async () => accounts, memoryStorage())
  assert.equal(result.length, 40)
  assert.deepEqual(result.map(({ id }) => id), accounts.map(({ id }) => id))
})

test("C: legacy provider aliases migrate only after the native write succeeds", async () => {
  const storage = memoryStorage(JSON.stringify([{ ...account("legacy"), provider: "google_flow" }]))
  const calls = []
  const result = await loadAccountsWithBridge(async (command, args) => {
    calls.push({ command, args })
    return null
  }, storage)
  assert.equal(result[0].provider, "google-flow")
  assert.equal(calls[1].command, "save_accounts")
  assert.equal(storage.current(), null)
})

test("D: invalid account structures reject without dropping or saving entries", async () => {
  const malformed = [account("good"), { ...account("old"), provider: "unknown-provider" }]
  const calls = []
  await assert.rejects(
    loadAccountsWithBridge(async (command) => { calls.push(command); return malformed }, memoryStorage()),
    /invalid structure/i,
  )
  assert.deepEqual(calls, ["load_accounts"])
})

test("F: native read failure is surfaced and never treated as an empty store", async () => {
  const calls = []
  await assert.rejects(
    loadAccountsWithBridge(async (command) => {
      calls.push(command)
      throw new Error("permission denied")
    }, memoryStorage(JSON.stringify([account("legacy")]))),
    /permission denied/,
  )
  assert.deepEqual(calls, ["load_accounts"])
  assert.equal(canPersistAccountList(true, false), false)
})

test("G: retry can recover after a temporary native failure", async () => {
  const accounts = [account("recovered")]
  let attempts = 0
  const invokeStore = async (command) => {
    assert.equal(command, "load_accounts")
    attempts += 1
    if (attempts === 1) throw new Error("temporary read failure")
    return accounts
  }
  const storage = memoryStorage()
  await assert.rejects(loadAccountsWithBridge(invokeStore, storage), /temporary read failure/)
  assert.equal(storage.current(), null)
  const recovered = await loadAccountsWithBridge(invokeStore, storage)
  assert.deepEqual(recovered, accounts)
  assert.equal(canPersistAccountList(true, true), true)
})

test("H: account changes persist after a successful load, with validation", async () => {
  const accounts = [account("saved")]
  const calls = []
  const loaded = await loadAccountsWithBridge(async () => accounts, memoryStorage())
  assert.equal(canPersistAccountList(true, false), false)
  assert.equal(canPersistAccountList(true, true), true)
  await saveAccountsWithBridge(async (command, args) => { calls.push({ command, args }) }, memoryStorage(), loaded)
  assert.equal(calls[0].command, "save_accounts")
  assert.deepEqual(calls[0].args.accounts, accounts)
  await assert.rejects(saveAccountsWithBridge(async () => { throw new Error("should not write") }, memoryStorage(), [{}]), /Invalid account data/)
})

test("stored account validation accepts legacy provider forms and rejects unknown providers", () => {
  assert.equal(isStoredAccount({ ...account("old"), provider: "google_flow" }), true)
  assert.equal(normalizeStoredAccounts([{ ...account("old"), provider: "google_flow" }])[0].provider, "google-flow")
  assert.equal(isStoredAccount({ ...account("bad"), provider: "unknown" }), false)
})
