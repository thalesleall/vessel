import { For, Show, createSignal } from "solid-js";
import { blank, type Identity, type Item, type SshKey } from "../api";
import { copy, itemsOf } from "../store";
import { Badge, Button, Empty, Field, Icon, Modal } from "../ui";
import { EditorFooter, ItemSelect, VaultSelect, defaultVault, removeItem, saveEntity } from "./common";
import { invalid } from "../validate";

type IdentityItem = Item<{ type: "identity"; data: Identity }>;
type KeyItem = Item<{ type: "ssh_key"; data: SshKey }>;

export function KeychainView() {
  const identities = itemsOf("identity");
  const keys = itemsOf("ssh_key");
  const known = itemsOf("known_host");
  const [tab, setTab] = createSignal<"identities" | "keys" | "known">("identities");
  const [editIdentity, setEditIdentity] = createSignal<IdentityItem | "new" | null>(null);
  const [editKey, setEditKey] = createSignal<KeyItem | "new" | null>(null);

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
        <Show when={tab() === "keys"}><Button variant="primary" icon="plus" onClick={() => setEditKey("new")}>Nova chave</Button></Show>
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
          <div class="list">
            <For each={keys()}>
              {(k) => (
                <div class="row">
                  <Icon name="key" />
                  <div class="meta"><div class="name">{k.entity.data.label}</div>
                    <div class="sub mono">{k.entity.data.public_key ? k.entity.data.public_key.slice(0, 60) + "…" : "sem chave pública"}</div></div>
                  <Show when={k.entity.data.passphrase}><Badge tone="blue">com senha</Badge></Show>
                  <Show when={k.entity.data.public_key}>
                    <Button small icon="copy" onClick={() => copy(k.entity.data.public_key, "Chave pública copiada")}>Pública</Button>
                  </Show>
                  <Button small icon="edit" onClick={() => setEditKey(k)}>Editar</Button>
                </div>
              )}
            </For>
          </div>
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
      <Show when={editKey()} keyed>{(e) => <KeyEditor item={e === "new" ? null : (e as KeyItem)} onClose={() => setEditKey(null)} />}</Show>
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

function KeyEditor(props: { item: KeyItem | null; onClose: () => void }) {
  const [d, setD] = createSignal<SshKey>(props.item ? { ...props.item.entity.data } : blank.sshKey());
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const importFile = async (file: File | undefined, field: "private_key" | "public_key") => {
    if (!file) return;
    const text = await file.text();
    setD({ ...d(), [field]: text.trim(), label: d().label || file.name.replace(/\.(pub|pem|key)$/, "") });
  };
  const save = async () => {
    if (!d().private_key.trim()) return invalid("Chave privada", "Chave privada: não pode ficar em branco. Cole ou importe a chave.");
    if (!d().private_key.includes("PRIVATE KEY")) return invalid("Chave privada", "Chave privada inválida: cole a chave inteira, de -----BEGIN até -----END.");
    const data = { ...d(), label: d().label.trim() || "Chave", passphrase: d().passphrase || null };
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
