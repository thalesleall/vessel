import { api, blank, errorCode, errorMessage, shellLabel, type Item, type RunRecord, type Snippet } from "./api";
import { byId, lastRuns, personalVaultId, setActiveTab, setLastRuns, toast } from "./store";
import { confirmAction } from "./ui";

export type SnippetItem = Item<{ type: "snippet"; data: Snippet }>;

/** A snippet with every field present, whatever version of the app wrote it. */
export function snippetData(item: SnippetItem): Snippet {
  const d = { ...blank.snippet(), ...item.entity.data };
  if (!d.target?.kind) d.target = { kind: "none" };
  return d;
}

/** Where a snippet runs, in words ("PowerShell", "api-prod-01", "3 hosts"). */
export function targetText(s: Snippet): string {
  switch (s.target.kind) {
    case "none": return "Colar no terminal";
    case "local": return shellLabel(s.target.shell);
    case "hosts": {
      const ids = s.target.host_ids;
      if (ids.length === 1) {
        const h = byId(ids[0]);
        return h && h.entity.type === "host" ? h.entity.data.label || h.entity.data.address : "host removido";
      }
      return `${ids.length} hosts`;
    }
  }
}

function summary(r: RunRecord): string {
  const failed = r.results.filter((t) => t.error || t.timed_out || t.exit_code !== 0);
  if (r.status === "ok") return `"${r.label}" terminou sem erros`;
  if (failed.length === 1) {
    const t = failed[0];
    const why = t.timed_out ? "tempo esgotado" : t.error ?? `código ${t.exit_code}`;
    return `"${r.label}" falhou em ${t.target}: ${why}`;
  }
  return `"${r.label}" falhou em ${failed.length} destinos`;
}

/** Runs a snippet on its own target, asking first when it is marked "confirm" or comes from a
 * shared vault. Returns the finished run, or null when cancelled or refused. */
export async function runSnippet(item: SnippetItem): Promise<RunRecord | null> {
  const s = snippetData(item);
  const ask = async (shared: boolean) => confirmAction({
    title: `Rodar "${s.label}"?`,
    message: shared
      ? `Este snippet vem de um cofre compartilhado: outro membro pode tê-lo alterado. Confira o script que vai rodar em ${targetText(s)}:`
      : `Vai rodar em ${targetText(s)}:`,
    code: s.script,
    confirm: "Rodar",
    danger: false,
  });
  let confirmed = false;
  if (s.confirm || item.vault_id !== personalVaultId()) {
    if (!(await ask(item.vault_id !== personalVaultId()))) return null;
    confirmed = true;
  }
  const start = async (confirmed: boolean): Promise<RunRecord | null> => {
    try {
      const r = await api.runSnippet(item.id, confirmed);
      setLastRuns({ ...lastRuns(), [r.snippet_id]: r });
      toast(summary(r), r.status === "ok" ? "success" : "error");
      return r;
    } catch (e) {
      // The backend has the last word on what needs confirming.
      if (errorCode(e) === "confirm_snippet" && !confirmed) {
        const shared = Boolean((e as { details?: { shared?: boolean } }).details?.shared);
        return (await ask(shared)) ? start(true) : null;
      }
      toast(errorMessage(e), "error");
      return null;
    }
  };
  return start(confirmed);
}

/** Types a snippet into a terminal session (snippets without a target). */
export async function pasteSnippet(s: Snippet, sessionId: string) {
  await api.write(sessionId, s.script.replace(/\r?\n/g, "\r") + "\r");
  setActiveTab(sessionId);
  toast("Snippet enviado ao terminal", "success");
}
