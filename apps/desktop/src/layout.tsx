import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { api, shellLabel } from "./api";
import {
  activeTab, itemsOf, lastSync, section, sessions, setActiveTab, setSection, setStatus, settings, status, syncError, syncing,
  refreshSessions, type Section,
} from "./store";
import { HostKeyPrompt, TerminalView, connectHost, openProfile } from "./terminal";
import { Icon, colorFor, initials } from "./ui";
import { HostsView } from "./views/hosts";
import { ProfilesView } from "./views/profiles";
import { KeychainView } from "./views/keychain";
import { SnippetsView } from "./views/snippets";
import { ForwardsView } from "./views/forwards";
import { VaultsView } from "./views/vaults";
import { AgentsView } from "./views/agents";
import { SettingsView } from "./views/settings";

const nav: { id: Section; label: string; icon: string }[] = [
  { id: "hosts", label: "Hosts", icon: "server" },
  { id: "terminals", label: "Terminais locais", icon: "terminal" },
  { id: "keychain", label: "Chaves", icon: "key" },
  { id: "snippets", label: "Snippets", icon: "code" },
  { id: "forwards", label: "Redirecionamentos", icon: "route" },
  { id: "vaults", label: "Cofres e times", icon: "vault" },
  { id: "agents", label: "Agentes de IA", icon: "bot" },
  { id: "settings", label: "Configurações", icon: "settings" },
];

function Sidebar() {
  const hosts = itemsOf("host");
  const lock = async () => setStatus(await api.lock());
  const count = (id: Section) => (id === "hosts" ? hosts().length : undefined);
  return (
    <aside class="sidebar">
      <div class="brand">
        <div class="brand-logo"><Icon name="terminal" size={17} /></div>
        <div>
          <div class="brand-name">Vessel</div>
          <div class="brand-sub">terminal + sync criptografado</div>
        </div>
      </div>
      <For each={nav}>
        {(n) => (
          <>
            <Show when={n.id === "vaults"}><div class="nav-sep" /></Show>
            <button class="nav-item" classList={{ active: activeTab() === "home" && section() === n.id }}
              onClick={() => { setSection(n.id); setActiveTab("home"); }}>
              <Icon name={n.icon} />
              <span>{n.label}</span>
              <Show when={count(n.id)}><span class="nav-count">{count(n.id)}</span></Show>
            </button>
          </>
        )}
      </For>
      <div class="sidebar-footer">
        <div class="sync-line" title={syncError() ?? ""}>
          <span class="dot" classList={{ busy: syncing(), err: !!syncError() }} />
          <span>{syncing() ? "Sincronizando…" : syncError() ? "Falha ao sincronizar" : lastSync() ? "Sincronizado" : "Conectado"}</span>
        </div>
        <div class="account-chip">
          <div class="avatar" style={{ width: "30px", height: "30px", "border-radius": "8px", display: "grid", "place-items": "center",
            background: colorFor(status()?.email ?? ""), "font-size": "12px", "font-weight": 650 }}>{initials(status()?.email ?? "?")}</div>
          <div class="meta">
            <div class="email">{status()?.email}</div>
            <div class="sync-line">{status()?.server_url?.replace(/^https?:\/\//, "")}</div>
          </div>
          <button class="icon-btn" title="Bloquear (Ctrl+L)" onClick={lock}><Icon name="lock" size={16} /></button>
        </div>
      </div>
    </aside>
  );
}

function TabBar() {
  const close = async (id: string, e: MouseEvent) => {
    e.stopPropagation();
    await api.closeTerminal(id);
    await refreshSessions();
    if (activeTab() === id) setActiveTab(sessions().at(-1)?.id ?? "home");
  };
  const profiles = itemsOf("terminal_profile");
  const quickOpen = () => {
    const def = settings()?.default_profile_id;
    const p = profiles().find((x) => x.id === def) ?? profiles()[0];
    if (p) openProfile(p.id);
    else setSection("terminals");
  };
  return (
    <div class="tabbar">
      <button class="tab tab-home" classList={{ active: activeTab() === "home" }} onClick={() => setActiveTab("home")} title="Início">
        <Icon name="vault" size={16} />
      </button>
      <For each={sessions()}>
        {(s) => (
          <div class="tab" classList={{ active: activeTab() === s.id }} onClick={() => setActiveTab(s.id)} role="tab">
            <span class="state" classList={{ dead: !s.alive }} />
            <Icon name={s.kind.kind === "ssh" ? "server" : "terminal"} size={14} />
            <span class="title" title={s.kind.kind === "ssh" ? s.kind.address : s.kind.shell}>{s.title}</span>
            <Show when={s.by_agent}><span class="agent-flag" title="Aberto por um agente de IA">IA</span></Show>
            <button class="close" onClick={(e) => close(s.id, e)} title="Fechar"><Icon name="x" size={13} /></button>
          </div>
        )}
      </For>
      <button class="tab tab-new" onClick={quickOpen} title="Novo terminal (Ctrl+T)"><Icon name="plus" size={16} /></button>
    </div>
  );
}

function Palette(props: { onClose: () => void }) {
  const [query, setQuery] = createSignal("");
  const [sel, setSel] = createSignal(0);
  const hosts = itemsOf("host");
  const profiles = itemsOf("terminal_profile");
  type Entry = { label: string; hint: string; icon: string; run: () => void };
  const entries = createMemo<Entry[]>(() => {
    const q = query().toLowerCase();
    const all: Entry[] = [
      ...hosts().map((h) => ({
        label: h.entity.data.label || h.entity.data.address, hint: h.entity.data.address, icon: "server", run: () => connectHost(h.id),
      })),
      ...profiles().map((p) => ({ label: p.entity.data.label, hint: shellLabel(p.entity.data.shell), icon: "terminal", run: () => openProfile(p.id) })),
      ...nav.map((n) => ({ label: n.label, hint: "ir para", icon: n.icon, run: () => { setSection(n.id); setActiveTab("home"); } })),
    ];
    return (q ? all.filter((e) => `${e.label} ${e.hint}`.toLowerCase().includes(q)) : all).slice(0, 40);
  });
  const run = (e?: Entry) => {
    if (!e) return;
    props.onClose();
    e.run();
  };
  return (
    <div class="palette" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="palette-box">
        <input autofocus placeholder="Conectar a um host, abrir um terminal, ir para…" value={query()}
          onInput={(e) => { setQuery(e.currentTarget.value); setSel(0); }}
          onKeyDown={(e) => {
            if (e.key === "Escape") props.onClose();
            if (e.key === "ArrowDown") { e.preventDefault(); setSel((s) => Math.min(s + 1, entries().length - 1)); }
            if (e.key === "ArrowUp") { e.preventDefault(); setSel((s) => Math.max(s - 1, 0)); }
            if (e.key === "Enter") run(entries()[sel()]);
          }} />
        <div class="palette-list">
          <For each={entries()}>
            {(e, i) => (
              <div class="palette-item" classList={{ sel: i() === sel() }} onMouseEnter={() => setSel(i())} onClick={() => run(e)}>
                <Icon name={e.icon} size={16} /><span>{e.label}</span><span class="hint">{e.hint}</span>
              </div>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}

export function Workspace() {
  const [palette, setPalette] = createSignal(false);
  const profiles = itemsOf("terminal_profile");
  // Strings compare by value, so terminal views survive refreshes of the session list.
  const sessionIds = createMemo(() => sessions().map((s) => s.id));

  // Auto-lock after inactivity.
  let last = Date.now();
  const touch = () => (last = Date.now());
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      touch();
      if (e.ctrlKey && e.code === "KeyK") { e.preventDefault(); setPalette((p) => !p); }
      if (e.ctrlKey && e.code === "KeyL" && !e.shiftKey && activeTab() === "home") { e.preventDefault(); api.lock().then(setStatus); }
      if (e.ctrlKey && e.code === "KeyT") {
        e.preventDefault();
        const p = profiles().find((x) => x.id === settings()?.default_profile_id) ?? profiles()[0];
        if (p) openProfile(p.id);
      }
    };
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("mousemove", touch);
    const timer = setInterval(async () => {
      const minutes = settings()?.lock_after_minutes ?? 15;
      if (minutes > 0 && Date.now() - last > minutes * 60_000) setStatus(await api.lock());
    }, 20_000);
    onCleanup(() => {
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("mousemove", touch);
      clearInterval(timer);
    });
  });

  return (
    <div class="app">
      <Sidebar />
      <main class="main">
        <TabBar />
        <div class="content">
          <Show when={activeTab() === "home"}>
            <div class="page" ref={(el) => createEffect(on(section, () => (el.scrollTop = 0)))}>
              <Switch>
                <Match when={section() === "hosts"}><HostsView /></Match>
                <Match when={section() === "terminals"}><ProfilesView /></Match>
                <Match when={section() === "keychain"}><KeychainView /></Match>
                <Match when={section() === "snippets"}><SnippetsView /></Match>
                <Match when={section() === "forwards"}><ForwardsView /></Match>
                <Match when={section() === "vaults"}><VaultsView /></Match>
                <Match when={section() === "agents"}><AgentsView /></Match>
                <Match when={section() === "settings"}><SettingsView /></Match>
              </Switch>
            </div>
          </Show>
          <For each={sessionIds()}>{(id) => <TerminalView id={id} />}</For>
        </div>
      </main>
      <Show when={palette()}><Palette onClose={() => setPalette(false)} /></Show>
      <HostKeyPrompt />
    </div>
  );
}
