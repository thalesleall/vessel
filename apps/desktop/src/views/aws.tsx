import { For, Show, createEffect, createMemo, createResource, createSignal, on, onCleanup } from "solid-js";
import {
  AWS_ALL, api, errorMessage, on as onEvent, type AwsError, type AwsFavorite, type AwsInstance, type AwsInventory, type AwsOverview,
  type AwsProfile, type AwsProfileNode, type AwsProgress, type AwsRegionNode, type AwsSettings,
} from "../api";
import { copy, refreshSessions, setActiveTab, toast } from "../store";
import { withHostKeyCheck } from "../terminal";
import { Badge, Button, Empty, Field, Icon, Modal, confirmAction } from "../ui";
import { ItemSelect } from "./common";
import { BLANK, PORT_MSG, intIn, invalid, isPort } from "../validate";

const COLS = 120;
const ROWS = 32;
/** Rows drawn per region before "Mostrar mais": a large account stays responsive. */
const PAGE = 200;

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
  onEvent<AwsProfileNode>("aws://profile", (node) => setLive((inv) => upsertProfile(inv, node)));
  onEvent<{ profile: string; node: AwsRegionNode }>("aws://region", ({ profile, node }) => setLive((inv) => upsertRegion(inv, profile, node)));
  onEvent<{ partial: boolean; count: number }>("aws://done", (d) => {
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

const ssmText = { online: "online", offline: "offline", unmanaged: "não gerenciada" } as const;
const ssmTone = { online: "green", offline: "amber", unmanaged: "gray" } as const;

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

function ErrorLine(props: { error: AwsError; onRetry?: () => void }) {
  return (
    <div class="aws-error" role="alert">
      <Icon name="alert" size={14} />
      <span>{props.error.message}<Show when={errorHint(props.error)}>{(h) => <span class="aws-hint"> {h()}</span>}</Show></span>
      <Show when={props.onRetry && props.error.kind !== "cancelled"}>
        <Button small icon="sync" onClick={() => props.onRetry!()}>Tentar de novo</Button>
      </Show>
    </div>
  );
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
  const [profileFilter, setProfileFilter] = createSignal("");
  const [regionFilter, setRegionFilter] = createSignal("");
  const [ssh, setSsh] = createSignal<Target | null>(null);
  const [forward, setForward] = createSignal<Target | null>(null);
  const [regionsOf, setRegionsOf] = createSignal<string | null>(null);

  const settings = () => overview()?.settings ?? { master: null, regions: {}, favorites: [] };
  const master = () => overview()?.master ?? null;
  const profiles = () => overview()?.profiles ?? [];
  const masters = () => profiles().filter((p) => p.master === p.name);
  const inv = () => live() ?? inventory() ?? null;
  const isMapping = () => mapping() || !!overview()?.mapping;

  const saveSettings = async (next: AwsSettings) => {
    try {
      const saved = await api.awsSaveSettings(next);
      const o = overview();
      if (o) mutateOverview({ ...o, settings: saved, master: saved.master ?? o.master });
    } catch (e) {
      toast(errorMessage(e), "error");
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
    const count = m === AWS_ALL ? profiles().length : profiles().filter((p) => p.master === m).length;
    if (!(await confirmAction({
      title: "Mapear todas as contas?",
      message: `Vai consultar todas as regiões habilitadas de ${count} perfil(is), uma conta por vez em cada região. Pode levar alguns minutos, e você pode cancelar a qualquer momento. As credenciais temporárias ficam só na memória durante o mapeamento.`,
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

  const toggleFavorite = (f: AwsFavorite) => {
    const s = settings();
    const has = s.favorites.some((x) => x.id === f.id && x.profile === f.profile && x.region === f.region);
    void saveSettings({ ...s, favorites: has ? s.favorites.filter((x) => !(x.id === f.id && x.profile === f.profile && x.region === f.region)) : [...s.favorites, f] });
  };
  const isFavorite = (profile: string, region: string, id: string) =>
    settings().favorites.some((x) => x.id === id && x.profile === profile && x.region === region);

  const matches = (i: AwsInstance) => {
    const q = search().trim().toLowerCase();
    if (q && !i.name.toLowerCase().includes(q) && !i.id.toLowerCase().includes(q) && !i.private_ip.includes(q)) return false;
    if (onlineOnly() && i.ssm !== "online") return false;
    return true;
  };
  const shownProfiles = () => (inv()?.profiles ?? []).filter((p) => !profileFilter() || p.profile === profileFilter());
  const allRegions = createMemo(() => [...new Set((inv()?.profiles ?? []).flatMap((p) => p.regions.map((r) => r.region)))].sort());
  const favorites = createMemo(() => {
    const out: Target[] = [];
    for (const f of settings().favorites) {
      const node = inv()?.profiles.find((p) => p.profile === f.profile)?.regions.find((r) => r.region === f.region);
      const instance = node?.instances.find((i) => i.id === f.id);
      if (instance) out.push({ profile: f.profile, region: f.region, instance });
    }
    return out;
  });

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

  return (
    <>
      <div class="page-head">
        <h1>AWS</h1>
        <div class="spacer" />
        <Show when={overview()?.mapped_at}>
          {(t) => <span class="aws-muted" title={new Date(t() * 1000).toLocaleString("pt-BR")}>Mapeado {ago(t())}{overview()?.partial ? " (incompleto)" : ""}</span>}
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
                      <For each={masters()}>{(p) => (
                        <option value={p.name}>{p.name}{p.dependents.length ? ` (+${profiles().filter((x) => x.master === p.name && x.name !== p.name).length} perfis)` : ""}</option>
                      )}</For>
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
                      ? `conta ${progress()!.accounts_done}/${progress()!.accounts_total} · região ${progress()!.regions_done}/${progress()!.regions_total} · ${elapsed(progress()!.elapsed_ms)}`
                      : "Pegando as credenciais dos perfis…"}</span>
                  </div>
                </Show>
                <label class="check aws-agents"><input type="checkbox" checked={o().agents_allowed} onChange={(e) => void setAgents(e.currentTarget.checked, e.currentTarget)} />
                  Agentes de IA podem listar estas instâncias e abrir sessões nelas (MCP)</label>
              </div>

              <Show when={favorites().length}>
                <div class="section-title"><Icon name="check" size={14} />Favoritas</div>
                <div class="list">
                  <For each={favorites()}>{(t) => (
                    <InstanceRow target={t} overview={o()} favorite onFavorite={() => toggleFavorite({ profile: t.profile, region: t.region, id: t.instance.id })}
                      onShell={() => void openShell(t)} onSsh={() => setSsh(t)} onForward={() => setForward(t)} showWhere />
                  )}</For>
                </div>
              </Show>

              <Show when={inv()?.profiles.length} fallback={
                <Show when={!isMapping()}>
                  <Empty icon="cloud" title="Nada mapeado ainda">
                    <span>“Mapear tudo” consulta as instâncias de todas as contas e regiões do perfil mestre. O resultado fica guardado, cifrado, só neste computador.</span>
                  </Empty>
                </Show>
              }>
                <div class="aws-filters">
                  <div class="search aws-search"><Icon name="search" size={16} />
                    <input type="search" placeholder="Buscar por nome, ID ou IP…" aria-label="Buscar instâncias" value={search()} onInput={(e) => setSearch(e.currentTarget.value)} />
                  </div>
                  <select aria-label="Filtrar por perfil" value={profileFilter()} onChange={(e) => setProfileFilter(e.currentTarget.value)}>
                    <option value="">Todos os perfis</option>
                    <For each={inv()?.profiles ?? []}>{(p) => <option value={p.profile}>{p.profile}</option>}</For>
                  </select>
                  <select aria-label="Filtrar por região" value={regionFilter()} onChange={(e) => setRegionFilter(e.currentTarget.value)}>
                    <option value="">Todas as regiões</option>
                    <For each={allRegions()}>{(r) => <option value={r}>{r}</option>}</For>
                  </select>
                  <label class="check"><input type="checkbox" checked={onlineOnly()} onChange={(e) => setOnlineOnly(e.currentTarget.checked)} />Só online no SSM</label>
                </div>

                <For each={shownProfiles()}>{(p) => (
                  <ProfileTree node={p} profile={profiles().find((x) => x.name === p.profile)} overview={o()} regionFilter={regionFilter()}
                    matches={matches} filtering={!!search().trim() || onlineOnly()} busy={isMapping()}
                    isFavorite={isFavorite} onFavorite={toggleFavorite}
                    onRetry={(region) => void start({ profile: p.profile, region })}
                    onRegions={() => setRegionsOf(p.profile)}
                    onShell={(t) => void openShell(t)} onSsh={setSsh} onForward={setForward} />
                )}</For>
              </Show>
            </Show>

            <Show when={ssh()} keyed>{(t) => <SshDialog target={t} onClose={() => setSsh(null)} />}</Show>
            <Show when={forward()} keyed>{(t) => <ForwardDialog target={t} onClose={() => setForward(null)} />}</Show>
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

interface Target { profile: string; region: string; instance: AwsInstance }

function ProfileTree(props: {
  node: AwsProfileNode;
  profile: AwsProfile | undefined;
  overview: AwsOverview;
  regionFilter: string;
  matches: (i: AwsInstance) => boolean;
  filtering: boolean;
  busy: boolean;
  isFavorite: (profile: string, region: string, id: string) => boolean;
  onFavorite: (f: AwsFavorite) => void;
  onRetry: (region?: string) => void;
  onRegions: () => void;
  onShell: (t: Target) => void;
  onSsh: (t: Target) => void;
  onForward: (t: Target) => void;
}) {
  const [open, setOpen] = createSignal(true);
  const regions = () => props.node.regions.filter((r) => !props.regionFilter || r.region === props.regionFilter);
  const count = () => props.node.regions.reduce((n, r) => n + r.instances.length, 0);
  const role = () => props.node.role_arn?.split("/").pop();
  const via = () => props.profile && props.profile.master !== props.profile.name ? props.profile.source_profile : null;
  const limited = () => props.overview.settings.regions[props.node.profile]?.length;
  return (
    <section class="aws-profile">
      <div class="aws-profile-head">
        <button class="aws-toggle" aria-expanded={open()} onClick={() => setOpen(!open())}>
          <Icon name={open() ? "arrow-down" : "chevron-left"} size={14} />
          <span class="name">{props.node.profile}</span>
        </button>
        <span class="aws-muted">
          {[props.node.account ? `conta ${props.node.account}` : null, role() ? `papel ${role()}` : null, via() ? `via ${via()}` : null].filter(Boolean).join(" · ")}
        </span>
        <Show when={props.profile?.problem}>{(p) => <Badge tone="red">{p()}</Badge>}</Show>
        <div class="spacer" />
        <span class="aws-muted">{count()} instâncias</span>
        <Button small icon="settings" onClick={props.onRegions} title="Escolher as regiões deste perfil">{limited() ? (limited() === 1 ? "1 região" : `${limited()} regiões`) : "Regiões"}</Button>
        <Button small icon="sync" onClick={() => props.onRetry()} disabled={props.busy} title="Mapear só este perfil de novo">Mapear</Button>
      </div>
      <Show when={props.node.error}>{(e) => <ErrorLine error={e()} onRetry={props.busy ? undefined : () => props.onRetry()} />}</Show>
      <Show when={open()}>
        <For each={regions()}>{(r) => (
          <RegionTree profile={props.node.profile} node={r} overview={props.overview} matches={props.matches} filtering={props.filtering} busy={props.busy}
            isFavorite={props.isFavorite} onFavorite={props.onFavorite} onRetry={() => props.onRetry(r.region)}
            onShell={props.onShell} onSsh={props.onSsh} onForward={props.onForward} />
        )}</For>
      </Show>
    </section>
  );
}

function RegionTree(props: {
  profile: string;
  node: AwsRegionNode;
  overview: AwsOverview;
  matches: (i: AwsInstance) => boolean;
  filtering: boolean;
  busy: boolean;
  isFavorite: (profile: string, region: string, id: string) => boolean;
  onFavorite: (f: AwsFavorite) => void;
  onRetry: () => void;
  onShell: (t: Target) => void;
  onSsh: (t: Target) => void;
  onForward: (t: Target) => void;
}) {
  const shown = createMemo(() => props.node.instances.filter(props.matches));
  // Large regions start closed; a search opens every region with matches.
  const [open, setOpen] = createSignal(props.node.instances.length <= 50);
  const [limit, setLimit] = createSignal(PAGE);
  const isOpen = () => open() || (props.filtering && shown().length > 0);
  return (
    <Show when={!props.filtering || shown().length || props.node.error}>
      <div class="aws-region">
        <div class="aws-region-head">
          <button class="aws-toggle" aria-expanded={isOpen()} onClick={() => setOpen(!isOpen())}>
            <Icon name={isOpen() ? "arrow-down" : "chevron-left"} size={13} />
            <span class="mono">{props.node.region}</span>
          </button>
          <span class="aws-muted">{props.filtering ? `${shown().length} de ${props.node.instances.length}` : props.node.instances.length}</span>
        </div>
        <Show when={props.node.error}>{(e) => <ErrorLine error={e()} onRetry={props.busy ? undefined : props.onRetry} />}</Show>
        <Show when={isOpen() && shown().length}>
          <div class="list aws-list">
            <For each={shown().slice(0, limit())}>{(i) => {
              const t = { profile: props.profile, region: props.node.region, instance: i };
              return (
                <InstanceRow target={t} overview={props.overview} favorite={props.isFavorite(props.profile, props.node.region, i.id)}
                  onFavorite={() => props.onFavorite({ profile: props.profile, region: props.node.region, id: i.id })}
                  onShell={() => props.onShell(t)} onSsh={() => props.onSsh(t)} onForward={() => props.onForward(t)} />
              );
            }}</For>
            <Show when={shown().length > limit()}>
              <div class="row"><Button small onClick={() => setLimit(limit() + PAGE)}>Mostrar mais {Math.min(PAGE, shown().length - limit())} de {shown().length - limit()}</Button></div>
            </Show>
          </div>
        </Show>
      </div>
    </Show>
  );
}

function InstanceRow(props: {
  target: Target;
  overview: AwsOverview;
  favorite: boolean;
  showWhere?: boolean;
  onFavorite: () => void;
  onShell: () => void;
  onSsh: () => void;
  onForward: () => void;
}) {
  const i = () => props.target.instance;
  const why = () => unavailable(i(), props.overview);
  const forget = async () => {
    try {
      const removed = await api.awsForgetHostKey(i().id);
      toast(removed ? "Chave do host esquecida. A próxima conexão SSH pede confirmação." : "Não havia chave salva para esta instância.", "info");
    } catch (e) { toast(errorMessage(e), "error"); }
  };
  return (
    <div class="row aws-row">
      <button class="icon-btn aws-star" classList={{ on: props.favorite }} aria-pressed={props.favorite}
        aria-label={props.favorite ? `Tirar ${i().name || i().id} das favoritas` : `Fixar ${i().name || i().id} no topo`}
        title={props.favorite ? "Tirar das favoritas" : "Fixar no topo"} onClick={props.onFavorite}>
        <Icon name={props.favorite ? "check" : "plus"} size={14} />
      </button>
      <div class="meta">
        <div class="name">{i().name || <span class="aws-muted">(sem nome)</span>}</div>
        <div class="sub"><span class="mono">{i().id}</span>{i().private_ip ? ` · ${i().private_ip}` : ""}{i().platform ? ` · ${i().platform}` : ""}
          <Show when={props.showWhere}>{` · ${props.target.profile} · ${props.target.region}`}</Show></div>
      </div>
      <Show when={i().state}><Badge tone={i().state === "running" ? "blue" : "gray"}>{i().state}</Badge></Show>
      <Badge tone={ssmTone[i().ssm]}>SSM {ssmText[i().ssm]}</Badge>
      <div class="aws-actions" title={why() ?? undefined}>
        <Button small icon="terminal" disabled={!!why()} onClick={props.onShell}>Terminal</Button>
        <Button small icon="key" disabled={!!why()} onClick={props.onSsh}>SSH</Button>
        <Button small icon="route" disabled={!!why()} onClick={props.onForward}>Porta</Button>
      </div>
      <button class="icon-btn" aria-label={`Copiar o ID ${i().id}`} title="Copiar ID" onClick={() => void copy(i().id, "ID copiado")}><Icon name="copy" size={14} /></button>
      <button class="icon-btn" aria-label={`Esquecer a chave SSH salva de ${i().id}`} title="Esquecer chave do host" onClick={() => void forget()}><Icon name="unlink" size={14} /></button>
    </div>
  );
}

function SshDialog(props: { target: Target; onClose: () => void }) {
  const [username, setUsername] = createSignal("ec2-user");
  const [identity, setIdentity] = createSignal<string | null>(null);
  const [key, setKey] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
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
