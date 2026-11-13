import { For, Show, createResource, createSignal } from "solid-js";
import { api, type Settings } from "../api";
import { attempt, copy, refreshData, setStatus, settings, settingsItem, status } from "../store";
import { Badge, Button, Field, Icon, confirmAction, timeAgo } from "../ui";
import { ItemSelect } from "./common";

export function SettingsView() {
  return (
    <>
      <div class="page-head"><h1>Configurações</h1></div>
      <Appearance />
      <Account />
      <Devices />
      <Conflicts />
      <Show when={status()?.is_server_admin}><ServerAdmin /></Show>
    </>
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
  return (
    <Show when={settings()}>
      {(s) => (
        <div class="panel">
          <h3>Terminal</h3>
          <p>Sincronizado entre seus computadores. Novos terminais usam estas opções.</p>
          <div class="form">
            <div class="form-row-3">
              <Field label="Fonte"><input value={s().font_family} onChange={(e) => update({ font_family: e.currentTarget.value })} /></Field>
              <Field label="Tamanho"><input type="number" value={s().font_size} onChange={(e) => update({ font_size: Number(e.currentTarget.value) })} /></Field>
              <Field label="Altura da linha"><input type="number" step="0.05" value={s().line_height} onChange={(e) => update({ line_height: Number(e.currentTarget.value) })} /></Field>
            </div>
            <div class="form-row-3">
              <Field label="Cursor">
                <select value={s().cursor_style} onChange={(e) => update({ cursor_style: e.currentTarget.value })}>
                  <option value="bar">Barra</option><option value="block">Bloco</option><option value="underline">Sublinhado</option>
                </select>
              </Field>
              <Field label="Histórico (linhas)"><input type="number" value={s().scrollback} onChange={(e) => update({ scrollback: Number(e.currentTarget.value) })} /></Field>
              <Field label="Bloquear após (min)" hint="0 = nunca"><input type="number" value={s().lock_after_minutes} onChange={(e) => update({ lock_after_minutes: Number(e.currentTarget.value) })} /></Field>
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

function Account() {
  const [current, setCurrent] = createSignal("");
  const [next, setNext] = createSignal("");
  const [next2, setNext2] = createSignal("");
  const change = async () => {
    if (next() !== next2()) return attempt(() => Promise.reject({ message: "As senhas novas não coincidem." }));
    if (next().length < 10) return attempt(() => Promise.reject({ message: "Use pelo menos 10 caracteres." }));
    const ok = await attempt(() => api.changePassword(current(), next()), "Senha mestre alterada. Outros dispositivos precisarão entrar de novo.");
    if (ok !== undefined) { setCurrent(""); setNext(""); setNext2(""); }
  };
  const logout = async () => {
    if (!(await confirmAction("Sair desta conta neste computador? Os dados locais serão apagados (continuam no servidor)."))) return;
    setStatus(await api.logout());
  };
  return (
    <div class="panel">
      <h3>Conta</h3>
      <dl class="kv" style={{ margin: "8px 0 16px" }}>
        <dt>E-mail</dt><dd>{status()?.email}</dd>
        <dt>Servidor</dt><dd class="mono">{status()?.server_url}</dd>
        <dt>Impressão digital</dt><dd><span class="fingerprint">{status()?.fingerprint}</span>
          <button class="icon-btn" style={{ display: "inline-grid", "vertical-align": "middle" }} onClick={() => copy(status()?.fingerprint ?? "")}><Icon name="copy" size={14} /></button></dd>
      </dl>
      <div class="section-title" style={{ "margin-top": "0" }}>Trocar senha mestre</div>
      <div class="form-row-3" style={{ "align-items": "end" }}>
        <Field label="Senha atual"><input type="password" value={current()} onInput={(e) => setCurrent(e.currentTarget.value)} /></Field>
        <Field label="Nova senha"><input type="password" value={next()} onInput={(e) => setNext(e.currentTarget.value)} /></Field>
        <Field label="Confirmar"><input type="password" value={next2()} onInput={(e) => setNext2(e.currentTarget.value)} /></Field>
      </div>
      <div style={{ display: "flex", gap: "8px", "margin-top": "12px" }}>
        <Button onClick={change} disabled={!current() || !next()}>Trocar senha</Button>
        <span style={{ flex: 1 }} />
        <Button icon="lock" onClick={async () => setStatus(await api.lock())}>Bloquear</Button>
        <Button variant="danger" icon="logout" onClick={logout}>Sair</Button>
      </div>
    </div>
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
      <div class="section-title"><Icon name="monitor" size={14} />Dispositivos</div>
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
      <div class="section-title"><Icon name="alert" size={14} />Conflitos de sincronização</div>
      <div class="list">
        <For each={conflicts() ?? []}>
          {(c) => (
            <div class="row">
              <Icon name="alert" />
              <div class="meta"><div class="name">{c.label}</div>
                <div class="sub">{c.conflicts.map((f) => `${f.field}: mantido ${JSON.stringify(f.local)}, descartado ${JSON.stringify(f.remote)}`).join(" · ")}</div></div>
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
      <div class="section-title"><Icon name="shield" size={14} />Administração do servidor</div>
      <div class="panel">
        <h3>Convidar para o servidor</h3>
        <p>Gera um convite de cadastro (para servidores com cadastro só por convite).</p>
        <div class="form-row" style={{ "align-items": "end" }}>
          <Field label="E-mail (opcional)"><input type="email" value={email()} onInput={(e) => setEmail(e.currentTarget.value)} /></Field>
          <Button variant="primary" onClick={async () => { const r = await attempt(() => api.signupInvite(email())); if (r) setLink(r.token); }}>Gerar convite</Button>
        </div>
        <Show when={link()}>
          <div class="code-block" style={{ "margin-top": "12px" }}>{link()}<button class="icon-btn" onClick={() => copy(link()!, "Código copiado")}><Icon name="copy" size={15} /></button></div>
        </Show>
      </div>
    </>
  );
}
