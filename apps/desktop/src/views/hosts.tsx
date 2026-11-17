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
          <input type="search" aria-label="Buscar hosts" placeholder="Buscar hosts, endereços, tags…" value={query()} onInput={(e) => setQuery(e.currentTarget.value)} />
        </div>
        <Button icon="folder" onClick={() => setEditingGroup("new")}>Novo grupo</Button>
        <Button variant="primary" icon="plus" onClick={() => setEditing("new")}>Novo host</Button>
      </div>

      <Show when={groups().length > 0}>
        <div class="chips">
          <button class="chip" classList={{ on: group() === null }} aria-pressed={group() === null} onClick={() => setGroup(null)}>Todos</button>
          <For each={groups()}>
            {(g) => (
              <button class="chip" classList={{ on: group() === g.id }} aria-pressed={group() === g.id} onClick={() => setGroup(g.id)}
                onDblClick={() => setEditingGroup(g)} onKeyDown={(e) => e.key === "F2" && setEditingGroup(g)} title="Duplo clique ou F2 para editar">
                <Icon name="folder" size={13} />{g.entity.data.label}
                <span style={{ color: "var(--text-3)" }}>{hosts().filter((h) => h.entity.data.group_id === g.id).length}</span>
              </button>
            )}
          </For>
          <Show when={groups().find((g) => g.id === group())}>
            {(g) => <button class="icon-btn" aria-label={`Editar grupo ${g().entity.data.label}`} title="Editar grupo" onClick={() => setEditingGroup(g())}><Icon name="edit" size={14} /></button>}
          </Show>
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
                <div class="card">
                  <button class="card-hit" onClick={() => connectHost(h.id)} aria-label={`Conectar a ${title()}`} title="Conectar" />
                  <div class="avatar" style={{ background: d().color || colorFor(title()) }}>
                    <Show when={connecting() === h.id} fallback={initials(title())}><span class="spinner" /></Show>
                  </div>
                  <div class="meta">
                    <div class="name">{title()}</div>
                    <div class="sub">{d().username ? `${d().username}@` : ""}{d().address}{d().port && d().port !== 22 ? `:${d().port}` : ""}</div>
                    <Show when={vaults().length > 1}><div class="sub">{vaultName(h.vault_id)}</div></Show>
                  </div>
                  <Show when={h.pending}><Badge tone="amber">não enviado</Badge></Show>
                  <div class="actions">
                    <button class="icon-btn" aria-label="Editar" title="Editar" onClick={() => setEditing(h)}><Icon name="edit" size={15} /></button>
                  </div>
                </div>
              );
            }}
          </For>
        </div>
      </Show>

      <Show when={editing()} keyed>{(e) => <HostEditor item={e === "new" ? null : (e as HostItem)} group={group()} onClose={() => setEditing(null)} />}</Show>
      <Show when={editingGroup()} keyed>{(e) => <GroupEditor item={e === "new" ? null : (e as GroupItem)} onClose={() => setEditingGroup(null)} />}</Show>
    </>
  );
}

function HostEditor(props: { item: HostItem | null; group: string | null; onClose: () => void }) {
  const initial = (): Host => {
    const h = props.item ? { ...blank.host(), ...props.item.entity.data } : { ...blank.host(), group_id: props.group };
    // Older versions only wrote the single field.
    if (!h.startup_snippet_ids?.length && h.startup_snippet_id) h.startup_snippet_ids = [h.startup_snippet_id];
    return h;
  };
  const [d, setD] = createSignal<Host>(initial());
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const [tags, setTags] = createSignal(d().tags.join(", "));
  const [busy, setBusy] = createSignal(false);
  const set = <K extends keyof Host>(k: K, v: Host[K]) => setD((x) => ({ ...x, [k]: v }));

  const [error, setError] = createSignal<string | null>(null);
  let addressInput: HTMLInputElement | undefined;
  const save = async () => {
    if (!d().address.trim()) {
      setError("Informe o endereço do host.");
      addressInput?.focus();
      return;
    }
    setError(null);
    setBusy(true);
    const host: Host = { ...d(), startup_snippet_id: d().startup_snippet_ids[0] ?? null, address: d().address.trim(), tags: splitTags(tags()), username: d().username?.trim() || null, password: d().password || null };
    if (await saveEntity(props.item, vault(), { type: "host", data: host })) props.onClose();
    setBusy(false);
  };

  return (
    <Modal title={props.item ? "Editar host" : "Novo host"} onClose={props.onClose} wide
      footer={<EditorFooter busy={busy()} onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "o host"); props.onClose(); } : undefined} />}>
      <div class="form">
        <div class="form-row-3">
          <Field label="Endereço" hint={error() ?? undefined}><input ref={addressInput} value={d().address} aria-invalid={!!error()} onInput={(e) => { set("address", e.currentTarget.value); setError(null); }} placeholder="192.168.0.10 ou servidor.com…" autofocus /></Field>
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
          <Field label="Snippets ao conectar" hint="Digitados no terminal logo após conectar, nesta ordem.">
            <StartupSnippets ids={d().startup_snippet_ids} onChange={(ids) => set("startup_snippet_ids", ids)} />
          </Field>
        </div>
        <div class="form-row">
          <Field label="Tags" hint="Separadas por vírgula."><input value={tags()} onInput={(e) => setTags(e.currentTarget.value)} placeholder="prod, banco" /></Field>
          <Field label="Keepalive (s)"><input type="number" value={d().keepalive_secs ?? ""} placeholder="30" onInput={(e) => set("keepalive_secs", e.currentTarget.value ? Number(e.currentTarget.value) : null)} /></Field>
        </div>
        <Field label="Notas"><textarea value={d().notes} spellcheck={true} onInput={(e) => set("notes", e.currentTarget.value)} style={{ "font-family": "var(--font)" }} /></Field>
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

function StartupSnippets(props: { ids: string[]; onChange: (ids: string[]) => void }) {
  const snippets = itemsOf("snippet");
  const label = (id: string) => snippets().find((x) => x.id === id)?.entity.data.label ?? "snippet removido";
  const move = (i: number, by: number) => {
    const ids = [...props.ids];
    [ids[i], ids[i + by]] = [ids[i + by], ids[i]];
    props.onChange(ids);
  };
  return (
    <div class="startup-list">
      <For each={props.ids}>
        {(id, i) => (
          <div class="startup-item">
            <span>{i() + 1}. {label(id)}</span>
            <button class="icon-btn" aria-label={`Subir ${label(id)}`} title="Subir" disabled={i() === 0} onClick={() => move(i(), -1)}><Icon name="arrow-up" size={14} /></button>
            <button class="icon-btn" aria-label={`Descer ${label(id)}`} title="Descer" disabled={i() === props.ids.length - 1} onClick={() => move(i(), 1)}><Icon name="arrow-down" size={14} /></button>
            <button class="icon-btn" aria-label={`Remover ${label(id)}`} title="Remover" onClick={() => props.onChange(props.ids.filter((_, j) => j !== i()))}><Icon name="x" size={14} /></button>
          </div>
        )}
      </For>
      <select value="" aria-label="Adicionar snippet ao conectar" onChange={(e) => { const v = e.currentTarget.value; if (v) props.onChange([...props.ids, v]); e.currentTarget.value = ""; }}>
        <option value="">{props.ids.length ? "Adicionar outro…" : "Nenhum (escolha para adicionar)"}</option>
        <For each={snippets().filter((x) => !props.ids.includes(x.id))}>{(x) => <option value={x.id}>{x.entity.data.label}</option>}</For>
      </select>
    </div>
  );
}
