import { For, Show, createMemo, createSignal } from "solid-js";
import { blank, type Group, type Host, type Item } from "../api";
import { itemsOf, vaultName, vaults } from "../store";
import { connectHost, connecting } from "../terminal";
import { Badge, Button, Empty, Field, Icon, Modal, colorFor, initials } from "../ui";
import { EditorFooter, ItemSelect, VaultSelect, defaultVault, removeItem, saveEntity, splitTags } from "./common";

type HostItem = Item<{ type: "host"; data: Host }>;
type GroupItem = Item<{ type: "group"; data: Group }>;

export function HostsView() {
  const hosts = itemsOf("host");
  const groups = itemsOf("group");
  const [query, setQuery] = createSignal("");
  const [group, setGroup] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal<HostItem | "new" | null>(null);
  const [editingGroup, setEditingGroup] = createSignal<GroupItem | "new" | null>(null);

  const filtered = createMemo(() => {
    const q = query().toLowerCase();
    return hosts().filter((h) => {
      const d = h.entity.data;
      if (group() && d.group_id !== group()) return false;
      if (!q) return true;
      return `${d.label} ${d.address} ${d.username ?? ""} ${d.tags.join(" ")}`.toLowerCase().includes(q);
    });
  });

  return (
    <>
      <div class="page-head">
        <h1>Hosts</h1>
        <div class="spacer" />
        <div class="search"><Icon name="search" size={16} />
          <input placeholder="Buscar hosts, endereços, tags…" value={query()} onInput={(e) => setQuery(e.currentTarget.value)} />
        </div>
        <Button icon="folder" onClick={() => setEditingGroup("new")}>Novo grupo</Button>
        <Button variant="primary" icon="plus" onClick={() => setEditing("new")}>Novo host</Button>
      </div>

      <Show when={groups().length > 0}>
        <div class="chips">
          <button class="chip" classList={{ on: group() === null }} onClick={() => setGroup(null)}>Todos</button>
          <For each={groups()}>
            {(g) => (
              <button class="chip" classList={{ on: group() === g.id }} onClick={() => setGroup(g.id)}
                onDblClick={() => setEditingGroup(g)} title="Duplo clique para editar">
                <Icon name="folder" size={13} />{g.entity.data.label}
                <span style={{ color: "var(--text-3)" }}>{hosts().filter((h) => h.entity.data.group_id === g.id).length}</span>
              </button>
            )}
          </For>
        </div>
      </Show>

      <Show when={filtered().length > 0} fallback={
        <Empty icon="server" title={hosts().length ? "Nenhum host encontrado" : "Nenhum host ainda"}>
          <span>Adicione servidores SSH. Eles sincronizam, criptografados, com todos os seus computadores.</span>
          <Button variant="primary" icon="plus" onClick={() => setEditing("new")}>Adicionar host</Button>
        </Empty>
      }>
        <div class="grid">
          <For each={filtered()}>
            {(h) => {
              const d = () => h.entity.data;
              const title = () => d().label || d().address;
              return (
                <div class="card" onClick={() => connectHost(h.id)} title="Conectar">
                  <div class="avatar" style={{ background: d().color || colorFor(title()) }}>
                    <Show when={connecting() === h.id} fallback={initials(title())}><span class="spinner" /></Show>
                  </div>
                  <div class="meta">
                    <div class="name">{title()}</div>
                    <div class="sub">{d().username ? `${d().username}@` : ""}{d().address}{d().port && d().port !== 22 ? `:${d().port}` : ""}</div>
                    <Show when={vaults().length > 1}><div class="sub">{vaultName(h.vault_id)}</div></Show>
                  </div>
                  <Show when={h.pending}><Badge tone="amber">não enviado</Badge></Show>
                  <div class="actions" onClick={(e) => e.stopPropagation()}>
                    <button class="icon-btn" title="Editar" onClick={() => setEditing(h)}><Icon name="edit" size={15} /></button>
                  </div>
                </div>
              );
            }}
          </For>
        </div>
      </Show>

      <Show when={editing()}>{(e) => <HostEditor item={e() === "new" ? null : (e() as HostItem)} group={group()} onClose={() => setEditing(null)} />}</Show>
      <Show when={editingGroup()}>{(e) => <GroupEditor item={e() === "new" ? null : (e() as GroupItem)} onClose={() => setEditingGroup(null)} />}</Show>
    </>
  );
}

function HostEditor(props: { item: HostItem | null; group: string | null; onClose: () => void }) {
  const [d, setD] = createSignal<Host>(props.item ? { ...props.item.entity.data } : { ...blank.host(), group_id: props.group });
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const [tags, setTags] = createSignal(d().tags.join(", "));
  const [busy, setBusy] = createSignal(false);
  const set = <K extends keyof Host>(k: K, v: Host[K]) => setD((x) => ({ ...x, [k]: v }));

  const save = async () => {
    if (!d().address.trim()) return;
    setBusy(true);
    const host: Host = { ...d(), address: d().address.trim(), tags: splitTags(tags()), username: d().username?.trim() || null, password: d().password || null };
    if (await saveEntity(props.item, vault(), { type: "host", data: host })) props.onClose();
    setBusy(false);
  };

  return (
    <Modal title={props.item ? "Editar host" : "Novo host"} onClose={props.onClose} wide
      footer={<EditorFooter busy={busy()} onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "o host"); props.onClose(); } : undefined} />}>
      <div class="form">
        <div class="form-row-3">
          <Field label="Endereço"><input value={d().address} onInput={(e) => set("address", e.currentTarget.value)} placeholder="192.168.0.10 ou servidor.com" autofocus /></Field>
          <Field label="Porta"><input type="number" value={d().port ?? ""} placeholder="22" onInput={(e) => set("port", e.currentTarget.value ? Number(e.currentTarget.value) : null)} /></Field>
          <Field label="Cofre"><VaultSelect value={vault()} onChange={setVault} /></Field>
        </div>
        <div class="form-row">
          <Field label="Nome"><input value={d().label} onInput={(e) => set("label", e.currentTarget.value)} placeholder="ex.: Banco de produção" /></Field>
          <Field label="Grupo"><ItemSelect kind="group" value={d().group_id} onChange={(v) => set("group_id", v)} /></Field>
        </div>
        <div class="section-title" style={{ margin: "6px 0 0" }}><Icon name="key" size={14} />Credenciais</div>
        <div class="form-row">
          <Field label="Identidade" hint="Usuário e chave reutilizáveis entre hosts.">
            <ItemSelect kind="identity" value={d().identity_id} onChange={(v) => set("identity_id", v)} none="Nenhuma (usar campos abaixo)" />
          </Field>
          <Field label="Chave SSH"><ItemSelect kind="ssh_key" value={d().ssh_key_id} onChange={(v) => set("ssh_key_id", v)} none="Da identidade / nenhuma" /></Field>
        </div>
        <div class="form-row">
          <Field label="Usuário"><input value={d().username ?? ""} onInput={(e) => set("username", e.currentTarget.value)} placeholder="da identidade" /></Field>
          <Field label="Senha"><input type="password" value={d().password ?? ""} onInput={(e) => set("password", e.currentTarget.value)} placeholder="opcional" autocomplete="new-password" /></Field>
        </div>
        <div class="section-title" style={{ margin: "6px 0 0" }}><Icon name="route" size={14} />Avançado</div>
        <div class="form-row">
          <Field label="Jump host (ProxyJump)"><ItemSelect kind="host" value={d().jump_host_id} onChange={(v) => set("jump_host_id", v)} exclude={props.item?.id} /></Field>
          <Field label="Snippet ao conectar"><ItemSelect kind="snippet" value={d().startup_snippet_id} onChange={(v) => set("startup_snippet_id", v)} /></Field>
        </div>
        <div class="form-row">
          <Field label="Tags" hint="Separadas por vírgula."><input value={tags()} onInput={(e) => setTags(e.currentTarget.value)} placeholder="prod, banco" /></Field>
          <Field label="Keepalive (s)"><input type="number" value={d().keepalive_secs ?? ""} placeholder="30" onInput={(e) => set("keepalive_secs", e.currentTarget.value ? Number(e.currentTarget.value) : null)} /></Field>
        </div>
        <Field label="Notas"><textarea value={d().notes} onInput={(e) => set("notes", e.currentTarget.value)} style={{ "font-family": "var(--font)" }} /></Field>
      </div>
    </Modal>
  );
}

function GroupEditor(props: { item: GroupItem | null; onClose: () => void }) {
  const [d, setD] = createSignal<Group>(props.item ? { ...props.item.entity.data } : blank.group());
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const save = async () => {
    if (!d().label.trim()) return;
    if (await saveEntity(props.item, vault(), { type: "group", data: d() })) props.onClose();
  };
  return (
    <Modal title={props.item ? "Editar grupo" : "Novo grupo"} onClose={props.onClose}
      footer={<EditorFooter onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "o grupo"); props.onClose(); } : undefined} />}>
      <div class="form">
        <Field label="Nome"><input value={d().label} onInput={(e) => setD({ ...d(), label: e.currentTarget.value })} autofocus /></Field>
        <Field label="Identidade padrão" hint="Usada pelos hosts do grupo sem credenciais próprias.">
          <ItemSelect kind="identity" value={d().identity_id} onChange={(v) => setD({ ...d(), identity_id: v })} />
        </Field>
        <Field label="Grupo pai"><ItemSelect kind="group" value={d().parent_id} onChange={(v) => setD({ ...d(), parent_id: v })} exclude={props.item?.id} /></Field>
        <Field label="Cofre"><VaultSelect value={vault()} onChange={setVault} /></Field>
      </div>
    </Modal>
  );
}
