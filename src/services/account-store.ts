import { invoke } from "@tauri-apps/api/core"
import { loadAccountsWithBridge, saveAccountsWithBridge } from "./account-store-core.js"

export type StoredAccount = {
  provider?: "google-flow" | "google_flow" | "dola" | "migoo" | "gemini" | "suno"
  id: string
  name: string
  email: string | null
  avatarUrl: string | null
  avatar: string
  favorite: boolean
  order: number
}

const invokeAccountStore = (command: string, args?: Record<string, unknown>) =>
  invoke<unknown>(command, args)

export async function loadAccounts(): Promise<StoredAccount[]> {
  return loadAccountsWithBridge(invokeAccountStore, localStorage)
}

export async function saveAccounts(accounts: StoredAccount[]): Promise<void> {
  return saveAccountsWithBridge(invokeAccountStore, localStorage, accounts)
}
