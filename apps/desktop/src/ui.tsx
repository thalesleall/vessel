import { For, Show, createSignal, onCleanup, onMount, type JSX, type ParentProps } from "solid-js";
import { Portal } from "solid-js/web";
import { toasts } from "./store";

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
    <button type={props.type ?? "button"} title={props.title}
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

export function Modal(props: ParentProps<{ title: string; onClose: () => void; wide?: boolean; footer?: JSX.Element }>) {
  const onKey = (e: KeyboardEvent) => e.key === "Escape" && props.onClose();
  onMount(() => document.addEventListener("keydown", onKey));
  onCleanup(() => document.removeEventListener("keydown", onKey));
  return (
    <Portal>
      <div class="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
        <div class={`modal ${props.wide ? "modal-wide" : ""}`} role="dialog" aria-label={props.title}>
          <header class="modal-header">
            <h2>{props.title}</h2>
            <button class="icon-btn" onClick={() => props.onClose()} title="Fechar"><Icon name="x" /></button>
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
    <div class="toasts">
      <For each={toasts()}>
        {(t) => <div class={`toast toast-${t.kind}`}>{t.text}</div>}
      </For>
    </div>
  );
}

export function Badge(props: ParentProps<{ tone?: "blue" | "green" | "amber" | "red" | "gray" }>) {
  return <span class={`badge badge-${props.tone ?? "gray"}`}>{props.children}</span>;
}

/** Confirm dialog returning a promise. */
const [confirmState, setConfirmState] = createSignal<{ text: string; danger: boolean; resolve: (v: boolean) => void } | null>(null);
export function confirmAction(text: string, danger = true): Promise<boolean> {
  return new Promise((resolve) => setConfirmState({ text, danger, resolve }));
}
export function ConfirmHost() {
  const close = (v: boolean) => {
    confirmState()?.resolve(v);
    setConfirmState(null);
  };
  return (
    <Show when={confirmState()}>
      {(c) => (
        <Modal title="Confirmar" onClose={() => close(false)}
          footer={<>
            <Button onClick={() => close(false)}>Cancelar</Button>
            <Button variant={c().danger ? "danger" : "primary"} onClick={() => close(true)}>Confirmar</Button>
          </>}>
          <p class="confirm-text">{c().text}</p>
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
  const palette = ["#3b82f6", "#8b5cf6", "#ec4899", "#f59e0b", "#10b981", "#06b6d4", "#ef4444", "#84cc16"];
  let h = 0;
  for (const c of text) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return palette[h % palette.length];
}

export function timeAgo(unixSeconds: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unixSeconds));
  if (s < 60) return "agora";
  if (s < 3600) return `há ${Math.floor(s / 60)} min`;
  if (s < 86400) return `há ${Math.floor(s / 3600)} h`;
  return `há ${Math.floor(s / 86400)} d`;
}
