import { createMemo, createSignal } from "solid-js";
import { transition } from "./motion";
import { api, errorMessage, on, type Item, type Kind, type RunRecord, type SchedulerStatus, type SessionInfo, type Settings, type Status, type SyncReport, type UpdateStatus, type VaultView } from "./api";

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
const [activeTab, setActiveTabNow] = createSignal<string>("home");
export type Section = "hosts" | "terminals" | "keychain" | "snippets" | "forwards" | "vaults" | "agents" | "settings";
const [section, setSectionNow] = createSignal<Section>("hosts");
export { activeTab, section };

/** Switches the visible tab with a short cross-fade. */
export function setActiveTab(id: string) {
  if (id !== activeTab()) transition(() => setActiveTabNow(id));
}

/** Switches the home section with a short cross-fade. */
export function setSection(s: Section) {
  if (s !== section()) transition(() => setSectionNow(s));
}

export function itemsOf<K extends Kind>(kind: K) {
  return createMemo(() => items().filter((i) => i.entity.type === kind) as Item<Extract<Item["entity"], { type: K }>>[]);
}

/** The user's own vault (verified by the client core, not the server's flag). */
export const personalVaultId = createMemo(() => vaults().find((v) => v.personal)?.id ?? null);

/** Settings and defaults come only from the personal vault: another member of a shared vault
 * must not be able to change them (e.g. the default terminal or the auto-lock delay). */
export const settingsItem = createMemo(() =>
  items().find((i) => i.entity.type === "settings" && i.vault_id === personalVaultId()) ?? null);

export const settings = createMemo<Settings | null>(() => {
  const s = settingsItem();
  return s && s.entity.type === "settings" ? s.entity.data : null;
});

/** Terminal profiles that may be opened implicitly (Ctrl+T, the + tab): the user's own. */
export const ownProfiles = createMemo(() =>
  items().filter((i) => i.entity.type === "terminal_profile" && i.vault_id === personalVaultId()));

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
    // The trust record is the app's own bookkeeping, not something to show or edit.
    setItems(i.filter((x) => (x.entity.type as string) !== "trust"));
    setVaults(v);
    setSessions(t);
  } catch (e) {
    toast(errorMessage(e), "error");
  }
  refreshSnippetRuns();
}

// ---------------------------------------------------------------------------------------------
// Snippet runs and schedules
// ---------------------------------------------------------------------------------------------

/** The latest run of each snippet (by snippet id), to show its state on the card. */
export const [lastRuns, setLastRuns] = createSignal<Record<string, RunRecord>>({});
export const [scheduler, setScheduler] = createSignal<SchedulerStatus | null>(null);

export async function refreshScheduler() {
  try { setScheduler(await api.schedulerStatus()); } catch { /* locked */ }
}

export async function refreshSnippetRuns() {
  await refreshScheduler();
  try {
    const latest: Record<string, RunRecord> = {};
    for (const r of await api.snippetRuns()) if (!latest[r.snippet_id]) latest[r.snippet_id] = r;
    setLastRuns(latest);
  } catch { /* locked */ }
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
  on<SyncReport>("vessel://synced", (r) => {
    setSyncError(null);
    setLastSync(Date.now());
    if (r?.rescued) toast(`${r.rescued} alteração(ões) de um cofre que deixou de existir foram guardadas no seu cofre pessoal.`, "info");
  });
  on<{ message: string }>("vessel://sync-error", (e) => setSyncError(e.message));
  on<string>("vessel://signed-out", async (message) => {
    toast(message, "error");
    await refreshStatus();
  });
  on("terminal://changed", () => refreshSessions());
  on<RunRecord>("snippets://run", (r) => {
    const current = lastRuns()[r.snippet_id];
    if (!current || current.id === r.id || current.started_at <= r.started_at) setLastRuns({ ...lastRuns(), [r.snippet_id]: r });
    refreshScheduler();
  });
  on<boolean>("snippets://paused", () => refreshScheduler());
}

// ---------------------------------------------------------------------------------------------
// App updates
// ---------------------------------------------------------------------------------------------

export const [updateStatus, setUpdateStatus] = createSignal<UpdateStatus | null>(null);

/** Asks the release feed for a newer version. Quiet unless `announce` (a manual check). */
export async function checkForUpdate(announce = false): Promise<void> {
  try {
    const s = await api.updateCheck();
    setUpdateStatus(s);
    if (announce) toast(s.available ? `Versão ${s.available.version} disponível` : "Você já está na versão mais recente", "info");
  } catch (e) {
    if (announce) toast(errorMessage(e), "error");
  }
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
