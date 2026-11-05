const STORAGE_KEY = "flowpilot-accounts"
const ALLOWED_PROVIDERS = ["google-flow", "google_flow", "dola", "migoo", "gemini", "suno"]

/** @param {unknown} value */
export function isStoredAccount(value) {
  if (!value || typeof value !== "object") return false
  const account = /** @type {Record<string, unknown>} */ (value)
  return (
    (account.provider == null || account.provider === "" || (typeof account.provider === "string" && ALLOWED_PROVIDERS.includes(account.provider))) &&
    typeof account.id === "string" &&
    typeof account.name === "string" &&
    (account.email === null || typeof account.email === "string") &&
    (account.avatarUrl === null || typeof account.avatarUrl === "string") &&
    typeof account.avatar === "string" &&
    typeof account.favorite === "boolean" &&
    typeof account.order === "number"
  )
}

/** @param {any[]} accounts */
export function normalizeStoredAccounts(accounts) {
  return accounts.map((account) => ({
    ...account,
    provider: !account.provider || account.provider === "google_flow" ? "google-flow" : account.provider,
  }))
}

/**
 * Load data through the native store. A null native result means the file does
 * not exist; every other read or validation failure must reject so callers
 * cannot mistake lost access to existing data for an empty account list.
 *
 * @param {(command: string, args?: Record<string, unknown>) => Promise<unknown>} invokeStore
 * @param {Pick<Storage, "getItem" | "removeItem">} storage
 * @returns {Promise<any[]>}
 */
export async function loadAccountsWithBridge(invokeStore, storage) {
  const native = await invokeStore("load_accounts")

  if (native !== null) {
    if (!Array.isArray(native) || !native.every(isStoredAccount)) {
      throw new Error("Stored accounts file has an invalid structure. The original file was left unchanged.")
    }
    const normalized = normalizeStoredAccounts(native)
    if (normalized.some((account, index) => native[index].provider !== account.provider)) {
      await invokeStore("save_accounts", { accounts: normalized })
    }
    try {
      storage.removeItem(STORAGE_KEY)
    } catch {
      // Native storage is authoritative after a successful read.
    }
    return normalized.sort((a, b) => a.order - b.order)
  }

  const legacyRaw = storage.getItem(STORAGE_KEY)
  if (legacyRaw === null) return []

  let legacy
  try {
    legacy = JSON.parse(legacyRaw)
  } catch {
    throw new Error("Legacy account data could not be parsed. It was left unchanged.")
  }
  if (!Array.isArray(legacy) || !legacy.every(isStoredAccount)) {
    throw new Error("Legacy account data has an invalid structure. It was left unchanged.")
  }

  const normalized = normalizeStoredAccounts(legacy)
  await invokeStore("save_accounts", { accounts: normalized })
  try {
    storage.removeItem(STORAGE_KEY)
  } catch {
    // The native copy has been committed; leave the legacy copy as a fallback.
  }
  return normalized.sort((a, b) => a.order - b.order)
}

/** Saving is enabled only after a successful, validated account load. */
export function canPersistAccountList(licensed, accountsLoaded) {
  return licensed && accountsLoaded
}

/**
 * @param {(command: string, args?: Record<string, unknown>) => Promise<unknown>} invokeStore
 * @param {Pick<Storage, "removeItem">} storage
 * @param {any[]} accounts
 */
export async function saveAccountsWithBridge(invokeStore, storage, accounts) {
  if (!Array.isArray(accounts) || !accounts.every(isStoredAccount)) {
    throw new Error("Invalid account data. Existing stored accounts were left unchanged.")
  }
  await invokeStore("save_accounts", { accounts })
  try {
    storage.removeItem(STORAGE_KEY)
  } catch {
    // Keep the successful native write; this is only legacy-cache cleanup.
  }
}
