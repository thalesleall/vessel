import { Show, createSignal } from "solid-js";
import { api, errorCode, errorMessage, type MigrationOutcome, type ServerInfo } from "../api";
import { attempt, copy, refreshData, setStatus, status, toast } from "../store";
import { Button, Field, Icon, Modal, confirmAction } from "../ui";
import { RecoveryKeyBody } from "../auth";

/** Account panel: password, lock, and moving between local-only mode and a server. */
export function Account() {
  const [current, setCurrent] = createSignal("");
  const [next, setNext] = createSignal("");
  const [next2, setNext2] = createSignal("");
  const [connecting, setConnecting] = createSignal(false);
  const [outcome, setOutcome] = createSignal<{ result: MigrationOutcome; toLocal: boolean } | null>(null);
  const local = () => status()?.local ?? false;

  const change = async () => {
    if (next() !== next2()) return toast("As senhas novas não coincidem.", "error");
    if (next().length < 10) return toast("Use pelo menos 10 caracteres.", "error");
    const ok = await attempt(
      () => api.changePassword(current(), next()),
      local() ? "Senha alterada." : "Senha alterada. Os outros computadores precisam entrar de novo.",
    );
    if (ok !== undefined) { setCurrent(""); setNext(""); setNext2(""); }
  };

  const logout = async () => {
    const ok = await confirmAction(local()
      ? {
        title: "Apagar os dados deste computador?",
        message: "Hosts, chaves e snippets salvos aqui serão apagados. Não há servidor com uma cópia. Isso não pode ser desfeito.",
        confirm: "Apagar tudo",
      }
      : {
        title: "Sair desta conta?",
        message: "Os dados locais deste computador serão apagados. Eles continuam no servidor e voltam quando você entrar de novo.",
        confirm: "Sair",
      });
    if (ok) setStatus(await api.logout());
  };

  const disconnect = async () => {
    const ok = await confirmAction({
      title: "Parar de sincronizar?",
      message: "Este computador fica com uma cópia de tudo o que você vê hoje, incluindo os cofres compartilhados, e deixa de sincronizar.\n\nSua conta e os dados continuam no servidor. A senha e a chave de recuperação continuam as mesmas. Antes de mudar, o Vessel salva um backup criptografado.",
      confirm: "Parar de sincronizar",
      danger: false,
    });
    if (!ok) return;
    const result = await attempt(() => api.disconnectToLocal());
    if (result) {
      setOutcome({ result, toLocal: true });
      setStatus(result.status);
      refreshData();
    }
  };

  return (
    <div class="panel">
      <h3>{local() ? "Este computador" : "Conta"}</h3>
      <Show when={local()} fallback={
        <dl class="kv" style={{ margin: "10px 0 18px" }}>
          <dt>E-mail</dt><dd>{status()?.email}</dd>
          <dt>Servidor</dt><dd class="mono">{status()?.server_url}</dd>
          <dt>Impressão digital</dt><dd><span class="fingerprint">{status()?.fingerprint}</span>
            <button class="icon-btn" style={{ display: "inline-grid", "vertical-align": "middle" }} title="Copiar"
              onClick={() => copy(status()?.fingerprint ?? "")}><Icon name="copy" size={14} /></button></dd>
        </dl>
      }>
        <p>Seus dados ficam só aqui, criptografados com a sua senha. Conecte um servidor para ter o mesmo ambiente em outros computadores e compartilhar com um time.</p>
        <div style={{ display: "flex", gap: "8px", "margin-bottom": "18px" }}>
          <Button variant="primary" icon="cloud" onClick={() => setConnecting(true)}>Conectar a um servidor</Button>
        </div>
      </Show>

      <div class="section-title" style={{ "margin-top": "0" }}>Trocar senha</div>
      <div class="form-row-3" style={{ "align-items": "end" }}>
        <Field label="Senha atual"><input type="password" value={current()} onInput={(e) => setCurrent(e.currentTarget.value)} autocomplete="current-password" /></Field>
        <Field label="Nova senha"><input type="password" value={next()} onInput={(e) => setNext(e.currentTarget.value)} autocomplete="new-password" /></Field>
        <Field label="Confirmar"><input type="password" value={next2()} onInput={(e) => setNext2(e.currentTarget.value)} autocomplete="new-password" /></Field>
      </div>
      <div style={{ display: "flex", gap: "8px", "margin-top": "12px", "flex-wrap": "wrap" }}>
        <Button onClick={change} disabled={!current() || !next()}>Trocar senha</Button>
        <span style={{ flex: 1 }} />
        <Button icon="lock" onClick={async () => setStatus(await api.lock())}>Bloquear</Button>
        <Show when={!local()}><Button icon="unlink" onClick={disconnect}>Parar de sincronizar</Button></Show>
        <Button variant="danger" icon={local() ? "trash" : "logout"} onClick={logout}>{local() ? "Apagar dados" : "Sair"}</Button>
      </div>

      <Show when={connecting()}>
        <ConnectServer onClose={() => setConnecting(false)}
          onDone={(result) => { setConnecting(false); setOutcome({ result, toLocal: false }); setStatus(result.status); refreshData(); }} />
      </Show>
      <Show when={outcome()}>
        {(o) => <MigrationDone outcome={o().result} toLocal={o().toLocal} onClose={() => setOutcome(null)} />}
      </Show>
    </div>
  );
}

/** Local mode to a server: a new account or an existing one. Nothing is lost either way. */
function ConnectServer(props: { onClose: () => void; onDone: (o: MigrationOutcome) => void }) {
  const [url, setUrl] = createSignal("");
  const [info, setInfo] = createSignal<ServerInfo | null>(null);
  const [mode, setMode] = createSignal<"new" | "existing">("new");
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [password2, setPassword2] = createSignal("");
  const [invite, setInvite] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const check = async (e: Event) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      setInfo(await api.serverInfo(url()));
    } catch (err) {
      setError(`Não foi possível falar com o servidor: ${errorMessage(err)}`);
    } finally {
      setBusy(false);
    }
  };

  const submit = async (e: Event) => {
    e.preventDefault();
    setError(null);
    if (mode() === "new") {
      if (password().length < 10) return setError("Use uma senha com pelo menos 10 caracteres.");
      if (password() !== password2()) return setError("As senhas não coincidem.");
    }
    setBusy(true);
    try {
      props.onDone(mode() === "new"
        ? await api.connectNewAccount(url(), email(), password(), invite())
        : await api.connectExistingAccount(url(), email(), password()));
    } catch (err) {
      setError(errorCode(err) === "wrong_password" ? "E-mail ou senha incorretos." : errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="Conectar a um servidor" onClose={props.onClose}>
      <Show when={info()} fallback={
        <form class="form" onSubmit={check}>
          <ul class="explain">
            <li><Icon name="check" size={16} /><span>Tudo o que está neste computador vai junto: hosts, chaves, snippets e perfis.</span></li>
            <li><Icon name="shield" size={16} /><span>Antes de mudar, o Vessel salva um backup criptografado aqui.</span></li>
          </ul>
          <Field label="Endereço do servidor" hint="Ex.: https://vessel.minhaempresa.com">
            <input type="url" placeholder="https://" value={url()} onInput={(e) => setUrl(e.currentTarget.value)} autofocus />
          </Field>
          <Show when={error()}><div class="callout callout-danger"><Icon name="alert" size={16} />{error()}</div></Show>
          <Button type="submit" variant="primary" busy={busy()} disabled={!url().trim()}>Continuar</Button>
        </form>
      }>
        <form class="form" onSubmit={submit}>
          <div class="server-pill">
            <Icon name="server" size={15} />
            <span style={{ flex: 1 }}>{info()!.name} em {info()!.server_id}</span>
            <button type="button" class="icon-btn" title="Trocar servidor" onClick={() => setInfo(null)}><Icon name="edit" size={14} /></button>
          </div>
          <div class="auth-tabs" role="tablist" style={{ "margin-bottom": 0 }}>
            <button type="button" role="tab" classList={{ on: mode() === "new" }} onClick={() => setMode("new")}>Criar conta</button>
            <button type="button" role="tab" classList={{ on: mode() === "existing" }} onClick={() => setMode("existing")}>Já tenho conta</button>
          </div>
          <Field label="E-mail">
            <input type="email" value={email()} onInput={(e) => setEmail(e.currentTarget.value)} autocomplete="username" required />
          </Field>
          <Field label={mode() === "new" ? "Senha da conta" : "Senha"}
            hint={mode() === "new" ? "Pode ser a mesma que você já usa aqui. Ela passa a desbloquear o Vessel." : undefined}>
            <input type="password" value={password()} onInput={(e) => setPassword(e.currentTarget.value)}
              autocomplete={mode() === "new" ? "new-password" : "current-password"} required />
          </Field>
          <Show when={mode() === "new"}>
            <Field label="Confirme a senha">
              <input type="password" value={password2()} onInput={(e) => setPassword2(e.currentTarget.value)} autocomplete="new-password" required />
            </Field>
            <Show when={info()!.signups !== "open" && !info()!.first_user}>
              <Field label="Código de convite" hint="Este servidor só aceita contas novas com convite.">
                <input class="mono" value={invite()} onInput={(e) => setInvite(e.currentTarget.value.trim().split("/").pop() ?? "")} />
              </Field>
            </Show>
            <div class="callout callout-info"><Icon name="key" size={16} />
              <span>Você vai receber uma chave de recuperação nova. A atual deixa de valer.</span>
            </div>
          </Show>
          <Show when={mode() === "existing"}>
            <div class="callout callout-info"><Icon name="check" size={16} />
              <span>O que está aqui é somado ao que já existe na conta, sem duplicar itens idênticos. A senha e a chave de recuperação passam a ser as da conta.</span>
            </div>
          </Show>
          <Show when={error()}><div class="callout callout-danger"><Icon name="alert" size={16} />{error()}</div></Show>
          <Button type="submit" variant="primary" busy={busy()}>
            {mode() === "new" ? "Criar conta e enviar meus dados" : "Entrar e somar meus dados"}
          </Button>
        </form>
      </Show>
    </Modal>
  );
}

function MigrationDone(props: { outcome: MigrationOutcome; toLocal: boolean; onClose: () => void }) {
  const [saved, setSaved] = createSignal(!props.outcome.recovery_key);
  return (
    <Modal title={props.toLocal ? "Sincronização desligada" : "Servidor conectado"} onClose={() => saved() && props.onClose()}
      footer={<Button variant="primary" disabled={!saved()} onClick={props.onClose}>Concluir</Button>}>
      <dl class="report">
        <dt>{props.toLocal ? "Itens copiados para cá" : "Itens enviados"}</dt><dd>{props.outcome.imported}</dd>
        <Show when={props.outcome.skipped_duplicates > 0}>
          <dt>Já existiam</dt><dd>{props.outcome.skipped_duplicates} (não foram duplicados)</dd>
        </Show>
        <dt>Backup criptografado</dt>
        <dd class="mono selectable" style={{ "word-break": "break-all" }}>{props.outcome.backup_path}</dd>
      </dl>
      <Show when={props.outcome.recovery_key}>
        <div class="section-title" style={{ "margin-top": "8px" }}>Sua nova chave de recuperação</div>
        <RecoveryKeyBody value={props.outcome.recovery_key!} />
        <label class="check" style={{ "margin-top": "12px" }}>
          <input type="checkbox" checked={saved()} onChange={(e) => setSaved(e.currentTarget.checked)} />
          Guardei a chave nova. A antiga não funciona mais.
        </label>
      </Show>
    </Modal>
  );
}
