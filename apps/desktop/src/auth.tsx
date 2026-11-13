import { For, Match, Show, Switch, createSignal, onMount } from "solid-js";
import { api, errorCode, errorMessage, type ServerInfo } from "./api";
import { copy, refreshStatus, setStatus, status, toast } from "./store";
import { themePref, setThemePref, type ThemePref } from "./theme";
import { transition } from "./motion";
import { Button, Field, Icon, confirmAction } from "./ui";

export function Brand(props: { sub?: string }) {
  return (
    <div class="brand">
      <div class="brand-logo" />
      <div>
        <div class="brand-name">Vessel</div>
        <Show when={props.sub}><div class="brand-sub">{props.sub}</div></Show>
      </div>
    </div>
  );
}

const MIN_PASSWORD = 10;

function passwordProblem(password: string, confirm: string): string | null {
  if (password.length < MIN_PASSWORD) return `Use uma senha com pelo menos ${MIN_PASSWORD} caracteres.`;
  if (password !== confirm) return "As senhas não coincidem.";
  return null;
}

// ---------------------------------------------------------------------------------------------
// First run: choose local-only or a sync server
// ---------------------------------------------------------------------------------------------

type Step = "choose" | "local" | "server";

export function Onboarding() {
  const [step, setStepNow] = createSignal<Step>("choose");
  const setStep = (s: Step) => transition(() => setStepNow(s));
  return (
    <Switch>
      <Match when={step() === "choose"}><Welcome onPick={setStep} /></Match>
      <Match when={step() === "local"}><LocalSetup onBack={() => setStep("choose")} /></Match>
      <Match when={step() === "server"}><Connect onBack={() => setStep("choose")} /></Match>
    </Switch>
  );
}

function Welcome(props: { onPick: (s: Step) => void }) {
  return (
    <div class="welcome">
      <div class="welcome-inner">
        <Brand />
        <h1>Como você quer usar o Vessel?</h1>
        <p class="lead">Os terminais rodam sempre no seu computador. A escolha é sobre onde ficam seus hosts, chaves e snippets.</p>
        <div class="choices">
          <button class="choice" onClick={() => props.onPick("local")}>
            <div class="art"><LocalArt /></div>
            <h2>Só neste computador</h2>
            <p>Tudo fica guardado aqui, criptografado com a sua senha. Não precisa de servidor.</p>
            <span class="hint">Dá para conectar um servidor depois, sem perder nada.</span>
          </button>
          <button class="choice" onClick={() => props.onPick("server")}>
            <div class="art"><SyncArt /></div>
            <h2>Sincronizar com um servidor</h2>
            <p>O mesmo ambiente em todos os seus computadores, e times compartilhando acessos.</p>
            <span class="hint">Precisa do endereço de um agente Vessel.</span>
          </button>
        </div>
        <div class="foot">
          <span>Nada sai deste computador sem estar criptografado.</span>
          <span style={{ "margin-left": "auto" }}><ThemeSwitch /></span>
        </div>
      </div>
    </div>
  );
}

export function ThemeSwitch() {
  const options: [ThemePref, string][] = [["system", "Sistema"], ["dark", "Escuro"], ["light", "Claro"]];
  return (
    <div class="segmented" role="radiogroup" aria-label="Tema">
      <For each={options}>
        {([value, label]) => (
          <button role="radio" aria-checked={themePref() === value} classList={{ on: themePref() === value }} onClick={() => setThemePref(value)}>
            {label}
          </button>
        )}
      </For>
    </div>
  );
}

/** A laptop with a prompt, a caret and a padlock: everything stays here. */
function LocalArt() {
  return (
    <svg viewBox="0 0 260 150" aria-hidden="true">
      <rect class="art-fill draw" x="66" y="18" width="128" height="86" rx="7" />
      <path class="art-line draw" d="M48 116h164l-10-12H58z" />
      <path class="art-line draw" d="M86 46l11 9-11 9" />
      <rect class="art-signal" x="104" y="58" width="9" height="14" rx="1.5" />
      <rect class="art-fill draw" x="160" y="70" width="20" height="16" rx="3" />
      <path class="art-line draw" d="M164 70v-5a6 6 0 0 1 12 0v5" />
      <circle class="art-signal" cx="170" cy="78" r="2.2" />
    </svg>
  );
}

/** One small server and three computers that hold the same things. */
function SyncArt() {
  return (
    <svg viewBox="0 0 260 150" aria-hidden="true">
      <path class="art-faint" d="M130 58L52 92M130 58v38M130 58l78 34" />
      <rect class="art-fill draw" x="108" y="14" width="44" height="44" rx="6" />
      <path class="art-line draw" d="M117 28h26M117 40h26" />
      <circle class="art-signal" cx="121" cy="49" r="2.4" />
      <rect class="art-fill draw" x="24" y="92" width="56" height="38" rx="5" />
      <rect class="art-fill draw" x="102" y="96" width="56" height="38" rx="5" />
      <rect class="art-fill draw" x="180" y="92" width="56" height="38" rx="5" />
      <rect class="art-signal" x="36" y="104" width="6" height="11" rx="1" />
      <rect class="art-signal" x="114" y="108" width="6" height="11" rx="1" />
      <rect class="art-signal" x="192" y="104" width="6" height="11" rx="1" />
      <circle class="art-signal" cx="91" cy="75" r="2.6" />
      <circle class="art-signal" cx="169" cy="75" r="2.6" />
    </svg>
  );
}

// ---------------------------------------------------------------------------------------------
// Local-only setup
// ---------------------------------------------------------------------------------------------

function LocalSetup(props: { onBack: () => void }) {
  const [name, setName] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [password2, setPassword2] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [recovery, setRecovery] = createSignal<string | null>(null);

  const submit = async (e: Event) => {
    e.preventDefault();
    const problem = passwordProblem(password(), password2());
    if (problem) return setError(problem);
    setError(null);
    setBusy(true);
    try {
      setRecovery(await api.createLocal(name(), password()));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="auth">
      <Show when={!recovery()} fallback={<RecoveryKeyScreen value={recovery()!} step={2} onDone={() => refreshStatus()} />}>
        <div class="auth-card">
          <button class="auth-back" onClick={props.onBack}><Icon name="chevron-left" size={14} />Voltar</button>
          <div class="steps" aria-label="Passo 1 de 2"><span class="on" /><span /></div>
          <h1>Crie sua senha</h1>
          <p class="lead">Ela desbloqueia o Vessel e protege tudo o que fica salvo neste computador.</p>
          <form class="form" onSubmit={submit}>
            <Field label="Seu nome" hint="Opcional. Aparece no canto da tela.">
              <input value={name()} onInput={(e) => setName(e.currentTarget.value)} placeholder="Ex.: Ana…" autofocus />
            </Field>
            <Field label="Senha">
              <input type="password" value={password()} onInput={(e) => setPassword(e.currentTarget.value)} autocomplete="new-password" required />
            </Field>
            <Field label="Confirme a senha">
              <input type="password" value={password2()} onInput={(e) => setPassword2(e.currentTarget.value)} autocomplete="new-password" required />
            </Field>
            <div class="callout callout-warn"><Icon name="alert" size={16} />
              <span>Ninguém consegue recuperar essa senha por você. No próximo passo você recebe uma chave de recuperação.</span>
            </div>
            <Show when={error()}><div class="callout callout-danger" role="alert"><Icon name="alert" size={16} />{error()}</div></Show>
            <Button type="submit" variant="primary" busy={busy()}>Criar senha</Button>
          </form>
        </div>
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Server sign-in / sign-up / recovery
// ---------------------------------------------------------------------------------------------

type Mode = "login" | "signup" | "recover";

export function Connect(props: { onBack?: () => void }) {
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
    if (mode() !== "login") {
      const problem = passwordProblem(password(), password2());
      if (problem) return setError(problem);
    }
    setBusy(true);
    try {
      if (mode() === "login") {
        setStatus(await api.login(url(), email(), password()));
      } else if (mode() === "signup") {
        setNewRecovery(await api.signup(url(), email(), password(), invite()));
      } else {
        // The used recovery key stopped working: show the new one before entering the app.
        setNewRecovery(await api.recover(url(), email(), recoveryKey(), password()));
        toast("Senha redefinida. Os outros computadores precisam entrar de novo.", "success");
      }
    } catch (err) {
      setError(errorCode(err) === "wrong_password" ? "E-mail ou senha incorretos." : errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="auth">
      <Show when={!newRecovery()} fallback={<RecoveryKeyScreen value={newRecovery()!} onDone={() => refreshStatus()} />}>
        <div class="auth-card">
          <Show when={props.onBack}>
            <button class="auth-back" onClick={props.onBack}><Icon name="chevron-left" size={14} />Voltar</button>
          </Show>
          <h1>Conectar a um servidor</h1>
          <p class="lead">Hosts, chaves e snippets sincronizam criptografados pelo seu agente Vessel. Os terminais continuam rodando aqui.</p>

          <Show when={info()} fallback={
            <form class="form" onSubmit={(e) => { e.preventDefault(); checkServer(); }}>
              <Field label="Endereço do servidor" hint="Ex.: https://vessel.minhaempresa.com">
                <input type="url" placeholder="https://" value={url()} onInput={(e) => setUrl(e.currentTarget.value)} autofocus />
              </Field>
              <Show when={error()}><div class="callout callout-danger" role="alert"><Icon name="alert" size={16} />{error()}</div></Show>
              <Button type="submit" variant="primary" busy={busy()} disabled={!url().trim()}>Verificar servidor</Button>
            </form>
          }>
            <div class="form">
              <div class="server-pill">
                <Icon name="server" size={15} />
                <span style={{ flex: 1 }}>{info()!.name} em {info()!.server_id}</span>
                <button class="icon-btn" aria-label="Trocar servidor" title="Trocar servidor" onClick={() => setInfo(null)}><Icon name="edit" size={14} /></button>
              </div>
              <div class="auth-tabs" role="tablist">
                <button role="tab" classList={{ on: mode() === "login" }} onClick={() => setMode("login")}>Entrar</button>
                <button role="tab" classList={{ on: mode() === "signup" }} onClick={() => setMode("signup")}>Criar conta</button>
                <button role="tab" classList={{ on: mode() === "recover" }} onClick={() => setMode("recover")}>Esqueci a senha</button>
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
                <Field label={mode() === "login" ? "Senha" : "Nova senha"}>
                  <input type="password" value={password()} onInput={(e) => setPassword(e.currentTarget.value)}
                    autocomplete={mode() === "login" ? "current-password" : "new-password"} required />
                </Field>
                <Show when={mode() !== "login"}>
                  <Field label="Confirme a senha">
                    <input type="password" value={password2()} onInput={(e) => setPassword2(e.currentTarget.value)} autocomplete="new-password" required />
                  </Field>
                </Show>
                <Show when={mode() === "signup" && info()!.signups !== "open" && !info()!.first_user}>
                  <Field label="Código de convite" hint="Este servidor só aceita contas novas com convite.">
                    <input class="mono" value={invite()} onInput={(e) => setInvite(e.currentTarget.value.trim().split("/").pop() ?? "")} />
                  </Field>
                </Show>
                <Show when={mode() === "signup" && info()!.first_user}>
                  <div class="callout callout-info"><Icon name="shield" size={16} />Esta é a primeira conta do servidor, então ela será a administradora.</div>
                  <Field label="Código de instalação" hint="Aparece no log do agente ao iniciar (ou é o valor de VESSEL_BOOTSTRAP_TOKEN).">
                    <input class="mono" value={invite()} onInput={(e) => setInvite(e.currentTarget.value.trim())} required />
                  </Field>
                </Show>
                <Show when={mode() !== "login"}>
                  <div class="callout callout-warn"><Icon name="alert" size={16} />
                    <span>A senha nunca sai deste computador. Se você a esquecer, só a chave de recuperação devolve o acesso.</span>
                  </div>
                </Show>
                <Show when={error()}><div class="callout callout-danger" role="alert"><Icon name="alert" size={16} />{error()}</div></Show>
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

// ---------------------------------------------------------------------------------------------
// Recovery key
// ---------------------------------------------------------------------------------------------

export function RecoveryKeyBody(props: { value: string }) {
  return (
    <>
      <ul class="explain">
        <li><Icon name="key" size={16} /><span>Se você esquecer a senha, esta chave cria uma nova e devolve o acesso a tudo.</span></li>
        <li><Icon name="shield" size={16} /><span>Ela não fica salva em lugar nenhum. Nem o Vessel nem o servidor conseguem mostrá-la de novo.</span></li>
        <li><Icon name="lock" size={16} /><span>Quem tiver a chave pode redefinir sua senha. Guarde do mesmo jeito que guarda uma senha.</span></li>
      </ul>
      <div class="recovery">{props.value}</div>
      <div style={{ display: "flex", gap: "8px", margin: "12px 0 4px" }}>
        <Button small icon="copy" onClick={() => copy(props.value, "Chave copiada")}>Copiar chave</Button>
      </div>
    </>
  );
}

export function RecoveryKeyScreen(props: { value: string; onDone: () => void; step?: number }) {
  const [saved, setSaved] = createSignal(false);
  return (
    <div class="auth-card" style={{ width: "min(500px, 100%)" }}>
      <Show when={props.step}><div class="steps" aria-label="Passo 2 de 2"><span class="on" /><span class="on" /></div></Show>
      <h1>Guarde sua chave de recuperação</h1>
      <p class="lead">É a única forma de voltar a entrar se você esquecer a senha.</p>
      <RecoveryKeyBody value={props.value} />
      <label class="check" style={{ margin: "14px 0 18px" }}>
        <input type="checkbox" checked={saved()} onChange={(e) => setSaved(e.currentTarget.checked)} />
        Guardei a chave num gerenciador de senhas ou no papel
      </label>
      <Button variant="primary" disabled={!saved()} onClick={props.onDone}>Começar a usar</Button>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Unlock
// ---------------------------------------------------------------------------------------------

export function Unlock() {
  const [password, setPassword] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [recovering, setRecovering] = createSignal(false);
  const local = () => status()?.local ?? false;

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

  const leave = async () => {
    const ok = await confirmAction({
      title: local() ? "Apagar os dados deste computador?" : "Sair desta conta?",
      message: local()
        ? "Hosts, chaves e snippets salvos aqui serão apagados. Não há servidor com uma cópia. Isso não pode ser desfeito."
        : "Os dados locais deste computador serão apagados. Eles continuam no servidor e voltam quando você entrar de novo.",
      confirm: local() ? "Apagar tudo" : "Sair",
      danger: true,
    });
    if (ok) setStatus(await api.logout());
  };

  return (
    <div class="auth">
      <Show when={!recovering()} fallback={<LocalRecovery onBack={() => setRecovering(false)} />}>
        <div class="auth-card">
          <Brand sub={local() ? "Só neste computador" : status()?.server_url?.replace(/^https?:\/\//, "") ?? ""} />
          <h1>Desbloquear</h1>
          <p class="lead">{status()?.email}</p>
          <form class="form" onSubmit={submit}>
            <Field label="Senha">
              <input type="password" value={password()} onInput={(e) => setPassword(e.currentTarget.value)} autofocus autocomplete="current-password" />
            </Field>
            <Show when={error()}><div class="callout callout-danger" role="alert"><Icon name="alert" size={16} />{error()}</div></Show>
            <Button type="submit" variant="primary" icon="lock" busy={busy()} disabled={!password()}>Desbloquear</Button>
          </form>
          <div style={{ display: "flex", "justify-content": "space-between", "margin-top": "18px" }}>
            <Show when={local()} fallback={<span />}>
              <button class="link-btn" onClick={() => setRecovering(true)}>Esqueci a senha</button>
            </Show>
            <button class="link-btn" style={{ color: "var(--text-3)" }} onClick={leave}>
              {local() ? "Apagar dados e recomeçar" : "Sair desta conta"}
            </button>
          </div>
        </div>
      </Show>
    </div>
  );
}

function LocalRecovery(props: { onBack: () => void }) {
  const [key, setKey] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [password2, setPassword2] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const submit = async (e: Event) => {
    e.preventDefault();
    const problem = passwordProblem(password(), password2());
    if (problem) return setError(problem);
    setBusy(true);
    setError(null);
    try {
      setStatus(await api.recoverLocal(key(), password()));
      toast("Senha redefinida.", "success");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="auth-card">
      <button class="auth-back" onClick={props.onBack}><Icon name="chevron-left" size={14} />Voltar</button>
      <h1>Criar uma senha nova</h1>
      <p class="lead">Use a chave de recuperação que você guardou quando começou a usar o Vessel.</p>
      <form class="form" onSubmit={submit}>
        <Field label="Chave de recuperação">
          <input class="mono" value={key()} onInput={(e) => setKey(e.currentTarget.value)} placeholder="XXXX-XXXX-…" autofocus required />
        </Field>
        <Field label="Nova senha">
          <input type="password" value={password()} onInput={(e) => setPassword(e.currentTarget.value)} autocomplete="new-password" required />
        </Field>
        <Field label="Confirme a senha">
          <input type="password" value={password2()} onInput={(e) => setPassword2(e.currentTarget.value)} autocomplete="new-password" required />
        </Field>
        <Show when={error()}><div class="callout callout-danger" role="alert"><Icon name="alert" size={16} />{error()}</div></Show>
        <Button type="submit" variant="primary" busy={busy()}>Redefinir senha</Button>
      </form>
    </div>
  );
}
