import { For, Show, createEffect, createMemo, createResource, createSignal, on, onCleanup } from "solid-js";
import {
  AWS_ALL, api, errorMessage, on as onEvent, type AwsError, type AwsFavorite, type AwsInstance, type AwsInventory, type AwsOverview,
  type AwsProfile, type AwsProfileNode, type AwsProgress, type AwsRegionNode, type AwsSettings,
} from "../api";
import { copy, refreshSessions, setActiveTab, toast } from "../store";
import { withHostKeyCheck } from "../terminal";
import { Badge, Button, Empty, Field, Icon, Modal, colorFor, confirmAction, initials } from "../ui";
import { ItemSelect } from "./common";
import { BLANK, PORT_MSG, intIn, invalid, isPort } from "../validate";

const COLS = 120;
const ROWS = 32;
/** Rows drawn per client before "Mostrar mais": a large account stays responsive. */
const PAGE = 100;

const CLI_URL = "https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html";
const PLUGIN_URL = "https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html";

// Live state of a mapping, kept while the screen is closed (the events keep coming).
const [progress, setProgress] = createSignal<AwsProgress | null>(null);
const [live, setLive] = createSignal<AwsInventory | null>(null);
const [mapping, setMapping] = createSignal(false);
let wired = false;
let reload: (() => void) | null = null;

function wire() {
  if (wired) return;
  wired = true;
  onEvent<AwsProgress>("aws://progress", (p) => setProgress(p));
  // Hundreds of accounts send thousands of region results: they reach the screen in batches.
  let queue: ((inv: AwsInventory | null) => AwsInventory)[] = [];
  let timer: number | undefined;
  const flush = () => {
    timer = undefined;
    const q = queue;
    queue = [];
    if (q.length) setLive((inv) => q.reduce<AwsInventory | null>((acc, f) => f(acc), inv));
  };
  const push = (f: (inv: AwsInventory | null) => AwsInventory) => {
    queue.push(f);
    timer ??= window.setTimeout(flush, 300);
  };
  onEvent<AwsProfileNode>("aws://profile", (node) => push((inv) => upsertProfile(inv, node)));
  onEvent<{ profile: string; node: AwsRegionNode }>("aws://region", ({ profile, node }) => push((inv) => upsertRegion(inv, profile, node)));
  onEvent<{ partial: boolean; count: number }>("aws://done", (d) => {
    flush();
    setMapping(false);
    setProgress(null);
    toast(d.partial ? `Mapeamento cancelado: ${d.count} instâncias guardadas até aqui.` : `Mapeamento concluído: ${d.count} instâncias.`, d.partial ? "info" : "success");
    reload?.();
  });
}

function upsertProfile(inv: AwsInventory | null, node: AwsProfileNode): AwsInventory {
  const base = inv ?? { mapped_at: 0, profiles: [], partial: false };
  const old = base.profiles.find((p) => p.profile === node.profile);
  const merged = { ...node, regions: old?.regions ?? node.regions };
  const profiles = [...base.profiles.filter((p) => p.profile !== node.profile), merged].sort((a, b) => a.profile.localeCompare(b.profile));
  return { ...base, profiles };
}

function upsertRegion(inv: AwsInventory | null, profile: string, node: AwsRegionNode): AwsInventory {
  const base = inv ?? { mapped_at: 0, profiles: [], partial: false };
  const p = base.profiles.find((x) => x.profile === profile) ?? { profile, account: null, role_arn: null, regions: [], error: null };
  const regions = [...p.regions.filter((r) => r.region !== node.region), node].sort((a, b) => a.region.localeCompare(b.region));
  return upsertProfile(base, { ...p, regions });
}

function ago(ts: number): string {
  const s = Math.max(0, Date.now() / 1000 - ts);
  if (s < 60) return "agora há pouco";
  if (s < 3600) return `há ${Math.round(s / 60)} min`;
  if (s < 86400) return `há ${Math.round(s / 3600)} h`;
  const d = Math.round(s / 86400);
  return d === 1 ? "há 1 dia" : `há ${d} dias`;
}

function elapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}


/** Why the SSM actions of an instance are off, or null when they are available. */
function unavailable(i: AwsInstance, o: AwsOverview | undefined): string | null {
  if (!o?.tools.cli) return "O AWS CLI não está instalado.";
  if (!o.tools.plugin) return "O Session Manager Plugin não está instalado.";
  if (i.ssm === "unmanaged") return "Esta instância não é gerenciada pelo Session Manager (sem o agente SSM, ou sem o papel de instância).";
  if (i.ssm === "offline") return "O agente SSM desta instância não está respondendo.";
  if (i.state && i.state !== "running") return `A instância está ${i.state === "stopped" ? "parada" : i.state}.`;
  return null;
}

function errorHint(e: AwsError): string | null {
  switch (e.kind) {
    case "session_expired": return "Use “Entrar na AWS” e mapeie de novo.";
    case "access_denied": return "Peça a permissão a quem administra a conta.";
    case "assume_role_denied": return "Confira a relação de confiança do papel com o perfil mestre.";
    default: return null;
  }
}

// ---------------------------------------------------------------------------------------------
// Clients: one per AWS account (or per profile when the account is unknown). Profiles that reach
// the same account merge into one client, and each instance shows once.
// ---------------------------------------------------------------------------------------------

interface Target { profile: string; region: string; instance: AwsInstance }
interface Row extends Target { client: string }
interface Client {
  key: string;
  name: string;
  account: string | null;
  /** Profiles that reached it without errors. */
  profiles: string[];
  /** The profile its sessions use. */
  access: string;
  rows: Row[];
}
interface Problem { key: string; client: string; profile: string; region: string | null; error: AwsError }

const accountKey = (account: string | null, profile: string) => account ?? `profile:${profile}`;

/** The shared start of several profile names ("acme-prod" for "acme-prod-admin" and
 * "acme-prod-readonly"), or null when they share too little to name the client. */
function commonName(names: string[]): string | null {
  if (names.length < 2) return null;
  let prefix = names[0];
  for (const n of names.slice(1)) while (!n.startsWith(prefix)) prefix = prefix.slice(0, -1);
  // Cut at a separator, so "acme-prod-a" from "acme-prod-admin"/"acme-prod-audit" stays "acme-prod".
  const whole = names.every((n) => n.length === prefix.length || /[-_./ ]/.test(n[prefix.length]));
  if (!whole) prefix = prefix.slice(0, Math.max(...[..."-_./ "].map((c) => prefix.lastIndexOf(c))) + 1);
  prefix = prefix.replace(/[-_./ ]+$/, "");
  return prefix.length >= 3 ? prefix : null;
}

function buildClients(inv: AwsInventory | null, config: AwsProfile[], settings: AwsSettings) {
  type Acc = { key: string; account: string | null; profiles: Set<string>; seen: Map<string, Target[]>; problems: Omit<Problem, "client">[] };
  const byKey = new Map<string, Acc>();
  for (const node of inv?.profiles ?? []) {
    const account = node.account ?? config.find((p) => p.name === node.profile)?.account ?? null;
    const key = accountKey(account, node.profile);
    const acc: Acc = byKey.get(key) ?? { key, account, profiles: new Set<string>(), seen: new Map<string, Target[]>(), problems: [] };
    byKey.set(key, acc);
    if (node.error) {
      acc.problems.push({ key, profile: node.profile, region: null, error: node.error });
      continue;
    }
    acc.profiles.add(node.profile);
    for (const r of node.regions) {
      if (r.error && r.error.kind !== "cancelled") acc.problems.push({ key, profile: node.profile, region: r.region, error: r.error });
      for (const instance of r.instances) {
        const id = `${r.region}|${instance.id}`;
        const list = acc.seen.get(id) ?? [];
        list.push({ profile: node.profile, region: r.region, instance });
        acc.seen.set(id, list);
      }
    }
  }
  const clients: Client[] = [];
  const problems: Problem[] = [];
  let hidden = 0;
  for (const acc of byKey.values()) {
    const prefs = settings.clients[acc.key];
    // Every profile of the account, mapped or not: the mapping signs in with one of them.
    const profiles = [...new Set([...acc.profiles, ...(acc.account ? config.filter((p) => p.account === acc.account && !p.problem).map((p) => p.name) : [])])].sort();
    const mapped = [...acc.profiles];
    const access = prefs?.profile && profiles.includes(prefs.profile) ? prefs.profile : mapped.sort()[0] ?? profiles[0] ?? acc.problems[0]?.profile ?? "";
    const name = prefs?.name || commonName(profiles) || access || acc.key;
    for (const p of acc.problems) problems.push({ ...p, client: name });
    const rows: Row[] = [...acc.seen.values()]
      .map((candidates) => ({ ...(candidates.find((c) => c.profile === access) ?? candidates[0]), client: name }))
      .sort((a, b) => (a.instance.name || "￿").localeCompare(b.instance.name || "￿") || a.instance.id.localeCompare(b.instance.id));
    if (rows.length) clients.push({ key: acc.key, name, account: acc.account, profiles, access, rows });
    else if (!acc.problems.length) hidden++;
  }
  clients.sort((a, b) => a.name.localeCompare(b.name));
  return { clients, problems, hidden };
}

/** Clients start open when there are few, closed when there are many; a click is remembered. */
const OPEN_KEY = "vessel.aws.open";
const OPEN_BY_DEFAULT_UP_TO = 6;
function loadOpen(): Record<string, boolean> {
  try { return JSON.parse(localStorage.getItem(OPEN_KEY) ?? "{}") ?? {}; } catch { return {}; }
}
function saveOpen(o: Record<string, boolean>) {
  try { localStorage.setItem(OPEN_KEY, JSON.stringify(o)); } catch { /* per-viewer convenience only */ }
}

export function AwsView() {
  wire();
  const [overview, { refetch: refetchOverview, mutate: mutateOverview }] = createResource(() => api.awsOverview());
  const [inventory, { refetch: refetchInventory }] = createResource(async () => {
    const inv = await api.awsInventory();
    setLive(inv);
    return inv;
  });
  reload = () => { refetchOverview(); refetchInventory(); };
  onCleanup(() => { reload = null; });

  const [session, setSession] = createSignal<{ active: boolean; error: AwsError | null } | "checking" | null>(null);
  const [search, setSearch] = createSignal("");
  const [onlineOnly, setOnlineOnly] = createSignal(false);
  const [clientFilter, setClientFilter] = createSignal("");
  const [regionFilter, setRegionFilter] = createSignal("");
  const [ssh, setSsh] = createSignal<Target | null>(null);
  const [forward, setForward] = createSignal<Target | null>(null);
  const [editing, setEditing] = createSignal<Client | null>(null);
  const [regionsOf, setRegionsOf] = createSignal<string | null>(null);
  const [showProblems, setShowProblems] = createSignal(false);
  const [opened, setOpened] = createSignal(loadOpen());

  const settings = (): AwsSettings => ({ master: null, regions: {}, favorites: [], clients: {}, ...overview()?.settings });
  const master = () => overview()?.master ?? null;
  const profiles = () => overview()?.profiles ?? [];
  const masters = () => profiles().filter((p) => p.master === p.name);
  const inv = () => live() ?? inventory() ?? null;
  const isMapping = () => mapping() || !!overview()?.mapping;
  const built = createMemo(() => buildClients(inv(), profiles(), settings()));

  const saveSettings = async (next: AwsSettings) => {
    try {
      const saved = await api.awsSaveSettings(next);
      const o = overview();
      if (o) mutateOverview({ ...o, settings: saved, master: saved.master ?? o.master });
      return true;
    } catch (e) {
      toast(errorMessage(e), "error");
      return false;
    }
  };

  const checkSession = async () => {
    const m = master();
    if (!m || m === AWS_ALL || !overview()?.tools.cli) return setSession(null);
    setSession("checking");
    try { setSession(await api.awsSessionStatus(m)); } catch (e) { setSession({ active: false, error: { kind: "other", message: errorMessage(e) } }); }
  };
  // Checked once the overview is in, and again when the master changes.
  createEffect(on(() => (overview() ? master() : undefined), (m) => { if (m) void checkSession(); }));

  const login = async () => {
    const m = master();
    if (!m || m === AWS_ALL) return invalid("Perfil mestre", "Escolha o perfil mestre para entrar na AWS.");
    try {
      const s = await api.awsLogin(m, COLS, ROWS);
      await refreshSessions();
      setActiveTab(s.id);
      toast("Siga as instruções na aba aberta. Depois, volte aqui e verifique a sessão.", "info");
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const mapAll = async () => {
    const m = master();
    if (!m) return invalid("Perfil mestre", "Escolha o perfil mestre (ou todos os perfis) antes de mapear.");
    const family = m === AWS_ALL ? profiles() : profiles().filter((p) => p.master === m);
    const accounts = new Set(family.map((p) => p.account ?? `profile:${p.name}`)).size;
    if (!(await confirmAction({
      title: "Mapear todas as contas?",
      message: `Vai consultar as regiões habilitadas de ${accounts} ${accounts === 1 ? "conta" : "contas"} (${family.length} ${family.length === 1 ? "perfil" : "perfis"}; perfis da mesma conta contam uma vez). Você pode cancelar a qualquer momento. As credenciais temporárias ficam só na memória durante o mapeamento.`,
      confirm: "Mapear",
      danger: false,
    }))) return;
    await start({ master: m });
  };

  const start = async (opts: { master?: string; profile?: string; region?: string }) => {
    try {
      setProgress(null);
      setMapping(true);
      await api.awsMap(opts);
    } catch (e) {
      setMapping(false);
      toast(errorMessage(e), "error");
    }
  };

  const sameFav = (f: AwsFavorite, t: Target) => f.id === t.instance.id && f.region === t.region;
  const isFavorite = (t: Target) => settings().favorites.some((f) => sameFav(f, t));
  const toggleFavorite = (t: Target) => {
    const s = settings();
    void saveSettings({
      ...s,
      favorites: isFavorite(t) ? s.favorites.filter((f) => !sameFav(f, t)) : [...s.favorites, { profile: t.profile, region: t.region, id: t.instance.id }],
    });
  };

  const query = () => search().trim().toLowerCase();
  const filtering = () => !!query() || onlineOnly() || !!regionFilter() || !!clientFilter();
  const rowMatches = (r: Row, clientHit: boolean) => {
    const i = r.instance;
    if (onlineOnly() && i.ssm !== "online") return false;
    if (regionFilter() && r.region !== regionFilter()) return false;
    const q = query();
    return !q || clientHit || i.name.toLowerCase().includes(q) || i.id.toLowerCase().includes(q) || i.private_ip.includes(q);
  };
  const shown = createMemo(() =>
    built().clients
      .filter((c) => !clientFilter() || c.key === clientFilter())
      .map((c) => {
        const hit = !!query() && c.name.toLowerCase().includes(query());
        return { client: c, rows: c.rows.filter((r) => rowMatches(r, hit)) };
      })
      .filter((x) => x.rows.length),
  );
  const totals = createMemo(() => {
    const rows = built().clients.flatMap((c) => c.rows);
    return { clients: built().clients.length, instances: rows.length, online: rows.filter((r) => r.instance.ssm === "online").length };
  });
  const allRegions = createMemo(() => [...new Set(built().clients.flatMap((c) => c.rows.map((r) => r.region)))].sort());
  const favorites = createMemo(() => {
    const rows = built().clients.flatMap((c) => c.rows);
    return settings().favorites.flatMap((f) => rows.filter((r) => r.instance.id === f.id && r.region === f.region).slice(0, 1));
  });

  const isOpen = (key: string) => opened()[key] ?? built().clients.length <= OPEN_BY_DEFAULT_UP_TO;
  const setAllOpen = (open: boolean) => {
    const next = { ...opened() };
    for (const c of built().clients) next[c.key] = open;
    setOpened(next);
    saveOpen(next);
  };
  const toggleOpen = (key: string) => {
    const next = { ...opened(), [key]: !isOpen(key) };
    setOpened(next);
    saveOpen(next);
  };

  const openShell = async (t: Target) => {
    try {
      const s = await api.awsOpenSession(t.profile, t.region, t.instance.id, COLS, ROWS);
      await refreshSessions();
      setActiveTab(s.id);
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const setAgents = async (on: boolean, box: HTMLInputElement) => {
    if (on && !(await confirmAction({
      title: "Liberar a AWS para agentes de IA?",
      message: "Os agentes conectados ao Vessel (MCP) vão poder listar as instâncias do último mapeamento e abrir sessões do Session Manager nelas, numa aba que você vê e pode fechar. Eles não podem mapear nem entrar na AWS.",
      confirm: "Liberar",
    }))) { box.checked = false; return; }
    try {
      await api.awsSetAgents(on);
      const o = overview();
      if (o) mutateOverview({ ...o, agents_allowed: on });
    } catch (e) {
      box.checked = !on;
      toast(errorMessage(e), "error");
    }
  };

  const actions = { onShell: (t: Target) => void openShell(t), onSsh: setSsh, onForward: setForward, onFavorite: toggleFavorite, isFavorite };

  return (
    <>
      <div class="page-head">
        <h1>AWS</h1>
        <div class="spacer" />
        <Show when={overview()?.mapped_at}>
          {(t) => <span class="aws-muted" title={new Date(t() * 1000).toLocaleString("pt-BR")}>Mapeado {ago(t())}{overview()?.partial ? ", incompleto" : ""}</span>}
        </Show>
      </div>

      <Show when={overview()} fallback={<p class="aws-muted">{overview.loading ? "Procurando o AWS CLI e os perfis…" : overview.error ? errorMessage(overview.error) : ""}</p>}>
        {(o) => (
          <>
            <Tools overview={o()} onRecheck={() => refetchOverview()} />
            <Show when={o().config_error}>{(e) => <div class="callout callout-warn"><Icon name="alert" /><span>{e()}</span></div>}</Show>

            <Show when={o().tools.cli && profiles().length === 0 && !o().config_error}>
              <Empty icon="cloud" title="Nenhum perfil da AWS neste computador">
                <span>O Vessel lê os perfis de <span class="mono">{o().config_path ?? "~/.aws/config"}</span>. Configure um com <span class="mono">aws configure sso</span> ou <span class="mono">aws login</span>.</span>
              </Empty>
            </Show>

            <Show when={profiles().length > 0}>
              <div class="panel aws-top">
                <div class="aws-top-row">
                  <Field label="Perfil mestre">
                    <select value={master() ?? ""} onChange={(e) => void saveSettings({ ...settings(), master: e.currentTarget.value || null })}>
                      <For each={masters()}>{(p) => {
                        const deps = () => profiles().filter((x) => x.master === p.name && x.name !== p.name).length;
                        return <option value={p.name}>{p.name}{deps() ? ` (+${deps()} ${deps() === 1 ? "perfil" : "perfis"})` : ""}</option>;
                      }}</For>
                      <option value={AWS_ALL}>Todos os perfis</option>
                    </select>
                  </Field>
                  <div class="aws-session" aria-live="polite">
                    <Show when={master() !== AWS_ALL} fallback={<span class="aws-muted">Cada perfil entra por conta própria.</span>}>
                      <Show when={session()} keyed>
                        {(s) => s === "checking"
                          ? <span class="aws-muted"><span class="spinner" /> Verificando a sessão…</span>
                          : s.active
                            ? <Badge tone="green">Sessão ativa</Badge>
                            : <span class="aws-session-bad"><Badge tone="amber">{s.error?.kind === "session_expired" ? "Sessão expirada" : "Sem sessão"}</Badge>
                                <Show when={s.error && s.error.kind !== "session_expired"}><span class="aws-muted">{s.error!.message}</span></Show></span>}
                      </Show>
                      <button class="icon-btn" aria-label="Verificar a sessão" title="Verificar a sessão" onClick={() => void checkSession()}><Icon name="sync" size={14} /></button>
                    </Show>
                  </div>
                  <div class="spacer" />
                  <Button icon="user" onClick={() => void login()} disabled={!o().tools.cli || master() === AWS_ALL}
                    title={master() === AWS_ALL ? "Escolha um perfil mestre para entrar" : "Abre o login do AWS CLI numa aba"}>Entrar na AWS</Button>
                  <Show when={isMapping()} fallback={<Button variant="primary" icon="search" onClick={() => void mapAll()} disabled={!o().tools.cli}>Mapear tudo</Button>}>
                    <Button variant="danger" icon="stop" onClick={() => void api.awsCancel()}>Cancelar</Button>
                  </Show>
                </div>
                <Show when={isMapping()}>
                  <div class="aws-progress" role="status">
                    <div class="aws-bar"><div style={{ width: `${progress() && progress()!.regions_total ? Math.round((progress()!.regions_done / progress()!.regions_total) * 100) : 4}%` }} /></div>
                    <span>{progress()
                      ? `Conta ${progress()!.accounts_done} de ${progress()!.accounts_total}, região ${progress()!.regions_done} de ${progress()!.regions_total}`
                      : "Pegando as credenciais dos perfis…"}</span>
                    <Show when={progress()}><span class="aws-muted mono">{elapsed(progress()!.elapsed_ms)}</span></Show>
                  </div>
                </Show>
              </div>

              <Show when={built().problems.length}>
                <div class="callout callout-warn aws-problems" role="status"><Icon name="alert" />
                  <div class="aws-problems-body">
                    <div class="aws-problems-head">
                      <span>{built().problems.length === 1 ? "Um perfil ou região não pôde ser mapeado." : `${built().problems.length} perfis ou regiões não puderam ser mapeados.`}</span>
                      <button class="link-like" aria-expanded={showProblems()} onClick={() => setShowProblems(!showProblems())}>{showProblems() ? "Esconder" : "Ver detalhes"}</button>
                    </div>
                    <Show when={showProblems()}>
                      <ul class="aws-problem-list">
                        <For each={built().problems}>{(p) => (
                          <li>
                            <div class="aws-problem-where"><b>{p.client}</b><span class="aws-muted">{p.region ? `${p.profile}, ${p.region}` : p.profile}</span></div>
                            <div class="aws-problem-what">{p.error.message}<Show when={errorHint(p.error)}>{(h) => <span class="aws-muted"> {h()}</span>}</Show></div>
                            <Button small icon="sync" disabled={isMapping()} onClick={() => void start({ profile: p.profile, region: p.region ?? undefined })}>Tentar de novo</Button>
                          </li>
                        )}</For>
                      </ul>
                    </Show>
                  </div>
                </div>
              </Show>

              <Show when={built().clients.length} fallback={
                <Show when={!isMapping()}>
                  <Empty icon="cloud" title={inv()?.profiles.length ? "Nenhuma instância encontrada" : "Nada mapeado ainda"}>
                    <span>{inv()?.profiles.length
                      ? "O último mapeamento não encontrou instâncias nas contas e regiões consultadas."
                      : "“Mapear tudo” consulta as instâncias de todas as contas e regiões do perfil mestre. O resultado fica guardado, cifrado, só neste computador."}</span>
                  </Empty>
                </Show>
              }>
                <div class="aws-summary" aria-label="Resumo">
                  <span><b>{totals().clients}</b> {totals().clients === 1 ? "cliente" : "clientes"}</span>
                  <span><b>{totals().instances}</b> {totals().instances === 1 ? "instância" : "instâncias"}</span>
                  <span><span class="aws-dot online" /><b>{totals().online}</b> online no SSM</span>
                  <Show when={built().clients.length > 1}>
                    <span class="aws-summary-tools">
                      <button class="link-like" onClick={() => setAllOpen(true)}>Expandir todos</button>
                      <button class="link-like" onClick={() => setAllOpen(false)}>Recolher todos</button>
                    </span>
                  </Show>
                </div>

                <Show when={favorites().length}>
                  <section class="aws-client aws-favorites">
                    <div class="aws-client-head static">
                      <span class="aws-client-icon"><Icon name="star" size={16} /></span>
                      <div class="aws-client-title"><span class="name">Favoritas</span><span class="aws-muted">Também servem de destino para snippets</span></div>
                    </div>
                    <InstanceTable rows={favorites()} overview={o()} showClient {...actions} />
                  </section>
                </Show>

                <div class="aws-filters">
                  <div class="search aws-search"><Icon name="search" size={16} />
                    <input type="search" placeholder="Buscar cliente, instância, ID ou IP…" aria-label="Buscar instâncias" value={search()} onInput={(e) => setSearch(e.currentTarget.value)} />
                  </div>
                  <Show when={built().clients.length > 1}>
                    <select aria-label="Filtrar por cliente" value={clientFilter()} onChange={(e) => setClientFilter(e.currentTarget.value)}>
                      <option value="">Todos os clientes</option>
                      <For each={built().clients}>{(c) => <option value={c.key}>{c.name}</option>}</For>
                    </select>
                  </Show>
                  <Show when={allRegions().length > 1}>
                    <select aria-label="Filtrar por região" value={regionFilter()} onChange={(e) => setRegionFilter(e.currentTarget.value)}>
                      <option value="">Todas as regiões</option>
                      <For each={allRegions()}>{(r) => <option value={r}>{r}</option>}</For>
                    </select>
                  </Show>
                  <label class="check"><input type="checkbox" checked={onlineOnly()} onChange={(e) => setOnlineOnly(e.currentTarget.checked)} />Só online no SSM</label>
                </div>

                <Show when={shown().length} fallback={<p class="aws-empty-filter">Nenhuma instância com esses filtros.</p>}>
                  <For each={shown()}>{(x) => (
                    <ClientCard client={x.client} rows={x.rows} total={x.client.rows.length} overview={o()} busy={isMapping()}
                      open={(filtering() && !!query()) || isOpen(x.client.key)} onToggle={() => toggleOpen(x.client.key)}
                      onEdit={() => setEditing(x.client)} onRemap={() => void start({ profile: x.client.access })}
                      onRegion={(r) => setRegionFilter(r)} {...actions} />
                  )}</For>
                </Show>
                <Show when={built().hidden}>
                  <p class="aws-muted aws-footnote">{built().hidden === 1 ? "Uma conta sem instâncias não aparece." : `${built().hidden} contas sem instâncias não aparecem.`}</p>
                </Show>
              </Show>

              <label class="check aws-agents"><input type="checkbox" checked={o().agents_allowed} onChange={(e) => void setAgents(e.currentTarget.checked, e.currentTarget)} />
                Agentes de IA podem listar estas instâncias e abrir sessões nelas (MCP)</label>
            </Show>

            <Show when={ssh()} keyed>{(t) => <SshDialog target={t} onClose={() => setSsh(null)} />}</Show>
            <Show when={forward()} keyed>{(t) => <ForwardDialog target={t} onClose={() => setForward(null)} />}</Show>
            <Show when={editing()} keyed>{(c) => (
              <ClientDialog client={c} limited={settings().regions[c.access]?.length ?? 0}
                onRegions={() => setRegionsOf(c.access)}
                onSave={async (name, profile) => {
                  const clients = { ...settings().clients, [c.key]: { name: name === c.access ? "" : name, profile } };
                  if (await saveSettings({ ...settings(), clients })) setEditing(null);
                }}
                onClose={() => setEditing(null)} />
            )}</Show>
            <Show when={regionsOf()} keyed>{(p) => (
              <RegionsDialog profile={p} known={inv()?.profiles.find((x) => x.profile === p)?.regions.map((r) => r.region) ?? []}
                chosen={settings().regions[p] ?? []}
                onSave={(regions) => { const r = { ...settings().regions }; if (regions.length) r[p] = regions; else delete r[p]; void saveSettings({ ...settings(), regions: r }); setRegionsOf(null); }}
                onClose={() => setRegionsOf(null)} />
            )}</Show>
          </>
        )}
      </Show>
    </>
  );
}

function Tools(props: { overview: AwsOverview; onRecheck: () => void }) {
  const t = () => props.overview.tools;
  return (
    <>
      <Show when={!t().cli}>
        <div class="callout callout-warn aws-tool" role="alert"><Icon name="alert" />
          <div>
            <b>O AWS CLI não foi encontrado neste computador.</b> Ele é usado para entrar na AWS e abrir as sessões; o Vessel não lê nem guarda chaves da AWS.
            <div class="aws-tool-row"><span class="mono">{CLI_URL}</span><Button small icon="copy" onClick={() => void copy(CLI_URL, "Link copiado")}>Copiar link</Button></div>
            <div class="aws-tool-row">Depois de instalar, confira com <span class="mono">aws --version</span> e <Button small icon="sync" onClick={props.onRecheck}>Verificar de novo</Button></div>
          </div>
        </div>
      </Show>
      <Show when={t().cli && !t().plugin}>
        <div class="callout callout-warn aws-tool" role="alert"><Icon name="alert" />
          <div>
            <b>O Session Manager Plugin não foi encontrado.</b> Sem ele o mapeamento funciona, mas as sessões (terminal, SSH e port forward) não abrem.
            <div class="aws-tool-row"><span class="mono">{PLUGIN_URL}</span><Button small icon="copy" onClick={() => void copy(PLUGIN_URL, "Link copiado")}>Copiar link</Button></div>
            <div class="aws-tool-row">Depois de instalar, confira com <span class="mono">session-manager-plugin --version</span> e <Button small icon="sync" onClick={props.onRecheck}>Verificar de novo</Button></div>
          </div>
        </div>
      </Show>
    </>
  );
}

interface RowActions {
  onShell: (t: Target) => void;
  onSsh: (t: Target) => void;
  onForward: (t: Target) => void;
  onFavorite: (t: Target) => void;
  isFavorite: (t: Target) => boolean;
}

function ClientCard(props: RowActions & {
  client: Client;
  rows: Row[];
  total: number;
  overview: AwsOverview;
  busy: boolean;
  open: boolean;
  onToggle: () => void;
  onEdit: () => void;
  onRemap: () => void;
  onRegion: (r: string) => void;
}) {
  const online = () => props.client.rows.filter((r) => r.instance.ssm === "online").length;
  const regions = createMemo(() => {
    const m = new Map<string, number>();
    for (const r of props.client.rows) m.set(r.region, (m.get(r.region) ?? 0) + 1);
    return [...m.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  });
  const sub = () => {
    const parts = [props.client.account ? `Conta ${props.client.account}` : "Conta não identificada"];
    if (props.client.profiles.length > 1 || props.client.name !== props.client.access) parts.push(`acesso por ${props.client.access}`);
    return parts.join(", ");
  };
  return (
    <section class="aws-client" aria-label={`Cliente ${props.client.name}`}>
      <div class="aws-client-head">
        <button class="aws-client-toggle" aria-expanded={props.open} onClick={props.onToggle} title={props.open ? "Recolher" : "Expandir"}>
          <span class="aws-client-icon" style={{ background: colorFor(props.client.name) }}>{initials(props.client.name)}</span>
          <span class="aws-client-title">
            <span class="name">{props.client.name}</span>
            <span class="aws-muted">{sub()}</span>
          </span>
        </button>
        <div class="aws-client-stats">
          <span>{props.rows.length === props.total ? props.total : `${props.rows.length} de ${props.total}`} {props.total === 1 ? "instância" : "instâncias"}</span>
          <span title="Online no Session Manager"><span class="aws-dot online" />{online()} online</span>
        </div>
        <div class="aws-region-chips" aria-label="Regiões">
          <For each={regions()}>{([r, n]) => (
            <button class="aws-region-chip" title={`Mostrar só ${r}`} onClick={() => props.onRegion(r)}><span class="mono">{r}</span><span class="n">{n}</span></button>
          )}</For>
        </div>
        <div class="aws-client-tools">
          <button class="icon-btn" aria-label={`Editar o cliente ${props.client.name}`} title="Nome, perfil de acesso e regiões" onClick={props.onEdit}><Icon name="edit" size={15} /></button>
          <button class="icon-btn" aria-label={`Mapear ${props.client.name} de novo`} title="Mapear este cliente de novo" disabled={props.busy} onClick={props.onRemap}><Icon name="sync" size={15} /></button>
          <button class="icon-btn aws-chevron" classList={{ open: props.open }} aria-label={props.open ? "Recolher" : "Expandir"} onClick={props.onToggle}><Icon name="chevron-down" size={15} /></button>
        </div>
      </div>
      <Show when={props.open}>
        <InstanceTable {...props} />
      </Show>
    </section>
  );
}

const stateText: Record<string, string> = { running: "Em execução", stopped: "Parada", stopping: "Parando", pending: "Iniciando", "shutting-down": "Encerrando" };
const ssmLabel = { online: "Online", offline: "Offline", unmanaged: "Sem SSM" } as const;

function InstanceTable(props: RowActions & { rows: Row[]; overview: AwsOverview; showClient?: boolean }) {
  const [limit, setLimit] = createSignal(PAGE);
  return (
    <div class="aws-table" role="table" aria-label="Instâncias">
      <div class="aws-tr aws-th" role="row">
        <span role="columnheader">Instância</span>
        <span role="columnheader">{props.showClient ? "Cliente" : "Região"}</span>
        <span role="columnheader" class="aws-col-ip">IP privado</span>
        <span role="columnheader">Estado</span>
        <span role="columnheader">SSM</span>
        <span role="columnheader" class="aws-col-actions"><span class="sr-only">Ações</span></span>
      </div>
      <For each={props.rows.slice(0, limit())}>{(r) => <InstanceRow row={r} {...props} />}</For>
      <Show when={props.rows.length > limit()}>
        <div class="aws-more"><Button small onClick={() => setLimit(limit() + PAGE)}>Mostrar mais {Math.min(PAGE, props.rows.length - limit())} de {props.rows.length - limit()}</Button></div>
      </Show>
    </div>
  );
}

function InstanceRow(props: RowActions & { row: Row; overview: AwsOverview; showClient?: boolean }) {
  const i = () => props.row.instance;
  const why = () => unavailable(i(), props.overview);
  const fav = () => props.isFavorite(props.row);
  return (
    <div class="aws-tr" role="row" classList={{ dim: i().ssm === "unmanaged" || (!!i().state && i().state !== "running") }}>
      <div class="aws-cell-name" role="cell">
        <span class="name">{i().name || <span class="aws-muted">sem nome</span>}</span>
        <button class="aws-id mono" title="Copiar o ID" aria-label={`Copiar o ID ${i().id}`} onClick={() => void copy(i().id, "ID copiado")}>{i().id}<Icon name="copy" size={12} /></button>
      </div>
      <span role="cell" class="aws-cell-where">
        <Show when={props.showClient} fallback={<span class="mono">{props.row.region}</span>}>
          <span>{props.row.client}</span><span class="aws-muted mono">{props.row.region}</span>
        </Show>
      </span>
      <span role="cell" class="mono aws-col-ip">{i().private_ip || <span class="aws-muted">sem IP</span>}</span>
      <span role="cell" class="aws-state"><span class="aws-dot" classList={{ running: i().state === "running" }} />{i().state ? stateText[i().state] ?? i().state : <span class="aws-muted">Híbrida</span>}</span>
      <span role="cell" class="aws-state" title={i().platform || undefined}><span class={`aws-dot ${i().ssm}`} />{ssmLabel[i().ssm]}</span>
      <div class="aws-col-actions" role="cell">
        <span class="aws-actions" title={why() ?? undefined}>
          <Button small icon="terminal" disabled={!!why()} onClick={() => props.onShell(props.row)}>Terminal</Button>
          <button class="icon-btn" disabled={!!why()} aria-label={`SSH em ${i().name || i().id}`} title="SSH via Session Manager" onClick={() => props.onSsh(props.row)}><Icon name="key" size={15} /></button>
          <button class="icon-btn" disabled={!!why()} aria-label={`Port forward para ${i().name || i().id}`} title="Port forward via Session Manager" onClick={() => props.onForward(props.row)}><Icon name="route" size={15} /></button>
        </span>
        <button class="icon-btn aws-star" classList={{ on: fav() }} aria-pressed={fav()}
          aria-label={fav() ? `Tirar ${i().name || i().id} das favoritas` : `Fixar ${i().name || i().id} nas favoritas`}
          title={fav() ? "Tirar das favoritas" : "Fixar nas favoritas"} onClick={() => props.onFavorite(props.row)}>
          <Icon name="star" size={15} />
        </button>
      </div>
    </div>
  );
}

function ClientDialog(props: { client: Client; limited: number; onRegions: () => void; onSave: (name: string, profile: string | null) => void; onClose: () => void }) {
  const [name, setName] = createSignal(props.client.name);
  const [profile, setProfile] = createSignal(props.client.access);
  const save = () => {
    const n = name().trim() || props.client.access;
    if (n.length > 60) return invalid("Nome do cliente", "Nome do cliente: use até 60 caracteres.");
    props.onSave(n, props.client.profiles.length > 1 ? profile() : null);
  };
  return (
    <Modal title="Editar cliente" onClose={props.onClose}
      footer={<><Button onClick={props.onClose}>Cancelar</Button><Button variant="primary" onClick={save}>Salvar</Button></>}>
      <div class="form">
        <Field label="Nome do cliente" hint="Só neste computador. Vazio volta para o nome do perfil.">
          <input value={name()} maxLength={60} onInput={(e) => setName(e.currentTarget.value)} onKeyDown={(e) => e.key === "Enter" && save()} autofocus placeholder={props.client.access} />
        </Field>
        <Show when={props.client.profiles.length > 1}>
          <Field label="Acessar com o perfil" hint="Usado para abrir terminal, SSH e port forward nas instâncias deste cliente.">
            <select value={profile()} onChange={(e) => setProfile(e.currentTarget.value)}>
              <For each={props.client.profiles}>{(p) => <option value={p}>{p}</option>}</For>
            </select>
          </Field>
        </Show>
        <div class="field">
          <span class="field-label">Regiões mapeadas</span>
          <div class="aws-tool-row">
            <span>{props.limited ? (props.limited === 1 ? "1 região escolhida" : `${props.limited} regiões escolhidas`) : "Todas as regiões habilitadas da conta"}</span>
            <Button small icon="settings" onClick={props.onRegions}>Escolher</Button>
          </div>
        </div>
        <Show when={props.client.account}><p class="field-hint">Conta <span class="mono">{props.client.account}</span>{props.client.profiles.length ? `, perfis ${props.client.profiles.join(", ")}` : ""}.</p></Show>
      </div>
    </Modal>
  );
}

function SshDialog(props: { target: Target; onClose: () => void }) {
  const [username, setUsername] = createSignal("ec2-user");
  const [identity, setIdentity] = createSignal<string | null>(null);
  const [key, setKey] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const forget = async () => {
    try {
      const removed = await api.awsForgetHostKey(props.target.instance.id);
      toast(removed ? "Chave do servidor esquecida. A próxima conexão pede confirmação." : "Não havia chave salva para esta instância.", "info");
    } catch (e) { toast(errorMessage(e), "error"); }
  };
  const connect = async () => {
    if (!identity() && !username().trim()) return invalid("Usuário", `Usuário: ${BLANK.toLowerCase()}`);
    if (!identity() && !key()) return invalid("Chave SSH", "Escolha uma chave SSH do cofre, ou uma identidade.");
    setBusy(true);
    await withHostKeyCheck(props.target.instance.id, async (trustNew) => {
      const s = await api.awsSsh({
        profile: props.target.profile, region: props.target.region, instanceId: props.target.instance.id,
        username: username(), identityId: identity(), sshKeyId: key(),
      }, COLS, ROWS, trustNew);
      await refreshSessions();
      setActiveTab(s.id);
      props.onClose();
    }, { local: true });
    setBusy(false);
  };
  return (
    <Modal title={`SSH via SSM: ${props.target.instance.name || props.target.instance.id}`} onClose={props.onClose}
      footer={<><Button onClick={props.onClose}>Cancelar</Button><Button variant="primary" icon="key" busy={busy()} onClick={() => void connect()}>Conectar</Button></>}>
      <div class="form">
        <p class="confirm-text">A conexão passa pelo Session Manager (porta 22 da instância), sem abrir porta na internet. A chave do servidor é confirmada na primeira vez e fica salva só neste computador.</p>
        <Field label="Identidade" hint="Usuário e chave salvos no cofre. Sem identidade, use os campos abaixo.">
          <ItemSelect kind="identity" value={identity()} onChange={setIdentity} none="Nenhuma" />
        </Field>
        <div class="form-row">
          <Field label="Usuário"><input value={username()} onInput={(e) => setUsername(e.currentTarget.value)} placeholder="ec2-user, ubuntu, admin…" /></Field>
          <Field label="Chave SSH"><ItemSelect kind="ssh_key" value={key()} onChange={setKey} none={identity() ? "Da identidade" : "Escolha…"} /></Field>
        </div>
        <p class="field-hint">A instância foi reinstalada e a chave mudou? <button class="link-like" onClick={() => void forget()}>Esquecer a chave salva</button></p>
      </div>
    </Modal>
  );
}

function ForwardDialog(props: { target: Target; onClose: () => void }) {
  const [remotePort, setRemotePort] = createSignal("");
  const [localPort, setLocalPort] = createSignal("");
  const [remoteHost, setRemoteHost] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const start = async () => {
    if (!isPort(remotePort())) return invalid("Porta na instância", PORT_MSG);
    const local = localPort().trim() ? localPort() : remotePort();
    if (!isPort(local)) return invalid("Porta local", PORT_MSG);
    const host = remoteHost().trim();
    if (host && !/^[A-Za-z0-9.\-_:]+$/.test(host)) return invalid("Host de destino", "Host de destino inválido: use um nome ou IP, sem espaços.");
    setBusy(true);
    try {
      const s = await api.awsPortForward({
        profile: props.target.profile, region: props.target.region, instanceId: props.target.instance.id,
        remotePort: intIn(remotePort(), 1, 65535)!, localPort: intIn(local, 1, 65535)!, remoteHost: host || null,
      }, COLS, ROWS);
      await refreshSessions();
      setActiveTab(s.id);
      toast(`Porta ${intIn(local, 1, 65535)} deste computador ligada. Feche a aba para encerrar.`, "success");
      props.onClose();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
    setBusy(false);
  };
  return (
    <Modal title={`Port forward via SSM: ${props.target.instance.name || props.target.instance.id}`} onClose={props.onClose}
      footer={<><Button onClick={props.onClose}>Cancelar</Button><Button variant="primary" icon="route" busy={busy()} onClick={() => void start()}>Ligar</Button></>}>
      <div class="form">
        <div class="form-row">
          <Field label="Porta na instância"><input type="number" min="1" max="65535" value={remotePort()} onInput={(e) => setRemotePort(e.currentTarget.value)} placeholder="5432" autofocus /></Field>
          <Field label="Porta local" hint="Vazio usa a mesma."><input type="number" min="1" max="65535" value={localPort()} onInput={(e) => setLocalPort(e.currentTarget.value)} placeholder="igual" /></Field>
        </div>
        <Field label="Host de destino" hint="Opcional: um host que a instância alcança (ex.: o endereço de um banco RDS). Vazio liga a porta da própria instância.">
          <input value={remoteHost()} onInput={(e) => setRemoteHost(e.currentTarget.value)} placeholder="meu-banco.xxxx.rds.amazonaws.com" />
        </Field>
        <p class="confirm-text">A porta fica aberta só neste computador (localhost) enquanto a aba estiver aberta.</p>
      </div>
    </Modal>
  );
}

const COMMON_REGIONS = [
  "us-east-1", "us-east-2", "us-west-1", "us-west-2", "sa-east-1", "ca-central-1", "eu-west-1", "eu-west-2", "eu-west-3",
  "eu-central-1", "eu-north-1", "ap-south-1", "ap-southeast-1", "ap-southeast-2", "ap-northeast-1", "ap-northeast-2", "ap-northeast-3",
];

function RegionsDialog(props: { profile: string; known: string[]; chosen: string[]; onSave: (r: string[]) => void; onClose: () => void }) {
  const options = [...new Set([...props.known, ...props.chosen, ...COMMON_REGIONS])].sort();
  const [picked, setPicked] = createSignal<string[]>(props.chosen);
  const toggle = (r: string, on: boolean) => setPicked(on ? [...picked(), r] : picked().filter((x) => x !== r));
  return (
    <Modal title={`Regiões de ${props.profile}`} onClose={props.onClose}
      footer={<><Button onClick={() => setPicked([])}>Todas as habilitadas</Button><span style={{ flex: 1 }} /><Button onClick={props.onClose}>Cancelar</Button>
        <Button variant="primary" onClick={() => props.onSave(picked())}>Salvar</Button></>}>
      <p class="confirm-text">Sem nenhuma marcada, o mapeamento consulta todas as regiões habilitadas na conta. Marque só as que você usa para mapear mais rápido.</p>
      <div class="aws-region-picks" role="group" aria-label="Regiões">
        <For each={options}>{(r) => (
          <label class="check"><input type="checkbox" checked={picked().includes(r)} onChange={(e) => toggle(r, e.currentTarget.checked)} /><span class="mono">{r}</span></label>
        )}</For>
      </div>
    </Modal>
  );
}
