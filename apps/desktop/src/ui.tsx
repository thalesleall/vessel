import { For, Show, createSignal, onCleanup, onMount, type JSX, type ParentProps } from "solid-js";
import { Portal } from "solid-js/web";
import { toasts } from "./store";
import { exitCopy } from "./motion";

// ---------------------------------------------------------------------------------------------
// Icons (inline, stroke-based)
// ---------------------------------------------------------------------------------------------

const paths: Record<string, string> = {
  server: "M4 5h16v6H4zM4 13h16v6H4zM8 8h.01M8 16h.01",
  terminal: "M4 17l6-5-6-5M12 19h8",
  key: "M15 7a4 4 0 1 1-3.9 4.9L4 19v-3h3v-3h3l1.1-1.1A4 4 0 0 1 15 7zM16 8h.01",
  code: "M8 9l-4 3 4 3M16 9l4 3-4 3M13 6l-2 12",
  route: "M6 19a2 2 0 1 0 0-4 2 2 0 0 0 0 4zM18 9a2 2 0 1 0 0-4 2 2 0 0 0 0 4zM6 15V9a4 4 0 0 1 4-4h4M18 9v6a4 4 0 0 1-4 4h-4",
  vault: "M4 5h16v14H4zM12 12m-3 0a3 3 0 1 0 6 0a3 3 0 1 0-6 0M12 9v-1M12 16v-1",
  bot: "M7 8h10a3 3 0 0 1 3 3v5a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3v-5a3 3 0 0 1 3-3zM12 4v4M9 13h.01M15 13h.01M9 16h6",
  settings: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-2.7 1.1V21a2 2 0 1 1-4 0v-.1a1.6 1.6 0 0 0-2.7-1.1l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.6 1.6 0 0 0-1.1-2.7H3a2 2 0 1 1 0-4h.1a1.6 1.6 0 0 0 1.1-2.7l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.6 1.6 0 0 0 2.7-1.1V3a2 2 0 1 1 4 0v.1a1.6 1.6 0 0 0 2.7 1.1l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0 1.1 2.7H21a2 2 0 1 1 0 4h-.1a1.6 1.6 0 0 0-1.5 1.3z",
  plus: "M12 5v14M5 12h14",
  search: "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM20 20l-3.5-3.5",
  x: "M6 6l12 12M18 6L6 18",
  edit: "M4 20h4L19 9l-4-4L4 16zM14 6l4 4",
  trash: "M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3",
  play: "M7 5l12 7-12 7z",
  download: "M12 4v11M7 10l5 5 5-5M5 20h14",
  stop: "M7 7h10v10H7z",
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  lock: "M6 11h12v9H6zM8 11V8a4 4 0 0 1 8 0v3",
  sync: "M4 12a8 8 0 0 1 14-5.3L20 9M20 4v5h-5M20 12a8 8 0 0 1-14 5.3L4 15M4 20v-5h5",
  user: "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 20a8 8 0 0 1 16 0",
  users: "M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM2 20a7 7 0 0 1 14 0M16 3.5a4 4 0 0 1 0 7M22 20a7 7 0 0 0-4-6.3",
  copy: "M8 8h12v12H8zM4 16V4h12",
  check: "M5 12l5 5L20 7",
  alert: "M12 9v4M12 17h.01M10.3 3.9L1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z",
  shield: "M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z",
  monitor: "M3 4h18v12H3zM8 20h8M12 16v4",
  zap: "M13 2L4 14h7l-1 8 9-12h-7z",
  logout: "M15 4h4v16h-4M10 8l-4 4 4 4M6 12h10",
  file: "M6 3h8l4 4v14H6zM14 3v4h4",
  "chevron-left": "M15 6l-6 6 6 6",
  laptop: "M5 5h14v10H5zM2 19h20",
  cloud: "M7 18a4 4 0 0 1-.6-8A6 6 0 0 1 18 9a4.5 4.5 0 0 1 0 9z",
  unlink: "M9 15l-1.5 1.5a3.5 3.5 0 0 1-5-5L5 9M15 9l1.5-1.5a3.5 3.5 0 0 1 5 5L19 15M4 4l16 16",
};

export function Icon(props: { name: string; size?: number; class?: string }) {
  return (
    <svg class={`icon ${props.class ?? ""}`} width={props.size ?? 18} height={props.size ?? 18} viewBox="0 0 24 24" fill="none"
      stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d={paths[props.name] ?? paths.file} />
    </svg>
  );
}

// ---------------------------------------------------------------------------------------------
// Shell badges: a colored tile with a glyph per shell type (and per WSL distro)
// ---------------------------------------------------------------------------------------------

type ShellLike = { kind: string; distro?: string } | null | undefined;

const distroColors: [RegExp, string][] = [
  [/ubuntu/i, "#e95420"],
  [/debian/i, "#a81d33"],
  [/kali/i, "#367bf0"],
  [/arch/i, "#1793d1"],
  [/fedora/i, "#3c6eb4"],
  [/alpine/i, "#0d597f"],
  [/suse/i, "#5a9a20"],
  [/oracle/i, "#c74634"],
  [/alma|rocky|centos|rhel|red ?hat/i, "#b0272d"],
];

function badgeFor(shell: ShellLike, ssh: boolean): { bg: string; glyph?: string; icon?: string; title: string } {
  if (ssh) return { bg: "#14806b", icon: "server", title: "SSH" };
  switch (shell?.kind) {
    case "pwsh": return { bg: "#2f6fd6", glyph: ">_", title: "PowerShell" };
    case "windows_powershell": return { bg: "#123f7a", glyph: ">_", title: "Windows PowerShell" };
    case "cmd": return { bg: "#30343c", glyph: "C:\\", title: "Prompt de Comando" };
    case "git_bash": return { bg: "#e0502e", glyph: "git", title: "Git Bash" };
    case "wsl": {
      const d = shell.distro ?? "";
      const bg = distroColors.find(([re]) => re.test(d))?.[1] ?? "#3d8c40";
      return { bg, glyph: "$_", title: `WSL (${d})` };
    }
    default: return { bg: "#6b4fd8", icon: "terminal", title: "Terminal" };
  }
}

/** Colored tile identifying a shell type (or SSH). */
export function ShellIcon(props: { shell?: ShellLike; ssh?: boolean; size?: number; color?: string }) {
  const size = () => props.size ?? 18;
  const b = () => badgeFor(props.shell, !!props.ssh);
  return (
    <span class="shell-icon" title={b().title} aria-hidden="true"
      style={{ width: `${size()}px`, height: `${size()}px`, background: props.color || b().bg, "border-radius": `${Math.round(size() * 0.26)}px` }}>
      <Show when={b().glyph} fallback={<Icon name={b().icon ?? "terminal"} size={Math.round(size() * 0.66)} />}>
        <span class="glyph" style={{ "font-size": `${Math.max(7, Math.round(size() * (b().glyph!.length > 2 ? 0.4 : 0.5)))}px` }}>{b().glyph}</span>
      </Show>
    </span>
  );
}

// ---------------------------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------------------------

export function Button(props: ParentProps<{
  onClick?: (e: MouseEvent) => void;
  variant?: "primary" | "ghost" | "danger" | "subtle";
  icon?: string;
  disabled?: boolean;
  type?: "button" | "submit";
  title?: string;
  small?: boolean;
  busy?: boolean;
}>) {
  return (
    <button type={props.type ?? "button"} title={props.title} aria-label={props.children ? undefined : props.title} aria-busy={props.busy || undefined}
      class={`btn btn-${props.variant ?? "subtle"} ${props.small ? "btn-sm" : ""}`}
      disabled={props.disabled || props.busy} onClick={(e) => props.onClick?.(e)}>
      <Show when={props.busy} fallback={<Show when={props.icon}><Icon name={props.icon!} size={16} /></Show>}>
        <span class="spinner" />
      </Show>
      <Show when={props.children}><span>{props.children}</span></Show>
    </button>
  );
}

export function Field(props: ParentProps<{ label: string; hint?: string }>) {
  return (
    <label class="field">
      <span class="field-label">{props.label}</span>
      {props.children}
      <Show when={props.hint}><span class="field-hint">{props.hint}</span></Show>
    </label>
  );
}

/**
 * Dialog. Escape, the close button and a click outside ask before discarding typed changes;
 * focus moves into the dialog and returns to where it was when it closes.
 */
export function Modal(props: ParentProps<{ title: string; onClose: () => void; wide?: boolean; footer?: JSX.Element }>) {
  let backdrop: HTMLDivElement | undefined;
  let dialog: HTMLDivElement | undefined;
  let dirty = false;
  const opener = document.activeElement as HTMLElement | null;
  const dismiss = async () => {
    if (dirty && !(await confirmAction({
      title: "Descartar alterações?",
      message: "O que você digitou neste formulário ainda não foi salvo.",
      confirm: "Descartar",
    }))) return;
    props.onClose();
  };
  const onKey = (e: KeyboardEvent) => {
    // Only the topmost dialog reacts.
    if (e.key === "Escape" && backdrop && backdrop === [...document.querySelectorAll(".modal-backdrop:not(.leaving)")].pop()) {
      e.stopPropagation();
      dismiss();
    }
  };
  onMount(() => {
    document.addEventListener("keydown", onKey);
    queueMicrotask(() => {
      if (dialog && !dialog.contains(document.activeElement)) dialog.focus();
    });
  });
  onCleanup(() => {
    document.removeEventListener("keydown", onKey);
    exitCopy(backdrop, "leaving", 150);
    if (opener?.isConnected) opener.focus();
  });
  return (
    <Portal>
      <div class="modal-backdrop" ref={backdrop} onMouseDown={(e) => e.target === e.currentTarget && dismiss()}>
        <div ref={dialog} class={`modal ${props.wide ? "modal-wide" : ""}`} role="dialog" aria-modal="true" aria-label={props.title}
          tabIndex={-1} onInput={() => (dirty = true)}>
          <header class="modal-header">
            <h2>{props.title}</h2>
            <button class="icon-btn" aria-label="Fechar" onClick={() => dismiss()} title="Fechar"><Icon name="x" /></button>
          </header>
          <div class="modal-body">{props.children}</div>
          <Show when={props.footer}><footer class="modal-footer">{props.footer}</footer></Show>
        </div>
      </div>
    </Portal>
  );
}

export function Empty(props: ParentProps<{ icon: string; title: string }>) {
  return (
    <div class="empty">
      <div class="empty-icon"><Icon name={props.icon} size={28} /></div>
      <h3>{props.title}</h3>
      <div class="empty-body">{props.children}</div>
    </div>
  );
}

export function Toasts() {
  return (
    <div class="toasts" aria-live="polite" aria-atomic="false">
      <For each={toasts()}>
        {(t) => {
          let el: HTMLDivElement | undefined;
          onCleanup(() => exitCopy(el, "leaving", 180, true));
          return <div ref={el} class={`toast toast-${t.kind}`} role="status">{t.text}</div>;
        }}
      </For>
    </div>
  );
}

export function Badge(props: ParentProps<{ tone?: "blue" | "green" | "amber" | "red" | "gray" }>) {
  return <span class={`badge badge-${props.tone ?? "gray"}`}>{props.children}</span>;
}

/** Confirm dialog returning a promise. Pass a string, or a title, message and button label. */
type ConfirmOptions = { title?: string; message: string; confirm?: string; danger?: boolean;
  /** Text the user must type to enable the button (compared ignoring case, spaces and dashes). */
  expect?: string; expectLabel?: string };
const normalizeTyped = (s: string) => s.toLowerCase().replace(/[^0-9a-z]/g, "");
const [confirmState, setConfirmState] = createSignal<(ConfirmOptions & { resolve: (v: boolean) => void }) | null>(null);
export function confirmAction(what: string | ConfirmOptions, danger = true): Promise<boolean> {
  const opts: ConfirmOptions = typeof what === "string" ? { message: what, danger } : what;
  return new Promise((resolve) => setConfirmState({ ...opts, resolve }));
}
export function ConfirmHost() {
  const [typed, setTyped] = createSignal("");
  const close = (v: boolean) => {
    confirmState()?.resolve(v);
    setConfirmState(null);
    setTyped("");
  };
  const matches = () => { const e = confirmState()?.expect; return !e || normalizeTyped(typed()) === normalizeTyped(e); };
  return (
    <Show when={confirmState()}>
      {(c) => (
        <Modal title={c().title ?? "Confirmar"} onClose={() => close(false)}
          footer={<>
            <Button onClick={() => close(false)}>Cancelar</Button>
            <Button variant={c().danger === false ? "primary" : "danger"} disabled={!matches()} onClick={() => matches() && close(true)}>{c().confirm ?? "Confirmar"}</Button>
          </>}>
          <p class="confirm-text">{c().message}</p>
          <Show when={c().expect}>
            <Field label={c().expectLabel ?? "Digite para confirmar"}>
              <input class="mono" value={typed()} onInput={(e) => setTyped(e.currentTarget.value)} aria-invalid={typed() !== "" && !matches()} autofocus />
            </Field>
          </Show>
        </Modal>
      )}
    </Show>
  );
}

export function initials(text: string): string {
  const parts = text.trim().split(/[\s.\-_@]+/).filter(Boolean);
  return ((parts[0]?.[0] ?? "?") + (parts[1]?.[0] ?? "")).toUpperCase();
}

export function colorFor(text: string): string {
  const palette = ["#1558c9", "#0f7fa8", "#6a2a8c", "#c2501a", "#2a8a5a", "#37b0de", "#8a1f9a", "#a8791a"];
  let h = 0;
  for (const c of text) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return palette[h % palette.length];
}

const relative = new Intl.RelativeTimeFormat(navigator.language || "pt-BR", { numeric: "auto", style: "short" });

export function timeAgo(unixSeconds: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unixSeconds));
  if (s < 60) return relative.format(0, "second");
  if (s < 3600) return relative.format(-Math.floor(s / 60), "minute");
  if (s < 86400) return relative.format(-Math.floor(s / 3600), "hour");
  return relative.format(-Math.floor(s / 86400), "day");
}
