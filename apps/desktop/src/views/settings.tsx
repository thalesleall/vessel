import { For, Show, createResource, createSignal } from "solid-js";
import { api, type AdminUser, type BackgroundSettings, type Settings } from "../api";
import { attempt, checkForUpdate, copy, refreshData, settings, settingsItem, status, updateStatus } from "../store";
import { installUpdate } from "../update";
import { Badge, Button, Field, Icon, Modal, confirmAction, timeAgo } from "../ui";
import { ItemSelect } from "./common";
import { Account } from "./account";
import { ThemeSwitch } from "../auth";
import { invalid } from "../validate";

export function SettingsView() {
  return (
    <>
      <div class="page-head"><h1>Configurações</h1></div>
      <div class="panel">
        <h3>Aparência</h3>
        <p>Vale só para este computador.</p>
        <ThemeSwitch />
      </div>
      <Appearance />
      <Background />
      <Account />
      <Show when={!status()?.local}><Devices /></Show>
      <Conflicts />
      <Show when={status()?.is_server_admin}><ServerAdmin /></Show>
      <Updates />
    </>
  );
}

function Updates() {
  const [checking, setChecking] = createSignal(false);
  const check = async () => { setChecking(true); await checkForUpdate(true); setChecking(false); };
  return (
    <div class="panel">
      <h3>Atualizações</h3>
      <p>O Vessel procura versões novas ao abrir e a cada 10 minutos, e só instala quando você confirma. Cada atualização é assinada pelo projeto; uma assinatura que não confere é recusada.</p>
      <Show when={updateStatus()?.current}><p class="sub">Versão instalada: {updateStatus()!.current}</p></Show>
      <div style={{ display: "flex", gap: "8px", "margin-top": "8px" }}>
        <Button icon="sync" onClick={check} disabled={checking()}>{checking() ? "Procurando…" : "Procurar atualizações"}</Button>
        <Show when={updateStatus()?.available}>
          {(u) => <Button variant="primary" icon="download" onClick={installUpdate}>Instalar a versão {u().version}</Button>}
        </Show>
      </div>
    </div>
  );
}

function Background() {
  const [bg, { mutate }] = createResource(() => api.backgroundSettings());
  // On cancel or failure the box goes back to the stored value (the browser already flipped it).
  const change = async (patch: Partial<BackgroundSettings>, box?: HTMLInputElement) => {
    const revert = () => { if (box) box.checked = !box.checked; };
    const current = bg();
    if (!current) return revert();
    if (patch.keep_unlocked_for_schedules && !(await confirmAction({
      title: "Manter o cofre aberto?",
      message: "Enquanto houver agendamentos ativos, o Vessel não vai se bloquear sozinho por inatividade. Quem usar este computador nesse tempo terá acesso aos seus hosts, senhas e chaves.",
      confirm: "Manter aberto",
    }))) return revert();
    const next = await attempt(() => api.setBackgroundSettings({ ...current, ...patch }));
    if (next) mutate(next);
    else revert();
  };
  return (
    <Show when={bg()}>
      {(b) => (
        <div class="panel">
          <h3>Segundo plano</h3>
          <p>Snippets agendados só rodam com o Vessel aberto. Estas opções valem só para este computador.</p>
          <div class="form" style={{ gap: "10px" }}>
            <label class="check"><input type="checkbox" checked={b().close_to_tray} onChange={(e) => change({ close_to_tray: e.currentTarget.checked }, e.currentTarget)} />
              Fechar a janela deixa o Vessel na bandeja (para sair, use Sair no ícone da bandeja)</label>
            <label class="check"><input type="checkbox" checked={b().autostart} onChange={(e) => change({ autostart: e.currentTarget.checked }, e.currentTarget)} />
              Iniciar com o Windows, na bandeja</label>
            <label class="check"><input type="checkbox" checked={b().keep_unlocked_for_schedules} onChange={(e) => change({ keep_unlocked_for_schedules: e.currentTarget.checked }, e.currentTarget)} />
              Não bloquear por inatividade enquanto houver agendamentos ativos</label>
            <Show when={b().keep_unlocked_for_schedules}>
              <p class="field-hint" style={{ margin: 0, color: "var(--warn)" }}>O cofre fica aberto enquanto houver agendamentos. Bloqueie com Ctrl+L quando sair de perto.</p>
            </Show>
          </div>
        </div>
      )}
    </Show>
  );
}

function Appearance() {
  const update = async (patch: Partial<Settings>) => {
    const item = settingsItem();
    const current = settings();
    if (!item || !current) return;
    await attempt(() => api.saveItem(item.vault_id, item.id, { type: "settings", data: { ...current, ...patch } }));
    refreshData();
  };
  /** A number field saved on change: out of range, nothing is saved and the message stays until it is fixed. */
  const num = (key: "font_size" | "line_height" | "scrollback" | "lock_after_minutes", label: string, min: number, max: number, whole: boolean) =>
    (e: Event & { currentTarget: HTMLInputElement }) => {
      const el = e.currentTarget;
      const v = el.value.trim() === "" ? NaN : Number(el.value);
      if (!Number.isFinite(v) || v < min || v > max || (whole && !Number.isInteger(v))) {
        invalid(el, el.value.trim() === "" ? `${label}: não pode ficar em branco.` : `${label} inválido: use ${whole ? "um número inteiro" : "um número"} de ${min} a ${max}.`);
        return;
      }
      update({ [key]: v });
    };
  return (
    <Show when={settings()}>
      {(s) => (
        <div class="panel">
          <h3>Terminal</h3>
          <p>Novos terminais usam estas opções. Com um servidor conectado, elas valem em todos os seus computadores.</p>
          <div class="form">
            <div class="form-row-3">
              <Field label="Fonte"><input value={s().font_family} onChange={(e) => {
                if (e.currentTarget.value.trim()) return update({ font_family: e.currentTarget.value.trim() });
                invalid(e.currentTarget, "Fonte: não pode ficar em branco.");
              }} /></Field>
              <Field label="Tamanho"><input type="number" value={s().font_size} onChange={num("font_size", "Tamanho", 8, 40, true)} /></Field>
              <Field label="Altura da linha"><input type="number" step="0.05" value={s().line_height} onChange={num("line_height", "Altura da linha", 1, 2, false)} /></Field>
            </div>
            <div class="form-row-3">
              <Field label="Cursor">
                <select value={s().cursor_style} onChange={(e) => update({ cursor_style: e.currentTarget.value })}>
                  <option value="bar">Barra</option><option value="block">Bloco</option><option value="underline">Sublinhado</option>
                </select>
              </Field>
              <Field label="Histórico (linhas)"><input type="number" value={s().scrollback} onChange={num("scrollback", "Histórico", 0, 1000000, true)} /></Field>
              <Field label="Bloquear após (min)" hint="0 = nunca"><input type="number" value={s().lock_after_minutes} onChange={num("lock_after_minutes", "Bloquear após", 0, 1440, true)} /></Field>
            </div>
            <Field label="Perfil padrão (Ctrl+T)">
              <ItemSelect kind="terminal_profile" value={s().default_profile_id} onChange={(v) => update({ default_profile_id: v })} none="Primeiro da lista" />
            </Field>
            <label class="check"><input type="checkbox" checked={s().cursor_blink} onChange={(e) => update({ cursor_blink: e.currentTarget.checked })} />Cursor piscando</label>
            <label class="check"><input type="checkbox" checked={s().copy_on_select} onChange={(e) => update({ copy_on_select: e.currentTarget.checked })} />Copiar ao selecionar</label>
          </div>
        </div>
      )}
    </Show>
  );
}

function Devices() {
  const [devices, { refetch }] = createResource(() => api.devices().catch(() => []));
  const revoke = async (id: string, name: string) => {
    if (!(await confirmAction(`Desconectar "${name}"? O dispositivo apaga os dados locais na próxima conexão.`))) return;
    await attempt(() => api.revokeDevice(id), "Dispositivo desconectado");
    refetch();
  };
  return (
    <>
      <div class="section-title">Computadores conectados</div>
      <div class="list">
        <For each={devices() ?? []}>
          {(d) => (
            <div class="row">
              <Icon name="monitor" />
              <div class="meta"><div class="name">{d.name} <Show when={d.current}><Badge tone="green">este</Badge></Show></div>
                <div class="sub">visto {timeAgo(d.last_seen_at)}</div></div>
              <Show when={!d.current}><Button small variant="danger" onClick={() => revoke(d.id, d.name)}>Desconectar</Button></Show>
            </div>
          )}
        </For>
      </div>
    </>
  );
}

function Conflicts() {
  const [conflicts, { refetch }] = createResource(() => api.conflicts().catch(() => []));
  return (
    <Show when={(conflicts() ?? []).length}>
      <div class="section-title">Conflitos de sincronização</div>
      <div class="list">
        <For each={conflicts() ?? []}>
          {(c) => (
            <div class="row">
              <Icon name="alert" />
              <div class="meta"><div class="name">{c.label}</div>
                <div class="sub">{c.conflicts.map((f) => `${f.field}: mantido ${JSON.stringify(f.local)}, descartado ${JSON.stringify(f.remote)}`).join(", ")}</div></div>
              <Button small onClick={async () => { await api.dismissConflict(c.id); refetch(); }}>Dispensar</Button>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

function ServerAdmin() {
  const [email, setEmail] = createSignal("");
  const [link, setLink] = createSignal<string | null>(null);
  return (
    <>
      <div class="section-title">Administração do servidor</div>
      <div class="panel">
        <h3>Convidar para o servidor</h3>
        <p>Gera um convite de cadastro (para servidores com cadastro só por convite).</p>
        <div class="form-row" style={{ "align-items": "end" }}>
          <Field label="E-mail (opcional)"><input type="email" value={email()} onInput={(e) => setEmail(e.currentTarget.value)} /></Field>
          <Button variant="primary" onClick={async () => { const r = await attempt(() => api.signupInvite(email())); if (r) setLink(r.token); }}>Gerar convite</Button>
        </div>
        <Show when={link()}>
          <div class="code-block" style={{ "margin-top": "12px" }}>{link()}<button class="icon-btn" aria-label="Copiar" onClick={() => copy(link()!, "Código copiado")}><Icon name="copy" size={15} /></button></div>
        </Show>
      </div>
      <ServerUsage />
      <ServerAccounts />
    </>
  );
}

const MIB = 1024 * 1024;
function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < MIB) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 * MIB) return `${(n / MIB).toFixed(1)} MiB`;
  return `${(n / (1024 * MIB)).toFixed(2)} GiB`;
}

function ServerUsage() {
  const [stats] = createResource(() => api.adminStats().catch(() => null));
  return (
    <Show when={stats()}>
      {(s) => (
        <div class="panel">
          <h3>Uso do servidor</h3>
          <dl class="kv stats-grid">
            <dt>Contas</dt><dd>{s().users}</dd>
            <dt>Ativas em 30 dias</dt><dd>{s().active_users_30d}</dd>
            <dt>Suspensas</dt><dd>{s().suspended_users}</dd>
            <dt>Cofres</dt><dd>{s().vaults}</dd>
            <dt>Itens</dt><dd>{s().records}</dd>
            <dt>Armazenamento</dt><dd>{bytes(s().bytes)}</dd>
            <dt>Sessões</dt><dd>{s().sessions}</dd>
            <dt>Cota padrão</dt><dd>{s().default_quota_bytes ? bytes(s().default_quota_bytes) : "Sem limite"}</dd>
          </dl>
        </div>
      )}
    </Show>
  );
}

function ServerAccounts() {
  const [users, { refetch }] = createResource(() => api.adminUsers());
  const me = () => status()?.user_id;
  const [quotaFor, setQuotaFor] = createSignal<AdminUser | null>(null);
  const [quotaText, setQuotaText] = createSignal("");
  const update = async (u: AdminUser, patch: Parameters<typeof api.adminUpdateUser>[1], done: string) => {
    if (await attempt(async () => { await api.adminUpdateUser(u.user_id, patch); return true; }, done)) refetch();
  };
  const suspend = async (u: AdminUser) => {
    if (!u.suspended && !(await confirmAction({
      title: `Suspender ${u.email}?`,
      message: "A conta sai de todos os dispositivos e não consegue entrar até você reativá-la. Os dados continuam guardados.",
      confirm: "Suspender",
    }))) return;
    update(u, { suspended: !u.suspended }, u.suspended ? `${u.email} reativada` : `${u.email} suspensa`);
  };
  const toggleAdmin = async (u: AdminUser) => {
    if (!u.is_admin && !(await confirmAction({
      title: `Tornar ${u.email} administradora do servidor?`,
      message: "Administradores veem todas as contas, suspendem, excluem e definem cotas. Não veem o conteúdo de nenhum cofre.",
      confirm: "Tornar administradora",
      danger: false,
    }))) return;
    update(u, { is_admin: !u.is_admin }, u.is_admin ? "Administração removida" : "Administração concedida");
  };
  const quota = (u: AdminUser) => {
    setQuotaText(u.quota_bytes === null ? "" : String(Math.round(u.quota_bytes / MIB)));
    setQuotaFor(u);
  };
  const quotaValid = () => /^\d*$/.test(quotaText().trim());
  const saveQuota = async () => {
    const u = quotaFor();
    if (!u) return;
    if (!quotaValid()) return invalid("Espaço em MiB", "Cota inválida: use um número inteiro de MiB (0 é sem limite), ou deixe em branco.");
    const t = quotaText().trim();
    setQuotaFor(null);
    await update(u, { quota_bytes: t === "" ? null : Number(t) * MIB }, "Cota atualizada");
  };
  const remove = async (u: AdminUser) => {
    if (!(await confirmAction({
      title: `Excluir a conta ${u.email}?`,
      message: "Os cofres dessa conta são apagados, com tudo o que há neles, inclusive para quem os compartilhava. Os dispositivos saem e o e-mail fica livre. Não dá para desfazer.",
      expect: u.email,
      expectLabel: "Digite o e-mail da conta para confirmar",
      confirm: "Excluir conta",
    }))) return;
    if (await attempt(async () => { await api.adminDeleteUser(u.user_id); return true; }, `${u.email} excluída`)) refetch();
  };
  return (
    <div class="panel">
      <h3>Contas</h3>
      <p>Você não vê o conteúdo dos cofres: só o que o servidor sabe sobre cada conta.</p>
      <Show when={quotaFor()}>
        {(u) => (
          <Modal title={`Cota de ${u().email}`} onClose={() => setQuotaFor(null)}
            footer={<>
              <Button onClick={() => setQuotaFor(null)}>Cancelar</Button>
              <Button variant="primary" onClick={saveQuota}>Salvar</Button>
            </>}>
            <form class="form" onSubmit={(e) => { e.preventDefault(); saveQuota(); }}>
              <Field label="Espaço em MiB" hint="Vazio usa o padrão do servidor. 0 é sem limite. Conta o que há nos cofres de que a conta é dona.">
                <input inputmode="numeric" value={quotaText()} onInput={(e) => setQuotaText(e.currentTarget.value)} autofocus />
              </Field>
            </form>
          </Modal>
        )}
      </Show>
      <div class="list admin-users">
        <For each={users() ?? []}>
          {(u) => (
            <div class="row">
              <div class="meta">
                <div class="name">{u.email} {u.user_id === me() ? "(você)" : ""}
                  <Show when={u.is_admin}> <Badge tone="blue">Admin</Badge></Show>
                  <Show when={u.suspended}> <Badge tone="red">Suspensa</Badge></Show>
                </div>
                <div class="sub">
                  {bytes(u.bytes_used)}{u.quota_bytes !== null ? ` de ${u.quota_bytes ? bytes(u.quota_bytes) : "sem limite"}` : ""}
                  {" · "}{u.vaults_owned} cofres · {u.devices} dispositivos
                  {u.last_seen_at ? ` · visto ${timeAgo(u.last_seen_at)}` : ""}
                </div>
              </div>
              <div class="actions">
                <Button small onClick={() => quota(u)}>Cota</Button>
                <Button small onClick={() => toggleAdmin(u)}>{u.is_admin ? "Remover admin" : "Tornar admin"}</Button>
                <Show when={u.user_id !== me()}>
                  <Button small variant={u.suspended ? "primary" : "subtle"} onClick={() => suspend(u)}>{u.suspended ? "Reativar" : "Suspender"}</Button>
                  <Button small variant="danger" icon="trash" title={`Excluir ${u.email}`} onClick={() => remove(u)} />
                </Show>
              </div>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
