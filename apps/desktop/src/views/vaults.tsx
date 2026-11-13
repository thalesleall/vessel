import { For, Show, createResource, createSignal } from "solid-js";
import { api, roleLabel, type InviteResponse, type Role, type VaultView } from "../api";
import { attempt, copy, items, refreshData, setSection, status, toast, tryAction, vaults } from "../store";
import { Badge, Button, Empty, Field, Icon, Modal, colorFor, confirmAction, initials } from "../ui";

export function VaultsView() {
  const [selected, setSelected] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [accepting, setAccepting] = createSignal(false);
  const count = (id: string) => items().filter((i) => i.vault_id === id).length;
  const vault = () => vaults().find((v) => v.id === selected()) ?? null;

  if (status()?.local) {
    return (
      <>
        <div class="page-head"><h1>Cofres e times</h1></div>
        <Empty icon="users" title="Compartilhar precisa de um servidor">
          <span>Neste computador tudo fica no seu cofre pessoal. Com um servidor conectado você cria cofres e divide hosts, chaves e snippets com um time.</span>
          <Button variant="primary" icon="cloud" onClick={() => setSection("settings")}>Conectar a um servidor</Button>
        </Empty>
      </>
    );
  }
  return (
    <>
      <div class="page-head">
        <h1>Cofres e times</h1>
        <div class="spacer" />
        <Button icon="users" onClick={() => setAccepting(true)}>Aceitar convite</Button>
        <Button variant="primary" icon="plus" onClick={() => setCreating(true)}>Novo cofre</Button>
      </div>
      <p class="subtitle">Cada cofre tem a própria chave. Compartilhe um cofre com seu time para dividir hosts, chaves e snippets.</p>

      <div class="grid">
        <For each={vaults()}>
          {(v) => (
            <div class="card" style={{ "border-color": selected() === v.id ? "var(--structure)" : undefined }}>
              <button class="card-hit" onClick={() => setSelected(v.id)} aria-label={`Abrir cofre ${v.name}`} aria-pressed={selected() === v.id} />
              <div class="avatar" style={{ background: v.color || colorFor(v.name) }}><Icon name={v.personal ? "lock" : "users"} size={18} /></div>
              <div class="meta">
                <div class="name">{v.name}</div>
                <div class="sub">{count(v.id)} itens, {v.personal ? "pessoal" : `${v.member_count} membro(s)`}</div>
              </div>
              <Badge tone={v.role === "owner" ? "blue" : "gray"}>{roleLabel[v.role]}</Badge>
              <Show when={v.rotating}><Badge tone="amber">trocando chave</Badge></Show>
            </div>
          )}
        </For>
      </div>

      <Show when={vault()}>{(v) => <VaultDetail vault={v()} onClosed={() => setSelected(null)} />}</Show>
      <Show when={creating()}><CreateVault onClose={() => setCreating(false)} /></Show>
      <Show when={accepting()}><AcceptInvite onClose={() => setAccepting(false)} /></Show>
    </>
  );
}

function VaultDetail(props: { vault: VaultView; onClosed: () => void }) {
  const [members, { refetch }] = createResource(() => props.vault.id, (id) => (props.vault.personal ? Promise.resolve(null) : api.members(id)));
  const [name, setName] = createSignal(props.vault.name);
  const [invite, setInvite] = createSignal<InviteResponse | null>(null);
  const [inviteRole, setInviteRole] = createSignal<Role>("editor");
  const [inviteEmail, setInviteEmail] = createSignal("");
  const manage = () => props.vault.role === "owner" || props.vault.role === "admin";

  const rename = async () => {
    await attempt(() => api.updateVault(props.vault.id, name(), props.vault.description, props.vault.color), "Cofre renomeado");
    refreshData();
  };
  const createInvite = async () => {
    const r = await attempt(() => api.invite(props.vault.id, inviteRole(), inviteEmail()));
    if (r) setInvite(r);
  };
  const approve = async (userId: string, email: string, fp: string) => {
    if (!(await confirmAction(`Aprovar ${email}?\n\nConfirme com a pessoa, por outro canal, que a impressão digital dela é:\n${fp}`, false))) return;
    await attempt(() => api.approveMember(props.vault.id, userId), `${email} agora tem acesso`);
    refetch();
  };
  const remove = async (userId: string, email: string) => {
    if (!(await confirmAction(`Remover ${email} do cofre?\nA chave do cofre será trocada e todos os itens recifrados.`))) return;
    await attempt(() => api.removeMember(props.vault.id, userId), "Membro removido e chave trocada");
    refetch();
    refreshData();
  };
  const leave = async () => {
    if (!(await confirmAction(`Sair do cofre "${props.vault.name}"? Você perde acesso aos itens dele.`))) return;
    if (await tryAction(() => api.leaveVault(props.vault.id), "Você saiu do cofre")) props.onClosed();
    refreshData();
  };
  const destroy = async () => {
    if (!(await confirmAction(`Excluir o cofre "${props.vault.name}" e TODOS os itens dele para todos os membros?\nIsso não pode ser desfeito.`))) return;
    if (await tryAction(() => api.deleteVault(props.vault.id), "Cofre excluído")) props.onClosed();
    refreshData();
  };

  return (
    <>
      <div class="section-title">{props.vault.name}</div>
      <div class="panel">
        <div class="form-row" style={{ "align-items": "end" }}>
          <Field label="Nome do cofre"><input value={name()} onInput={(e) => setName(e.currentTarget.value)} disabled={!manage()} /></Field>
          <div style={{ display: "flex", gap: "8px" }}>
            <Show when={manage()}><Button onClick={rename} disabled={name() === props.vault.name || !name().trim()}>Renomear</Button></Show>
            <Show when={manage() && !props.vault.personal}>
              <Button icon="sync" onClick={async () => { await attempt(() => api.rotateVault(props.vault.id), "Chave do cofre trocada"); refreshData(); }}>Trocar chave</Button>
            </Show>
          </div>
        </div>
      </div>

      <Show when={!props.vault.personal}>
        <Show when={members()}>
          {(m) => (
            <>
              <Show when={m().pending.length}>
                <div class="section-title"><Icon name="alert" size={14} />Aguardando aprovação</div>
                <div class="list">
                  <For each={m().pending}>
                    {(p) => (
                      <div class="row">
                        <div class="avatar" style={{ width: "30px", height: "30px", "border-radius": "8px", display: "grid", "place-items": "center", background: colorFor(p.email), "font-size": "12px" }}>{initials(p.email)}</div>
                        <div class="meta"><div class="name">{p.email} <Badge>{roleLabel[p.role]}</Badge></div><div class="sub fingerprint">{p.fingerprint}</div></div>
                        <Show when={manage()}><Button small variant="primary" icon="check" onClick={() => approve(p.user_id, p.email, p.fingerprint)}>Aprovar</Button></Show>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
              <div class="section-title"><Icon name="users" size={14} />Membros</div>
              <div class="list">
                <For each={m().members}>
                  {(mem) => (
                    <div class="row">
                      <div class="avatar" style={{ width: "30px", height: "30px", "border-radius": "8px", display: "grid", "place-items": "center", background: colorFor(mem.email), "font-size": "12px" }}>{initials(mem.email)}</div>
                      <div class="meta"><div class="name">{mem.email}{mem.is_me ? " (você)" : ""}</div><div class="sub fingerprint">{mem.fingerprint}</div></div>
                      <Show when={manage() && !mem.is_me && mem.role !== "owner"} fallback={<Badge tone={mem.role === "owner" ? "blue" : "gray"}>{roleLabel[mem.role]}</Badge>}>
                        <select style={{ width: "120px", height: "30px" }} value={mem.role}
                          onChange={async (e) => { await attempt(() => api.changeRole(props.vault.id, mem.user_id, e.currentTarget.value as Role), "Papel alterado"); refetch(); }}>
                          <option value="viewer">Leitor</option><option value="editor">Editor</option>
                          <Show when={props.vault.role === "owner"}><option value="admin">Admin</option></Show>
                        </select>
                        <Button small variant="danger" icon="trash" onClick={() => remove(mem.user_id, mem.email)} />
                      </Show>
                    </div>
                  )}
                </For>
              </div>
            </>
          )}
        </Show>

        <Show when={manage()}>
          <div class="section-title"><Icon name="plus" size={14} />Convidar</div>
          <div class="panel">
            <div class="form-row-3" style={{ "align-items": "end" }}>
              <Field label="E-mail (opcional)" hint="Restringe o convite a este endereço."><input type="email" value={inviteEmail()} onInput={(e) => setInviteEmail(e.currentTarget.value)} /></Field>
              <Field label="Papel">
                <select value={inviteRole()} onChange={(e) => setInviteRole(e.currentTarget.value as Role)}>
                  <option value="viewer">Leitor</option><option value="editor">Editor</option>
                  <Show when={props.vault.role === "owner"}><option value="admin">Admin</option></Show>
                </select>
              </Field>
              <Button variant="primary" onClick={createInvite}>Gerar convite</Button>
            </div>
            <Show when={invite()}>
              {(i) => (
                <div style={{ "margin-top": "14px" }} class="form">
                  <div class="code-block">{i().url}<button class="icon-btn" aria-label="Copiar" onClick={() => copy(i().url, "Link copiado")}><Icon name="copy" size={15} /></button></div>
                  <div class="field-hint">Válido por 7 dias. Depois que a pessoa aceitar, aprove-a aqui conferindo a impressão digital.</div>
                </div>
              )}
            </Show>
          </div>
        </Show>

        <div style={{ display: "flex", gap: "8px", "margin-top": "16px" }}>
          <Show when={props.vault.role !== "owner"}><Button variant="danger" icon="logout" onClick={leave}>Sair do cofre</Button></Show>
          <Show when={props.vault.role === "owner"}><Button variant="danger" icon="trash" onClick={destroy}>Excluir cofre</Button></Show>
        </div>
      </Show>
      <Show when={props.vault.personal}>
        <div class="panel" style={{ "margin-top": "14px" }}>
          <h3>Sua impressão digital</h3>
          <p>Leia para quem for aprovar você em um cofre compartilhado.</p>
          <div class="fingerprint" style={{ "font-size": "16px" }}>{status()?.fingerprint}</div>
        </div>
      </Show>
    </>
  );
}

function CreateVault(props: { onClose: () => void }) {
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const create = async () => {
    if (!name().trim()) return;
    setBusy(true);
    if (await attempt(() => api.createVault(name()), "Cofre criado") !== undefined) {
      await refreshData();
      props.onClose();
    }
    setBusy(false);
  };
  return (
    <Modal title="Novo cofre" onClose={props.onClose}
      footer={<><Button onClick={props.onClose}>Cancelar</Button><Button variant="primary" busy={busy()} onClick={create}>Criar</Button></>}>
      <Field label="Nome" hint="ex.: Infra, Clientes, Time de dados"><input value={name()} onInput={(e) => setName(e.currentTarget.value)} autofocus onKeyDown={(e) => e.key === "Enter" && create()} /></Field>
    </Modal>
  );
}

function AcceptInvite(props: { onClose: () => void }) {
  const [token, setToken] = createSignal("");
  const accept = async () => {
    const r = await attempt(() => api.acceptInvite(token()));
    if (r !== undefined) {
      toast("Convite aceito. Um administrador do cofre precisa aprovar você.", "success");
      props.onClose();
    }
  };
  return (
    <Modal title="Aceitar convite" onClose={props.onClose}
      footer={<><Button onClick={props.onClose}>Cancelar</Button><Button variant="primary" onClick={accept} disabled={!token().trim()}>Aceitar</Button></>}>
      <div class="form">
        <Field label="Link ou código do convite"><input class="mono" value={token()} onInput={(e) => setToken(e.currentTarget.value)} autofocus /></Field>
        <div class="callout callout-info">Depois de aceitar, informe sua impressão digital a quem convidou você:<br /><span class="fingerprint">{status()?.fingerprint}</span></div>
      </div>
    </Modal>
  );
}

export function EmptyVaults() {
  return <Empty icon="vault" title="Nenhum cofre" />;
}
