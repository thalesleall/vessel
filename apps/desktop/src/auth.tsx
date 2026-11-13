import { Show, createSignal, onMount } from "solid-js";
import { api, errorCode, errorMessage, type ServerInfo } from "./api";
import { copy, refreshStatus, setStatus, status, toast } from "./store";
import { Button, Field, Icon } from "./ui";

function Brand() {
  return (
    <div class="brand">
      <div class="brand-logo"><Icon name="terminal" size={17} /></div>
      <div class="brand-name">Vessel</div>
    </div>
  );
}

type Mode = "login" | "signup" | "recover";

export function Connect() {
  const [mode, setMode] = createSignal<Mode>("login");
  const [url, setUrl] = createSignal("");
  const [info, setInfo] = createSignal<ServerInfo | null>(null);
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [password2, setPassword2] = createSignal("");
  const [invite, setInvite] = createSignal("");
  const [recoveryKey, setRecoveryKey] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [newRecovery, setNewRecovery] = createSignal<string | null>(null);

  onMount(async () => {
    const s = await api.localSettings();
    if (s.last_server_url) setUrl(s.last_server_url);
    if (s.last_email) setEmail(s.last_email);
  });

  const checkServer = async () => {
    setError(null);
    setBusy(true);
    try {
      const i = await api.serverInfo(url());
      setInfo(i);
      if (i.first_user) setMode("signup");
    } catch (e) {
      setError(`Não foi possível falar com o servidor: ${errorMessage(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const submit = async (e: Event) => {
    e.preventDefault();
    setError(null);
    if ((mode() === "signup" || mode() === "recover") && password() !== password2()) {
      setError("As senhas não coincidem.");
      return;
    }
    if ((mode() === "signup" || mode() === "recover") && password().length < 10) {
      setError("Use uma senha mestre com pelo menos 10 caracteres.");
      return;
    }
    setBusy(true);
    try {
      if (mode() === "login") {
        setStatus(await api.login(url(), email(), password()));
      } else if (mode() === "signup") {
        const key = await api.signup(url(), email(), password(), invite());
        setNewRecovery(key);
      } else {
        setStatus(await api.recover(url(), email(), recoveryKey(), password()));
        toast("Senha redefinida. Os outros dispositivos foram desconectados.", "success");
      }
    } catch (e) {
      setError(errorCode(e) === "wrong_password" ? "E-mail ou senha incorretos." : errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="auth">
      <Show when={!newRecovery()} fallback={<RecoveryKeyScreen value={newRecovery()!} onDone={() => refreshStatus()} />}>
        <div class="auth-card">
          <Brand />
          <h1>Conecte ao seu servidor</h1>
          <p class="lead">Seus terminais ficam aqui. Hosts, chaves e snippets sincronizam, criptografados, pelo seu agente Vessel.</p>

          <Show when={info()} fallback={
            <form class="form" onSubmit={(e) => { e.preventDefault(); checkServer(); }}>
              <Field label="URL do servidor" hint="O endereço do seu agente Vessel, ex.: https://vessel.minhaempresa.com">
                <input type="url" placeholder="https://vessel.exemplo.com" value={url()} onInput={(e) => setUrl(e.currentTarget.value)} autofocus />
              </Field>
              <Show when={error()}><div class="callout callout-danger">{error()}</div></Show>
              <Button type="submit" variant="primary" busy={busy()} disabled={!url().trim()}>Continuar</Button>
            </form>
          }>
            <div class="form">
              <div class="server-pill">
                <Icon name="server" size={15} />
                <span style={{ flex: 1 }}>{info()!.name} · {info()!.server_id}</span>
                <button class="icon-btn" title="Trocar servidor" onClick={() => setInfo(null)}><Icon name="edit" size={14} /></button>
              </div>
              <div class="auth-tabs">
                <button classList={{ on: mode() === "login" }} onClick={() => setMode("login")}>Entrar</button>
                <button classList={{ on: mode() === "signup" }} onClick={() => setMode("signup")}>Criar conta</button>
                <button classList={{ on: mode() === "recover" }} onClick={() => setMode("recover")}>Recuperar</button>
              </div>
              <form class="form" onSubmit={submit}>
                <Field label="E-mail">
                  <input type="email" value={email()} onInput={(e) => setEmail(e.currentTarget.value)} autocomplete="username" required />
                </Field>
                <Show when={mode() === "recover"}>
                  <Field label="Chave de recuperação" hint="A chave mostrada quando você criou a conta.">
                    <input class="mono" value={recoveryKey()} onInput={(e) => setRecoveryKey(e.currentTarget.value)} placeholder="XXXX-XXXX-…" required />
                  </Field>
                </Show>
                <Field label={mode() === "login" ? "Senha mestre" : "Nova senha mestre"}>
                  <input type="password" value={password()} onInput={(e) => setPassword(e.currentTarget.value)}
                    autocomplete={mode() === "login" ? "current-password" : "new-password"} required />
                </Field>
                <Show when={mode() !== "login"}>
                  <Field label="Confirme a senha">
                    <input type="password" value={password2()} onInput={(e) => setPassword2(e.currentTarget.value)} autocomplete="new-password" required />
                  </Field>
                </Show>
                <Show when={mode() === "signup" && info()!.signups !== "open" && !info()!.first_user}>
                  <Field label="Código de convite" hint="Este servidor só aceita novas contas com convite.">
                    <input class="mono" value={invite()} onInput={(e) => setInvite(e.currentTarget.value.trim().split("/").pop() ?? "")} />
                  </Field>
                </Show>
                <Show when={mode() === "signup" && info()!.first_user}>
                  <div class="callout callout-info">Esta é a primeira conta do servidor: ela será a administradora.</div>
                </Show>
                <Show when={mode() !== "login"}>
                  <div class="callout callout-warn"><Icon name="alert" size={16} />
                    <span>A senha mestre nunca sai deste computador. Se você a esquecer, só a chave de recuperação devolve o acesso.</span>
                  </div>
                </Show>
                <Show when={error()}><div class="callout callout-danger">{error()}</div></Show>
                <Button type="submit" variant="primary" busy={busy()}>
                  {mode() === "login" ? "Entrar" : mode() === "signup" ? "Criar conta" : "Redefinir senha"}
                </Button>
              </form>
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

function RecoveryKeyScreen(props: { value: string; onDone: () => void }) {
  const [saved, setSaved] = createSignal(false);
  return (
    <div class="auth-card">
      <Brand />
      <h1>Guarde sua chave de recuperação</h1>
      <p class="lead">É a única forma de recuperar a conta se você esquecer a senha mestre. Ela não será mostrada de novo.</p>
      <div class="recovery">{props.value}</div>
      <div style={{ display: "flex", gap: "8px", margin: "14px 0" }}>
        <Button icon="copy" onClick={() => copy(props.value, "Chave copiada")}>Copiar</Button>
      </div>
      <label class="check" style={{ margin: "6px 0 16px" }}>
        <input type="checkbox" checked={saved()} onChange={(e) => setSaved(e.currentTarget.checked)} />
        Guardei a chave em um lugar seguro (gerenciador de senhas ou papel)
      </label>
      <Button variant="primary" disabled={!saved()} onClick={props.onDone}>Continuar</Button>
    </div>
  );
}

export function Unlock() {
  const [password, setPassword] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const submit = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      setStatus(await api.unlock(password()));
    } catch (err) {
      setError(errorCode(err) === "wrong_password" ? "Senha incorreta." : errorMessage(err));
      setPassword("");
    } finally {
      setBusy(false);
    }
  };

  const logout = async () => {
    setStatus(await api.logout());
  };

  return (
    <div class="auth">
      <div class="auth-card">
        <Brand />
        <h1>Desbloquear</h1>
        <p class="lead">{status()?.email}<br /><span style={{ "font-size": "12.5px" }}>{status()?.server_url}</span></p>
        <form class="form" onSubmit={submit}>
          <Field label="Senha mestre">
            <input type="password" value={password()} onInput={(e) => setPassword(e.currentTarget.value)} autofocus autocomplete="current-password" />
          </Field>
          <Show when={error()}><div class="callout callout-danger">{error()}</div></Show>
          <Button type="submit" variant="primary" icon="lock" busy={busy()} disabled={!password()}>Desbloquear</Button>
          <Button variant="ghost" icon="logout" onClick={logout}>Sair desta conta</Button>
        </form>
      </div>
    </div>
  );
}
