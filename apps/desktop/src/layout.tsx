import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, onMount, type JSX } from "solid-js";
import { api, shellLabel, type Shell } from "./api";
import {
  activeTab, itemsOf, lastSync, ownProfiles, section, sessions, setActiveTab, setSection, setStatus, settings, status, syncError, syncing,
  refreshSessions, type Section,
} from "./store";
import { HostKeyPrompt, TerminalView, connectHost, openProfile } from "./terminal";
import { Icon, ShellIcon, colorFor, initials } from "./ui";
import { Brand } from "./auth";
import { exitCopy } from "./motion";
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
      <Brand sub={status()?.local ? "Só neste computador" : "Sincronizado"} />
      <For each={nav}>
        {(n) => (
          <>
            <Show when={n.id === "vaults"}><div class="nav-sep" /></Show>
            <button class="nav-item" classList={{ active: activeTab() === "home" && section() === n.id }}
              aria-current={activeTab() === "home" && section() === n.id ? "page" : undefined}
              onClick={() => { setSection(n.id); setActiveTab("home"); }}>
              <Icon name={n.icon} />
              <span>{n.label}</span>
              <Show when={count(n.id)}><span class="nav-count">{count(n.id)}</span></Show>
            </button>
          </>
        )}
      </For>
      <div class="sidebar-footer">
        <Show when={!status()?.local} fallback={
          <button class="sync-line link-like" onClick={() => { setSection("settings"); setActiveTab("home"); }} title="Conectar a um servidor">
            <span class="dot off" />
            <span>Sem sincronização</span>
          </button>
        }>
          <div class="sync-line" title={syncError() ?? ""}>
            <span class="dot" classList={{ busy: syncing(), err: !!syncError() }} />
            <span>{syncing() ? "Sincronizando…" : syncError() ? "Falha ao sincronizar" : lastSync() ? "Sincronizado" : "Conectado"}</span>
          </div>
        </Show>
        <div class="account-chip">
          <div class="avatar" style={{ width: "30px", height: "30px", "border-radius": "8px", display: "grid", "place-items": "center",
            background: colorFor(status()?.email ?? ""), "font-size": "12px", "font-weight": 650 }}>{initials(status()?.email ?? "?")}</div>
          <div class="meta">
            <div class="email">{status()?.email}</div>
            <div class="sync-line" style={{ padding: 0 }}>{status()?.local ? "Este computador" : status()?.server_url?.replace(/^https?:\/\//, "")}</div>
          </div>
          <button class="icon-btn" aria-label="Bloquear (Ctrl+L)" title="Bloquear (Ctrl+L)" onClick={lock}><Icon name="lock" size={16} /></button>
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
  const quickOpen = () => {
    const def = settings()?.default_profile_id;
    const own = ownProfiles();
    const p = own.find((x) => x.id === def) ?? own[0];
    if (p) openProfile(p.id);
    else setSection("terminals");
  };
  return (
    <div class="tabbar" role="tablist" aria-label="Abas">
      <button class="tab tab-home" role="tab" aria-selected={activeTab() === "home"} classList={{ active: activeTab() === "home" }}
        onClick={() => setActiveTab("home")} title="Início" aria-label="Início">
        <Icon name="vault" size={16} />
      </button>
      {/* Keyed by id (strings compare by value), so refreshing the list does not rebuild tabs. */}
      <For each={sessions().map((x) => x.id)}>
        {(id) => {
          const s = () => sessions().find((x) => x.id === id);
          return (
            <Show when={s()}>
              {(s) => (
                <div class="tab" classList={{ active: activeTab() === id }} onClick={() => setActiveTab(id)} role="tab"
                  aria-selected={activeTab() === id} tabIndex={0} onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && setActiveTab(id)}>
                  <span class="state" classList={{ dead: !s().alive }} />
                  <ShellIcon ssh={s().kind.kind === "ssh"} shell={s().kind.kind === "local" ? (s().kind as { shell_kind?: Shell }).shell_kind : null} size={16} />
                  <span class="title" title={s().kind.kind === "ssh" ? (s().kind as { address: string }).address : (s().kind as { shell: string }).shell}>{s().title}</span>
                  <Show when={s().by_agent}><span class="agent-flag" title="Aberto por um agente de IA">IA</span></Show>
                  <button class="close" onClick={(e) => close(id, e)} title="Fechar" aria-label={`Fechar ${s().title}`}><Icon name="x" size={13} /></button>
                </div>
              )}
            </Show>
          );
        }}
      </For>
      <button class="tab tab-new" onClick={quickOpen} title="Novo terminal (Ctrl+T)" aria-label="Novo terminal"><Icon name="plus" size={16} /></button>
    </div>
  );
}

function Palette(props: { onClose: () => void }) {
  let paletteEl: HTMLDivElement | undefined;
  onCleanup(() => exitCopy(paletteEl, "leaving", 130));
  const [query, setQuery] = createSignal("");
  const [sel, setSel] = createSignal(0);
  const hosts = itemsOf("host");
  const profiles = itemsOf("terminal_profile");
  type Entry = { label: string; hint: string; icon: string; badge?: () => JSX.Element; run: () => void };
  const entries = createMemo<Entry[]>(() => {
    const q = query().toLowerCase();
    const all: Entry[] = [
      ...hosts().map((h) => ({
        label: h.entity.data.label || h.entity.data.address, hint: h.entity.data.address, icon: "server", badge: () => <ShellIcon ssh size={18} />, run: () => connectHost(h.id),
      })),
      ...profiles().map((p) => ({ label: p.entity.data.label, hint: shellLabel(p.entity.data.shell), icon: "terminal", badge: () => <ShellIcon shell={p.entity.data.shell} size={18} />, run: () => openProfile(p.id) })),
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
    <div class="palette" ref={paletteEl} onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="palette-box">
        <input autofocus placeholder="Conectar a um host, abrir um terminal, ir para…" value={query()}
          role="combobox" aria-label="Buscar comando" aria-expanded="true" aria-controls="palette-list" aria-autocomplete="list"
          aria-activedescendant={entries().length ? `palette-${sel()}` : undefined}
          onInput={(e) => { setQuery(e.currentTarget.value); setSel(0); }}
          onKeyDown={(e) => {
            if (e.key === "Escape") props.onClose();
            if (e.key === "ArrowDown") { e.preventDefault(); setSel((s) => Math.min(s + 1, entries().length - 1)); }
            if (e.key === "ArrowUp") { e.preventDefault(); setSel((s) => Math.max(s - 1, 0)); }
            if (e.key === "Enter") run(entries()[sel()]);
          }} />
        <div class="palette-list" id="palette-list" role="listbox" aria-label="Resultados">
          <For each={entries()} fallback={<div class="palette-empty">Nada encontrado para essa busca.</div>}>
            {(e, i) => (
              <div class="palette-item" id={`palette-${i()}`} role="option" aria-selected={i() === sel()} classList={{ sel: i() === sel() }}
                onMouseEnter={() => setSel(i())} onClick={() => run(e)}>
                {e.badge ? e.badge() : <Icon name={e.icon} size={16} />}<span>{e.label}</span><span class="hint">{e.hint}</span>
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
        const own = ownProfiles();
        const p = own.find((x) => x.id === settings()?.default_profile_id) ?? own[0];
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
      <a class="skip-link" href="#content" onClick={(e) => { e.preventDefault(); document.getElementById("content")?.focus(); }}>Ir para o conteúdo</a>
      <Sidebar />
      <main class="main">
        <TabBar />
        <div class="content" id="content" tabIndex={-1}>
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
