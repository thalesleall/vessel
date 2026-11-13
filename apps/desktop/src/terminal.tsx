import { Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { SearchAddon } from "@xterm/addon-search";
import { api, errorCode, errorMessage, type SessionInfo, type Shell } from "./api";
import { activeTab, refreshSessions, sessions, setActiveTab, settings, toast } from "./store";
import { Button, Icon, Modal } from "./ui";
import { resolvedTheme } from "./theme";

const darkTheme: ITheme = {
  background: "#030407",
  foreground: "#e3e8f4",
  cursor: "#d4ff24",
  cursorAccent: "#030407",
  selectionBackground: "rgba(21, 88, 201, 0.55)",
  black: "#141a26", red: "#ff6b5e", green: "#b8e62a", yellow: "#f5c84c", blue: "#4f8ef7", magenta: "#c08cff", cyan: "#4cc9f0", white: "#c9d1e3",
  brightBlack: "#4a5470", brightRed: "#ff8f84", brightGreen: "#d4ff24", brightYellow: "#ffdd7a", brightBlue: "#7fa9f5", brightMagenta: "#d6b0ff", brightCyan: "#86ddf7", brightWhite: "#f4f6fb",
};

const lightTheme: ITheme = {
  background: "#fbf9fc",
  foreground: "#400849",
  cursor: "#ff5a0a",
  cursorAccent: "#fbf9fc",
  selectionBackground: "rgba(55, 176, 222, 0.35)",
  black: "#400849", red: "#c2183a", green: "#1f7a3e", yellow: "#9a6200", blue: "#1558c9", magenta: "#8a1f9a", cyan: "#0f7fa8", white: "#8d7a92",
  brightBlack: "#6d5673", brightRed: "#e0334f", brightGreen: "#2a9a52", brightYellow: "#c27c00", brightBlue: "#2f74e6", brightMagenta: "#a43bb5", brightCyan: "#2a9ccc", brightWhite: "#c9bccd",
};

export const terminalTheme = () => (resolvedTheme() === "light" ? lightTheme : darkTheme);

// ---------------------------------------------------------------------------------------------
// Opening sessions
// ---------------------------------------------------------------------------------------------

const DEFAULT_COLS = 120;
const DEFAULT_ROWS = 32;

export async function openProfile(profileId: string) {
  try {
    const s = await api.openLocal({ profileId }, DEFAULT_COLS, DEFAULT_ROWS);
    await refreshSessions();
    setActiveTab(s.id);
  } catch (e) {
    toast(errorMessage(e), "error");
  }
}

export async function openShell(shell: Shell) {
  try {
    const s = await api.openLocal({ shell }, DEFAULT_COLS, DEFAULT_ROWS);
    await refreshSessions();
    setActiveTab(s.id);
  } catch (e) {
    toast(errorMessage(e), "error");
  }
}

interface HostKeyQuestion { hostId: string; host: string; fingerprint: string; keyType: string; changed: boolean; expected?: string; onTrust?: () => void }
const [hostKeyQuestion, setHostKeyQuestion] = createSignal<HostKeyQuestion | null>(null);
const [connecting, setConnecting] = createSignal<string | null>(null);
export { connecting };

/** Runs an SSH action, asking the user to confirm unknown host keys. */
export async function withHostKeyCheck(hostId: string, action: (trustNew: boolean) => Promise<void>) {
  try {
    await action(false);
  } catch (e) {
    const code = errorCode(e);
    const d = (e as { details?: Record<string, string> }).details ?? {};
    if (code === "host_key_unknown") {
      setHostKeyQuestion({
        hostId, host: d.host, fingerprint: d.fingerprint, keyType: d.key_type, changed: false,
        onTrust: () => action(true).catch((err) => toast(errorMessage(err), "error")),
      });
    } else if (code === "host_key_changed") {
      setHostKeyQuestion({ hostId, host: d.host, fingerprint: d.actual, keyType: d.key_type, changed: true, expected: d.expected });
    } else {
      toast(errorMessage(e), "error");
    }
  }
}

export async function connectHost(hostId: string) {
  setConnecting(hostId);
  await withHostKeyCheck(hostId, async (trustNew) => {
    const s = await api.openSsh(hostId, DEFAULT_COLS, DEFAULT_ROWS, trustNew);
    await refreshSessions();
    setActiveTab(s.id);
  });
  setConnecting(null);
}

export function HostKeyPrompt() {
  const close = () => setHostKeyQuestion(null);
  return (
    <Show when={hostKeyQuestion()}>
      {(q) => (
        <Modal title={q().changed ? "A chave do servidor mudou" : "Confirme o servidor"} onClose={close}
          footer={q().changed
            ? <Button onClick={close}>Entendi</Button>
            : <>
                <Button onClick={close}>Cancelar</Button>
                <Button variant="primary" icon="shield" onClick={() => { const t = q().onTrust; close(); t?.(); }}>Confiar e conectar</Button>
              </>}>
          <div class="form">
            <Show when={q().changed} fallback={
              <p class="confirm-text">É a primeira conexão com <b>{q().host}</b>. Compare a impressão digital abaixo com a do servidor
                (<span class="mono">ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub</span>) antes de confiar. Ela será salva e sincronizada.</p>
            }>
              <div class="callout callout-danger"><Icon name="alert" />
                <span>A identidade de <b>{q().host}</b> não é a que você salvou. Pode ser uma reinstalação, ou alguém interceptando a conexão.
                  Se a mudança for esperada, apague o host conhecido em Chaves → Hosts conhecidos e conecte de novo.</span>
              </div>
              <div class="field"><span class="field-label">Esperada</span><div class="code-block">{q().expected}</div></div>
            </Show>
            <div class="field"><span class="field-label">{q().changed ? "Recebida" : `Impressão digital (${q().keyType})`}</span>
              <div class="code-block">{q().fingerprint}</div></div>
          </div>
        </Modal>
      )}
    </Show>
  );
}

// ---------------------------------------------------------------------------------------------
// Terminal view
// ---------------------------------------------------------------------------------------------

export function TerminalView(props: { id: string }) {
  let host!: HTMLDivElement;
  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  const session = (): SessionInfo => sessions().find((s) => s.id === props.id) ?? {
    id: props.id, title: "", kind: { kind: "local", profile_id: null, shell: "" }, alive: false, created_at: 0, by_agent: false,
  };
  const visible = () => activeTab() === props.id;

  const sendResize = () => {
    if (!term || !fit || !visible()) return;
    try {
      fit.fit();
      api.resize(props.id, term.cols, term.rows);
    } catch {
      /* not laid out yet */
    }
  };

  createEffect(() => {
    const t = terminalTheme();
    if (term) term.options.theme = t;
  });

  onMount(() => {
    const s = settings();
    term = new Terminal({
      theme: terminalTheme(),
      fontFamily: s?.font_family || "Cascadia Mono, Consolas, monospace",
      fontSize: s?.font_size || 14,
      lineHeight: s?.line_height || 1.2,
      cursorStyle: (s?.cursor_style as "bar" | "block" | "underline") || "bar",
      cursorBlink: s?.cursor_blink ?? true,
      scrollback: s?.scrollback || 10000,
      allowProposedApi: true,
      macOptionIsMeta: true,
      rightClickSelectsWord: false,
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new SearchAddon());
    const unicode = new Unicode11Addon();
    term.loadAddon(unicode);
    term.unicode.activeVersion = "11";
    term.open(host);
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* falls back to the DOM renderer */
    }

    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      if (e.ctrlKey && e.shiftKey && e.code === "KeyC") {
        const sel = term!.getSelection();
        if (sel) navigator.clipboard.writeText(sel);
        return false;
      }
      if (e.ctrlKey && e.shiftKey && e.code === "KeyV") {
        navigator.clipboard.readText().then((t) => { if (t) api.write(props.id, t); });
        return false;
      }
      if (e.ctrlKey && e.code === "KeyK") return false; // command palette
      return true;
    });
    term.onSelectionChange(() => {
      if (settings()?.copy_on_select) {
        const sel = term!.getSelection();
        if (sel) navigator.clipboard.writeText(sel);
      }
    });
    host.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      const sel = term!.getSelection();
      if (sel) {
        navigator.clipboard.writeText(sel);
        term!.clearSelection();
      } else {
        navigator.clipboard.readText().then((t) => { if (t) api.write(props.id, t); });
      }
    });

    term.onData((d) => api.write(props.id, d));
    term.onBinary((d) => api.write(props.id, d));
    api.attach(props.id, (bytes) => term!.write(bytes)).catch((e) => toast(errorMessage(e), "error"));

    const ro = new ResizeObserver(() => sendResize());
    ro.observe(host);
    onCleanup(() => {
      ro.disconnect();
      term?.dispose();
    });
  });

  createEffect(() => {
    if (visible()) {
      requestAnimationFrame(() => {
        sendResize();
        term?.focus();
      });
    }
  });

  return (
    <div class="term-wrap" classList={{ visible: visible() }}>
      <Show when={!session().alive}>
        <div class="term-banner">
          <span>Sessão encerrada</span>
          <Show when={session().kind.kind === "ssh"}>
            <Button small icon="sync" onClick={() => { const k = session().kind; if (k.kind === "ssh") connectHost(k.host_id); }}>Reconectar</Button>
          </Show>
        </div>
      </Show>
      <div ref={host} style={{ height: "100%" }} />
    </div>
  );
}
