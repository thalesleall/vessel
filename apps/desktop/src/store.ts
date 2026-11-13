import { createMemo, createSignal } from "solid-js";
import { api, errorMessage, on, type Item, type Kind, type SessionInfo, type Settings, type Status, type VaultView } from "./api";

// ---------------------------------------------------------------------------------------------
// App-wide state
// ---------------------------------------------------------------------------------------------

export const [status, setStatus] = createSignal<Status | null>(null);
export const [items, setItems] = createSignal<Item[]>([]);
export const [vaults, setVaults] = createSignal<VaultView[]>([]);
export const [sessions, setSessions] = createSignal<SessionInfo[]>([]);
export const [syncing, setSyncing] = createSignal(false);
export const [syncError, setSyncError] = createSignal<string | null>(null);
export const [lastSync, setLastSync] = createSignal<number | null>(null);

/** Which top-level tab is shown: "home" or a terminal session id. */
export const [activeTab, setActiveTab] = createSignal<string>("home");
export type Section = "hosts" | "terminals" | "keychain" | "snippets" | "forwards" | "vaults" | "agents" | "settings";
export const [section, setSection] = createSignal<Section>("hosts");

export function itemsOf<K extends Kind>(kind: K) {
  return createMemo(() => items().filter((i) => i.entity.type === kind) as Item<Extract<Item["entity"], { type: K }>>[]);
}

export const settings = createMemo<Settings | null>(() => {
  const s = items().find((i) => i.entity.type === "settings");
  return s && s.entity.type === "settings" ? s.entity.data : null;
});

export const settingsItem = createMemo(() => items().find((i) => i.entity.type === "settings") ?? null);

export const writableVaults = createMemo(() => vaults().filter((v) => v.role !== "viewer"));

export function vaultName(id: string): string {
  return vaults().find((v) => v.id === id)?.name ?? "";
}

export function byId(id: string | null | undefined): Item | undefined {
  return id ? items().find((i) => i.id === id) : undefined;
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

export async function refreshStatus() {
  setStatus(await api.status());
}

export async function refreshData() {
  const s = status();
  if (!s?.unlocked) return;
  try {
    const [i, v, t] = await Promise.all([api.items(), api.vaults(), api.terminals()]);
    setItems(i);
    setVaults(v);
    setSessions(t);
  } catch (e) {
    toast(errorMessage(e), "error");
  }
}

export async function refreshSessions() {
  setSessions(await api.terminals());
}

let wired = false;
export function wireEvents() {
  if (wired) return;
  wired = true;
  on("vessel://changed", () => refreshData());
  on<boolean>("vessel://syncing", (v) => setSyncing(v));
  on("vessel://synced", () => {
    setSyncError(null);
    setLastSync(Date.now());
  });
  on<{ message: string }>("vessel://sync-error", (e) => setSyncError(e.message));
  on<string>("vessel://signed-out", async (message) => {
    toast(message, "error");
    await refreshStatus();
  });
  on("terminal://changed", () => refreshSessions());
}

// ---------------------------------------------------------------------------------------------
// Toasts
// ---------------------------------------------------------------------------------------------

export interface Toast { id: number; text: string; kind: "info" | "error" | "success" }
export const [toasts, setToasts] = createSignal<Toast[]>([]);
let toastId = 0;
export function toast(text: string, kind: Toast["kind"] = "info") {
  const id = ++toastId;
  setToasts((t) => [...t, { id, text, kind }]);
  setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "error" ? 7000 : 3500);
}

/** Runs an async action, showing errors as toasts. Returns undefined on failure. */
/** Runs an action that returns nothing useful; true when it succeeded (errors become toasts). */
export async function tryAction(fn: () => Promise<unknown>, success?: string): Promise<boolean> {
  try {
    await fn();
    if (success) toast(success, "success");
    return true;
  } catch (e) {
    toast(errorMessage(e), "error");
    return false;
  }
}

/** Runs an action and returns its result, or undefined when it failed (errors become toasts).
 * Do not use it to detect success of an action that returns nothing: use tryAction. */
export async function attempt<T>(fn: () => Promise<T>, success?: string): Promise<T | undefined> {
  try {
    const result = await fn();
    if (success) toast(success, "success");
    return result;
  } catch (e) {
    toast(errorMessage(e), "error");
    return undefined;
  }
}

export async function copy(text: string, what = "Copiado") {
  await navigator.clipboard.writeText(text);
  toast(`${what} para a área de transferência`, "success");
}
