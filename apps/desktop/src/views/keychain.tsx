import { For, Show, createMemo, createSignal } from "solid-js";
import { api, blank, type Identity, type Item, type SshKey } from "../api";
import { copy, itemsOf, refreshData, tryAction } from "../store";
import { Badge, Button, Empty, Field, Icon, Modal, confirmAction } from "../ui";
import { EditorFooter, ItemSelect, VaultSelect, defaultVault, removeItem, saveEntity } from "./common";
import { invalid } from "../validate";

type IdentityItem = Item<{ type: "identity"; data: Identity }>;
type KeyItem = Item<{ type: "ssh_key"; data: SshKey }>;

const folderOf = (k: KeyItem) => (k.entity.data.folder ?? "").trim();
const byLabel = (a: KeyItem, b: KeyItem) => a.entity.data.label.localeCompare(b.entity.data.label);

/** Moves keys to folders (by key id) in one go; "" takes a key out of its folder. */
async function fileKeys(keys: KeyItem[], target: Map<string, string>, done: string) {
  const changed = keys.filter((k) => target.has(k.id) && target.get(k.id) !== folderOf(k));
  if (!changed.length) return true;
  const ok = await tryAction(async () => {
    for (const k of changed) {
      await api.saveItem(k.vault_id, k.id, { type: "ssh_key", data: { ...k.entity.data, folder: target.get(k.id)! } });
    }
  }, done);
  await refreshData();
  return ok;
}

export function KeychainView() {
  const identities = itemsOf("identity");
  const keys = itemsOf("ssh_key");
  const known = itemsOf("known_host");
  const [tab, setTab] = createSignal<"identities" | "keys" | "known">("identities");
  const [editIdentity, setEditIdentity] = createSignal<IdentityItem | "new" | null>(null);
  const [editKey, setEditKey] = createSignal<KeyItem | "new" | null>(null);
  /** null: every key; "": keys without a folder; otherwise one folder. */
  const [folder, setFolder] = createSignal<string | null>(null);
  const [editFolder, setEditFolder] = createSignal<string | "new" | null>(null);
  const folders = createMemo(() => {
    const counts = new Map<string, number>();
    for (const k of keys()) if (folderOf(k)) counts.set(folderOf(k), (counts.get(folderOf(k)) ?? 0) + 1);
    return [...counts.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  });
  const loose = createMemo(() => keys().filter((k) => !folderOf(k)).sort(byLabel));
  const inFolder = (f: string) => keys().filter((k) => folderOf(k) === f).sort(byLabel);
  // A folder that was renamed away or emptied is no longer selected.
  const current = () => (folder() && !folders().some(([f]) => f === folder()) ? null : folder());

  return (
    <>
      <div class="page-head">
        <h1>Chaves</h1>
        <div class="segmented">
          <button classList={{ on: tab() === "identities" }} onClick={() => setTab("identities")}>Identidades</button>
          <button classList={{ on: tab() === "keys" }} onClick={() => setTab("keys")}>Chaves SSH</button>
          <button classList={{ on: tab() === "known" }} onClick={() => setTab("known")}>Hosts conhecidos</button>
        </div>
        <div class="spacer" />
        <Show when={tab() === "identities"}><Button variant="primary" icon="plus" onClick={() => setEditIdentity("new")}>Nova identidade</Button></Show>
        <Show when={tab() === "keys"}>
          <Button icon="folder" onClick={() => setEditFolder("new")} disabled={!keys().length} title={keys().length ? "Agrupar chaves numa pasta" : "Cadastre uma chave antes"}>Nova pasta</Button>
          <Button variant="primary" icon="plus" onClick={() => setEditKey("new")}>Nova chave</Button>
        </Show>
      </div>
      <p class="subtitle">Credenciais são cifradas no seu computador antes de sincronizar, e nunca ficam visíveis para agentes de IA.</p>

      <Show when={tab() === "identities"}>
        <Show when={identities().length} fallback={<Empty icon="user" title="Nenhuma identidade"><span>Uma identidade reúne usuário, senha e chave para reutilizar em vários hosts.</span></Empty>}>
          <div class="list">
            <For each={identities()}>
              {(i) => (
                <div class="row" onDblClick={() => setEditIdentity(i)}>
                  <Icon name="user" />
                  <div class="meta"><div class="name">{i.entity.data.label || i.entity.data.username}</div>
                    <div class="sub">{i.entity.data.username}{i.entity.data.password ? ", senha" : ""}{i.entity.data.ssh_key_id ? ", chave" : ""}</div></div>
                  <Button small icon="edit" onClick={() => setEditIdentity(i)}>Editar</Button>
                </div>
              )}
            </For>
          </div>
        </Show>
      </Show>

      <Show when={tab() === "keys"}>
        <Show when={keys().length} fallback={<Empty icon="key" title="Nenhuma chave SSH"><span>Importe uma chave privada (OpenSSH ou PEM).</span></Empty>}>
          <Show when={folders().length > 0}>
            <div class="chips" role="group" aria-label="Pastas">
              <button class="chip" classList={{ on: current() === null }} aria-pressed={current() === null} onClick={() => setFolder(null)}>
                Todas<span class="chip-count">{keys().length}</span>
              </button>
              <For each={folders()}>
                {([f, n]) => (
                  <button class="chip" classList={{ on: current() === f }} aria-pressed={current() === f} onClick={() => setFolder(f)}
                    onDblClick={() => setEditFolder(f)} onKeyDown={(e) => e.key === "F2" && setEditFolder(f)} title="Duplo clique ou F2 para editar">
                    <Icon name="folder" size={13} />{f}<span class="chip-count">{n}</span>
                  </button>
                )}
              </For>
              <Show when={loose().length}>
                <button class="chip" classList={{ on: current() === "" }} aria-pressed={current() === ""} onClick={() => setFolder("")}>
                  Sem pasta<span class="chip-count">{loose().length}</span>
                </button>
              </Show>
              <Show when={current()}>
                {(f) => <button class="icon-btn" aria-label={`Editar a pasta ${f()}`} title="Editar pasta" onClick={() => setEditFolder(f())}><Icon name="edit" size={14} /></button>}
              </Show>
            </div>
          </Show>
          <Show when={current() === null && folders().length > 0} fallback={
            <div class="list">
              <For each={current() === null ? [...keys()].sort(byLabel) : current() === "" ? loose() : inFolder(current()!)}>
                {(k) => <KeyRow item={k} onEdit={() => setEditKey(k)} />}
              </For>
            </div>
          }>
            <For each={folders()}>
              {([f, n]) => (
                <section class="key-folder" aria-label={`Pasta ${f}`}>
                  <div class="section-title key-folder-title">
                    <Icon name="folder" size={14} /><button class="link-like" onClick={() => setFolder(f)} title="Mostrar só esta pasta">{f}</button>
                    <span class="chip-count">{n}</span>
                    <button class="icon-btn" aria-label={`Editar a pasta ${f}`} title="Editar pasta" onClick={() => setEditFolder(f)}><Icon name="edit" size={13} /></button>
                  </div>
                  <div class="list"><For each={inFolder(f)}>{(k) => <KeyRow item={k} onEdit={() => setEditKey(k)} />}</For></div>
                </section>
              )}
            </For>
            <Show when={loose().length}>
              <section class="key-folder" aria-label="Sem pasta">
                <div class="section-title key-folder-title"><span>Sem pasta</span><span class="chip-count">{loose().length}</span></div>
                <div class="list"><For each={loose()}>{(k) => <KeyRow item={k} onEdit={() => setEditKey(k)} />}</For></div>
              </section>
            </Show>
          </Show>
        </Show>
      </Show>

      <Show when={tab() === "known"}>
        <Show when={known().length} fallback={<Empty icon="shield" title="Nenhum host conhecido"><span>Servidores confirmados na primeira conexão aparecem aqui.</span></Empty>}>
          <div class="list">
            <For each={known()}>
              {(k) => (
                <div class="row">
                  <Icon name="shield" />
                  <div class="meta"><div class="name">{k.entity.data.host}</div><div class="sub mono">{k.entity.data.key_type}, {k.entity.data.fingerprint}</div></div>
                  <Button small variant="danger" icon="trash" onClick={() => removeItem(k, "o host conhecido")}>Esquecer</Button>
                </div>
              )}
            </For>
          </div>
        </Show>
      </Show>

      <Show when={editIdentity()} keyed>{(e) => <IdentityEditor item={e === "new" ? null : (e as IdentityItem)} onClose={() => setEditIdentity(null)} />}</Show>
      <Show when={editKey()} keyed>{(e) => (
        <KeyEditor item={e === "new" ? null : (e as KeyItem)} folders={folders().map(([f]) => f)}
          folder={e === "new" ? current() ?? "" : undefined} onClose={() => setEditKey(null)} />
      )}</Show>
      <Show when={editFolder()} keyed>{(f) => (
        <FolderEditor name={f === "new" ? null : f} keys={keys()} folders={folders().map(([x]) => x)}
          onSaved={(name) => { if (name !== null) setFolder(name); else if (current() === f) setFolder(null); }}
          onClose={() => setEditFolder(null)} />
      )}</Show>
    </>
  );
}

function IdentityEditor(props: { item: IdentityItem | null; onClose: () => void }) {
  const [d, setD] = createSignal<Identity>(props.item ? { ...props.item.entity.data } : blank.identity());
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const save = async () => {
    if (!d().username.trim()) return invalid("Usuário", "Usuário: não pode ficar em branco.");
    const data = { ...d(), label: d().label.trim() || d().username, password: d().password || null };
    if (await saveEntity(props.item, vault(), { type: "identity", data })) props.onClose();
  };
  return (
    <Modal title={props.item ? "Editar identidade" : "Nova identidade"} onClose={props.onClose}
      footer={<EditorFooter onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "a identidade"); props.onClose(); } : undefined} />}>
      <div class="form">
        <Field label="Nome"><input value={d().label} onInput={(e) => setD({ ...d(), label: e.currentTarget.value })} placeholder="ex.: deploy@prod" autofocus /></Field>
        <Field label="Usuário"><input value={d().username} onInput={(e) => setD({ ...d(), username: e.currentTarget.value })} /></Field>
        <Field label="Senha"><input type="password" value={d().password ?? ""} onInput={(e) => setD({ ...d(), password: e.currentTarget.value })} autocomplete="new-password" /></Field>
        <Field label="Chave SSH"><ItemSelect kind="ssh_key" value={d().ssh_key_id} onChange={(v) => setD({ ...d(), ssh_key_id: v })} /></Field>
        <Field label="Cofre"><VaultSelect value={vault()} onChange={setVault} /></Field>
      </div>
    </Modal>
  );
}

function KeyRow(props: { item: KeyItem; onEdit: () => void }) {
  const d = () => props.item.entity.data;
  return (
    <div class="row" onDblClick={props.onEdit}>
      <Icon name="key" />
      <div class="meta"><div class="name">{d().label}</div>
        <div class="sub mono">{d().public_key ? d().public_key.slice(0, 60) + "…" : "sem chave pública"}</div></div>
      <Show when={d().passphrase}><Badge tone="blue">com senha</Badge></Show>
      <Show when={d().public_key}>
        <Button small icon="copy" onClick={() => copy(d().public_key, "Chave pública copiada")}>Pública</Button>
      </Show>
      <Button small icon="edit" onClick={props.onEdit}>Editar</Button>
    </div>
  );
}

/** Creates, renames or removes a folder, and says which keys are in it. A folder exists while it
 * has keys: removing it leaves its keys without a folder, never deletes them. */
function FolderEditor(props: { name: string | null; keys: KeyItem[]; folders: string[]; onSaved: (name: string | null) => void; onClose: () => void }) {
  const [name, setName] = createSignal(props.name ?? "");
  const [picked, setPicked] = createSignal(new Set(props.keys.filter((k) => props.name !== null && folderOf(k) === props.name).map((k) => k.id)));
  const [filter, setFilter] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const sorted = [...props.keys].sort((a, b) => {
    // Keys of this folder first, then those without a folder, then the rest.
    const rank = (k: KeyItem) => (props.name !== null && folderOf(k) === props.name ? 0 : folderOf(k) ? 2 : 1);
    return rank(a) - rank(b) || byLabel(a, b);
  });
  const shown = () => sorted.filter((k) => !filter() || k.entity.data.label.toLowerCase().includes(filter().toLowerCase()));
  const toggle = (id: string, on: boolean) => {
    const next = new Set(picked());
    if (on) next.add(id); else next.delete(id);
    setPicked(next);
  };
  const save = async () => {
    const n = name().trim();
    if (!n) return invalid("Nome", "Nome da pasta: não pode ficar em branco.");
    if (n.length > 60) return invalid("Nome", "Nome da pasta: use até 60 caracteres.");
    if (picked().size === 0) return invalid("Chaves", "Marque pelo menos uma chave. Uma pasta existe enquanto tiver chaves.");
    if (n !== props.name && props.folders.includes(n) && !(await confirmAction({
      title: `Juntar com a pasta "${n}"?`,
      message: `Já existe uma pasta "${n}". As chaves marcadas vão para ela.`,
      confirm: "Juntar",
      danger: false,
    }))) return;
    const target = new Map<string, string>();
    for (const k of props.keys) {
      if (picked().has(k.id)) target.set(k.id, n);
      else if (props.name !== null && folderOf(k) === props.name) target.set(k.id, "");
    }
    setBusy(true);
    const ok = await fileKeys(props.keys, target, props.name === null ? "Pasta criada" : "Pasta salva");
    setBusy(false);
    if (ok) { props.onSaved(n); props.onClose(); }
  };
  const remove = async () => {
    const members = props.keys.filter((k) => folderOf(k) === props.name);
    if (!(await confirmAction({
      title: `Excluir a pasta "${props.name}"?`,
      message: `${members.length === 1 ? "A chave continua salva" : `As ${members.length} chaves continuam salvas`}, só que sem pasta.`,
      confirm: "Excluir pasta",
    }))) return;
    const ok = await fileKeys(props.keys, new Map(members.map((k) => [k.id, ""])), "Pasta excluída");
    if (ok) { props.onSaved(null); props.onClose(); }
  };
  return (
    <Modal title={props.name === null ? "Nova pasta" : "Editar pasta"} onClose={props.onClose}
      footer={<>
        <Show when={props.name !== null}><Button variant="danger" icon="trash" onClick={() => void remove()}>Excluir pasta</Button></Show>
        <span style={{ flex: 1 }} />
        <Button onClick={props.onClose}>Cancelar</Button>
        <Button variant="primary" busy={busy()} onClick={() => void save()}>Salvar</Button>
      </>}>
      <div class="form">
        <Field label="Nome"><input value={name()} maxLength={60} onInput={(e) => setName(e.currentTarget.value)} placeholder="ex.: Clientes, Produção" autofocus /></Field>
        <Field label={`Chaves (${picked().size} marcadas)`} hint="Uma chave fica numa pasta só: marcar aqui tira a chave da pasta em que estava.">
          <Show when={props.keys.length > 8}><input placeholder="Filtrar chaves…" aria-label="Filtrar chaves" value={filter()} onInput={(e) => setFilter(e.currentTarget.value)} /></Show>
          <div class="host-picks">
            <For each={shown()}>
              {(k) => (
                <label class="check">
                  <input type="checkbox" checked={picked().has(k.id)} onChange={(e) => toggle(k.id, e.currentTarget.checked)} />
                  <span>{k.entity.data.label}</span>
                  <Show when={folderOf(k) && folderOf(k) !== props.name}><span class="field-hint">em {folderOf(k)}</span></Show>
                </label>
              )}
            </For>
          </div>
        </Field>
      </div>
    </Modal>
  );
}

function KeyEditor(props: { item: KeyItem | null; folders: string[]; folder?: string; onClose: () => void }) {
  const [d, setD] = createSignal<SshKey>(props.item ? { ...props.item.entity.data } : { ...blank.sshKey(), folder: props.folder ?? "" });
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const importFile = async (file: File | undefined, field: "private_key" | "public_key") => {
    if (!file) return;
    const text = await file.text();
    setD({ ...d(), [field]: text.trim(), label: d().label || file.name.replace(/\.(pub|pem|key)$/, "") });
  };
  const save = async () => {
    if (!d().private_key.trim()) return invalid("Chave privada", "Chave privada: não pode ficar em branco. Cole ou importe a chave.");
    if (!d().private_key.includes("PRIVATE KEY")) return invalid("Chave privada", "Chave privada inválida: cole a chave inteira, de -----BEGIN até -----END.");
    if ((d().folder ?? "").trim().length > 60) return invalid("Pasta", "Pasta: use até 60 caracteres.");
    const data = { ...d(), label: d().label.trim() || "Chave", passphrase: d().passphrase || null, folder: (d().folder ?? "").trim() };
    if (await saveEntity(props.item, vault(), { type: "ssh_key", data })) props.onClose();
  };
  return (
    <Modal title={props.item ? "Editar chave SSH" : "Nova chave SSH"} onClose={props.onClose} wide
      footer={<EditorFooter onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "a chave"); props.onClose(); } : undefined} />}>
      <div class="form">
        <div class="form-row">
          <Field label="Nome"><input value={d().label} onInput={(e) => setD({ ...d(), label: e.currentTarget.value })} autofocus /></Field>
          <Field label="Cofre"><VaultSelect value={vault()} onChange={setVault} /></Field>
        </div>
        <Field label="Pasta" hint="Escolha uma existente ou digite um nome novo. Vazio deixa a chave sem pasta.">
          <input list="key-folders" value={d().folder ?? ""} maxLength={60} onInput={(e) => setD({ ...d(), folder: e.currentTarget.value })} placeholder="Sem pasta" />
          <datalist id="key-folders"><For each={props.folders}>{(f) => <option value={f} />}</For></datalist>
        </Field>
        <Field label="Chave privada">
          <textarea rows={7} value={d().private_key} onInput={(e) => setD({ ...d(), private_key: e.currentTarget.value })} placeholder="-----BEGIN OPENSSH PRIVATE KEY-----" />
        </Field>
        <label class="btn btn-sm" style={{ "align-self": "flex-start" }}>
          <Icon name="file" size={15} />Importar arquivo (ex.: ~/.ssh/id_ed25519)
          <input type="file" style={{ display: "none" }} onChange={(e) => importFile(e.currentTarget.files?.[0], "private_key")} />
        </label>
        <Field label="Senha da chave (passphrase)"><input type="password" value={d().passphrase ?? ""} onInput={(e) => setD({ ...d(), passphrase: e.currentTarget.value })} autocomplete="new-password" /></Field>
        <Field label="Chave pública" hint="Opcional; usada para copiar para authorized_keys.">
          <textarea rows={2} value={d().public_key} onInput={(e) => setD({ ...d(), public_key: e.currentTarget.value })} placeholder="ssh-ed25519 AAAA…" />
        </Field>
      </div>
    </Modal>
  );
}
