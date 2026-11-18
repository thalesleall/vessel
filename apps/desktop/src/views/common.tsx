import { For, Show } from "solid-js";
import { api, type Entity, type Item, type Kind } from "../api";
import { attempt, tryAction, items, refreshData, vaults, writableVaults } from "../store";
import { Button, confirmAction } from "../ui";

export function VaultSelect(props: { value: string; onChange: (id: string) => void; label?: string }) {
  return (
    <select value={props.value} onChange={(e) => props.onChange(e.currentTarget.value)}>
      <For each={writableVaults()}>{(v) => <option value={v.id}>{v.personal ? `${v.name} (pessoal)` : v.name}</option>}</For>
    </select>
  );
}

export function ItemSelect(props: { kind: Kind; value: string | null; onChange: (id: string | null) => void; none?: string; exclude?: string }) {
  const options = () =>
    items().filter((i) => i.entity.type === props.kind && i.id !== props.exclude)
      .map((i) => ({ id: i.id, label: labelOf(i), folder: i.entity.type === "ssh_key" ? (i.entity.data.folder ?? "").trim() : "" }));
  // SSH keys filed in folders are listed under them.
  const folders = () => [...new Set(options().map((o) => o.folder))].sort((a, b) => (a === "" ? 1 : b === "" ? -1 : a.localeCompare(b)));
  return (
    <select value={props.value ?? ""} onChange={(e) => props.onChange(e.currentTarget.value || null)}>
      <option value="">{props.none ?? "Nenhum"}</option>
      <Show when={folders().some((f) => f)} fallback={<For each={options()}>{(o) => <option value={o.id}>{o.label}</option>}</For>}>
        <For each={folders()}>{(f) => (
          <optgroup label={f || "Sem pasta"}>
            <For each={options().filter((o) => o.folder === f)}>{(o) => <option value={o.id}>{o.label}</option>}</For>
          </optgroup>
        )}</For>
      </Show>
    </select>
  );
}

export function labelOf(item: Item): string {
  const e = item.entity;
  switch (e.type) {
    case "host": return e.data.label || e.data.address;
    case "known_host": return e.data.host;
    case "settings": return "Configurações";
    default: return (e.data as { label?: string }).label || "(sem nome)";
  }
}

export function defaultVault(): string {
  return vaults().find((v) => v.personal)?.id ?? writableVaults()[0]?.id ?? "";
}

/** Saves an entity, moving it first when the vault changed. */
export async function saveEntity(original: Item | null, vaultId: string, entity: Entity, after?: (id: string) => Promise<unknown>): Promise<boolean> {
  const ok = await tryAction(async () => {
    let id = original?.id ?? null;
    if (original && original.vault_id !== vaultId) {
      id = await api.moveItem(original.vault_id, vaultId, original.id);
    }
    const saved = await api.saveItem(vaultId, id, entity);
    if (after) await after(saved);
  }, "Salvo");
  await refreshData();
  return ok;
}

export async function removeItem(item: Item, what: string) {
  if (!(await confirmAction(`Excluir ${what} "${labelOf(item)}"?\nA exclusão sincroniza com todos os seus dispositivos e membros do cofre.`))) return;
  await attempt(() => api.deleteItem(item.vault_id, item.id), "Excluído");
  await refreshData();
}

export function EditorFooter(props: { onCancel: () => void; onSave: () => void; onDelete?: () => void; busy?: boolean }) {
  return (
    <>
      <Show when={props.onDelete}>
        <Button variant="danger" icon="trash" onClick={() => props.onDelete!()}>Excluir</Button>
        <span style={{ flex: 1 }} />
      </Show>
      <Button onClick={props.onCancel}>Cancelar</Button>
      <Button variant="primary" busy={props.busy} onClick={props.onSave}>Salvar</Button>
    </>
  );
}

export function splitTags(text: string): string[] {
  return text.split(",").map((t) => t.trim()).filter(Boolean);
}
