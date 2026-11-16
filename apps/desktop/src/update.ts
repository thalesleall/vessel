// Asking to install an app update (the check itself lives in store.ts).
import { api } from "./api";
import { toast, tryAction, updateStatus } from "./store";
import { confirmAction } from "./ui";

/** Shows what changes, asks for confirmation, then installs and restarts. */
export async function installUpdate(): Promise<void> {
  const next = updateStatus()?.available;
  if (!next) return;
  const notes = next.notes?.trim() ? `

${next.notes.trim().slice(0, 600)}` : "";
  const ok = await confirmAction({
    title: `Atualizar para a versão ${next.version}?`,
    message: `Terminais abertos e redirecionamentos ativos serão encerrados, e o Vessel reinicia sozinho.${notes}`,
    confirm: "Atualizar e reiniciar",
    danger: false,
  });
  if (!ok) return;
  toast("Baixando a atualização…", "info");
  await tryAction(() => api.updateInstall(next.version));
}
