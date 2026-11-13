import { For, Show, createSignal } from "solid-js";
import { api, blank, type Item, type Snippet } from "../api";
import { itemsOf, sessions, setActiveTab, toast } from "../store";
import { Button, Empty, Field, Icon, Modal } from "../ui";
import { EditorFooter, VaultSelect, defaultVault, removeItem, saveEntity, splitTags } from "./common";

type SnippetItem = Item<{ type: "snippet"; data: Snippet }>;

export function SnippetsView() {
  const snippets = itemsOf("snippet");
  const [editing, setEditing] = createSignal<SnippetItem | "new" | null>(null);
  const [running, setRunning] = createSignal<SnippetItem | null>(null);

  return (
    <>
      <div class="page-head">
        <h1>Snippets</h1>
        <div class="spacer" />
        <Button variant="primary" icon="plus" onClick={() => setEditing("new")}>Novo snippet</Button>
      </div>
      <Show when={snippets().length} fallback={<Empty icon="code" title="Nenhum snippet"><span>Guarde comandos que você repete e rode-os em qualquer terminal.</span></Empty>}>
        <div class="list">
          <For each={snippets()}>
            {(s) => (
              <div class="row">
                <Icon name="code" />
                <div class="meta"><div class="name">{s.entity.data.label}</div><div class="sub mono">{s.entity.data.script.split("\n")[0]}</div></div>
                <Button small icon="play" onClick={() => setRunning(s)} disabled={!sessions().some((x) => x.alive)}>Executar</Button>
                <Button small icon="edit" onClick={() => setEditing(s)}>Editar</Button>
              </div>
            )}
          </For>
        </div>
      </Show>
      <Show when={editing()}>{(e) => <SnippetEditor item={e() === "new" ? null : (e() as SnippetItem)} onClose={() => setEditing(null)} />}</Show>
      <Show when={running()}>
        {(s) => (
          <Modal title={`Executar "${s().entity.data.label}"`} onClose={() => setRunning(null)}>
            <div class="list">
              <For each={sessions().filter((x) => x.alive)}>
                {(t) => (
                  <div class="row" style={{ cursor: "pointer" }} onClick={async () => {
                    await api.write(t.id, s().entity.data.script.replace(/\n/g, "\r") + "\r");
                    setRunning(null);
                    setActiveTab(t.id);
                    toast("Snippet enviado", "success");
                  }}>
                    <Icon name={t.kind.kind === "ssh" ? "server" : "terminal"} />
                    <div class="meta"><div class="name">{t.title}</div></div>
                  </div>
                )}
              </For>
            </div>
          </Modal>
        )}
      </Show>
    </>
  );
}

function SnippetEditor(props: { item: SnippetItem | null; onClose: () => void }) {
  const [d, setD] = createSignal<Snippet>(props.item ? { ...props.item.entity.data } : blank.snippet());
  const [tags, setTags] = createSignal(d().tags.join(", "));
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const save = async () => {
    if (!d().script.trim()) return;
    const data = { ...d(), label: d().label.trim() || d().script.split("\n")[0].slice(0, 40), tags: splitTags(tags()) };
    if (await saveEntity(props.item, vault(), { type: "snippet", data })) props.onClose();
  };
  return (
    <Modal title={props.item ? "Editar snippet" : "Novo snippet"} onClose={props.onClose} wide
      footer={<EditorFooter onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "o snippet"); props.onClose(); } : undefined} />}>
      <div class="form">
        <div class="form-row">
          <Field label="Nome"><input value={d().label} onInput={(e) => setD({ ...d(), label: e.currentTarget.value })} autofocus /></Field>
          <Field label="Cofre"><VaultSelect value={vault()} onChange={setVault} /></Field>
        </div>
        <Field label="Script"><textarea rows={8} value={d().script} onInput={(e) => setD({ ...d(), script: e.currentTarget.value })} placeholder="sudo systemctl status nginx" /></Field>
        <Field label="Tags"><input value={tags()} onInput={(e) => setTags(e.currentTarget.value)} /></Field>
      </div>
    </Modal>
  );
}
