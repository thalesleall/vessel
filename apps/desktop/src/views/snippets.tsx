import { For, Show, createEffect, createMemo, createResource, createSignal, on, onCleanup } from "solid-js";
import { api, blank, shellLabel, type RunRecord, type Schedule, type Shell, type Snippet, type SnippetTarget } from "../api";
import { attempt, itemsOf, lastRuns, personalVaultId, scheduler, sessions, tryAction, vaultName, vaults } from "../store";
import { pasteSnippet, runSnippet, snippetData, targetText, type SnippetItem } from "../snippetrun";
import { Badge, Button, Empty, Field, Icon, Modal, ShellIcon, confirmAction, timeAgo } from "../ui";
import { EditorFooter, VaultSelect, defaultVault, removeItem, saveEntity, splitTags } from "./common";

function when(ts: number): string {
  const d = new Date(ts * 1000);
  const today = new Date();
  const tomorrow = new Date(Date.now() + 86_400_000);
  const time = d.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === today.toDateString()) return `hoje, ${time}`;
  if (d.toDateString() === tomorrow.toDateString()) return `amanhã, ${time}`;
  return `${d.toLocaleDateString("pt-BR", { weekday: "short", day: "2-digit", month: "2-digit" })}, ${time}`;
}

const statusText: Record<RunRecord["status"], string> = {
  running: "Rodando", ok: "Sem erros", failed: "Falhou", skipped: "Pulado", missed: "Perdido",
};
const statusTone: Record<RunRecord["status"], "blue" | "green" | "red" | "amber" | "gray"> = {
  running: "blue", ok: "green", failed: "red", skipped: "amber", missed: "gray",
};
const originText: Record<RunRecord["origin"], string> = { manual: "Manual", scheduled: "Agendado", agent: "Agente de IA" };

export function SnippetsView() {
  const snippets = itemsOf("snippet");
  const [editing, setEditing] = createSignal<SnippetItem | "new" | null>(null);
  const [pasting, setPasting] = createSignal<SnippetItem | null>(null);
  const [history, setHistory] = createSignal<SnippetItem | "all" | null>(null);
  const hasSchedules = () => (scheduler()?.schedules.length ?? 0) > 0;
  const nextFor = (id: string) => {
    const times = (scheduler()?.schedules ?? []).filter((s) => s.snippet_id === id && s.next).map((s) => s.next!);
    return times.length ? Math.min(...times) : null;
  };
  const running = (id: string) => scheduler()?.running.includes(id) || lastRuns()[id]?.status === "running";

  return (
    <>
      <div class="page-head">
        <h1>Snippets</h1>
        <div class="spacer" />
        <Show when={hasSchedules()}>
          <Button icon={scheduler()?.paused ? "play" : "pause"} onClick={() => attempt(() => api.setSchedulesPaused(!scheduler()?.paused))}
            title="Vale para todos os agendamentos deste computador">
            {scheduler()?.paused ? "Retomar agendamentos" : "Pausar agendamentos"}
          </Button>
        </Show>
        <Button icon="history" onClick={() => setHistory("all")}>Histórico</Button>
        <Button variant="primary" icon="plus" onClick={() => setEditing("new")}>Novo snippet</Button>
      </div>
      <Show when={scheduler()?.paused && hasSchedules()}>
        <div class="notice" role="status"><Icon name="pause" size={15} />Os agendamentos estão pausados. Nada roda sozinho até você retomar.</div>
      </Show>
      <Show when={snippets().length} fallback={
        <Empty icon="code" title="Nenhum snippet">
          <span>Guarde comandos que você repete. Rode com um clique neste computador ou em hosts SSH, agende, ou cole num terminal aberto.</span>
        </Empty>
      }>
        <div class="grid">
          <For each={snippets()}>
            {(s) => {
              const d = () => snippetData(s);
              const last = () => lastRuns()[s.id];
              const runnable = () => d().target.kind !== "none";
              return (
                <div class="card snippet-card">
                  <button class="card-hit" onClick={() => setHistory(s)} aria-label={`Histórico de ${d().label}`} title="Ver histórico" />
                  <div class="avatar snippet-icon">
                    <Show when={running(s.id)} fallback={
                      d().target.kind === "local" ? <ShellIcon shell={(d().target as { shell: Shell }).shell} size={22} />
                        : <Icon name={d().target.kind === "hosts" ? "server" : "code"} size={18} />
                    }><span class="spinner" /></Show>
                  </div>
                  <div class="meta">
                    <div class="name">{d().label}</div>
                    <div class="sub">{targetText(d())}<Show when={vaults().length > 1 && s.vault_id !== personalVaultId()}> · {vaultName(s.vault_id)}</Show></div>
                    <div class="sub snippet-state">
                      <Show when={last()} fallback={<Show when={!runnable()}><span class="mono">{d().script.split("\n")[0]}</span></Show>}>
                        {(r) => <><StatusDot status={r().status} />{statusText[r().status]} · {timeAgo(r().finished_at ?? r().started_at)}</>}
                      </Show>
                      <Show when={nextFor(s.id)}>{(n) => <span class="next" title="Próximo agendamento"><Icon name="clock" size={12} />{when(n())}</span>}</Show>
                    </div>
                  </div>
                  <div class="actions">
                    <button class="icon-btn" aria-label={`Editar ${d().label}`} title="Editar" onClick={() => setEditing(s)}><Icon name="edit" size={15} /></button>
                  </div>
                  <Show when={runnable()} fallback={
                    <button class="run-btn" onClick={() => setPasting(s)} disabled={!sessions().some((x) => x.alive)}
                      title={sessions().some((x) => x.alive) ? "Colar num terminal aberto" : "Abra um terminal para colar este snippet, ou dê a ele um destino ao editá-lo"}
                      aria-label={`Colar ${d().label} num terminal`}><Icon name="terminal" size={15} /></button>
                  }>
                    <button class="run-btn primary" onClick={() => runSnippet(s)} disabled={running(s.id)}
                      title={running(s.id) ? "Rodando…" : `Rodar em ${targetText(d())}`} aria-label={`Rodar ${d().label}`}><Icon name="play" size={15} /></button>
                  </Show>
                </div>
              );
            }}
          </For>
        </div>
      </Show>
      <Show when={editing()} keyed>{(e) => <SnippetEditor item={e === "new" ? null : (e as SnippetItem)} onClose={() => setEditing(null)} />}</Show>
      <Show when={history()} keyed>{(h) => <History item={h === "all" ? null : (h as SnippetItem)} onClose={() => setHistory(null)} onEdit={(s) => { setHistory(null); setEditing(s); }} />}</Show>
      <Show when={pasting()} keyed>{(s) => <PastePicker item={s} onClose={() => setPasting(null)} />}</Show>
    </>
  );
}

function StatusDot(props: { status: RunRecord["status"] }) {
  return <span class={`run-dot run-${props.status}`} aria-hidden="true" />;
}

function PastePicker(props: { item: SnippetItem; onClose: () => void }) {
  const s = () => snippetData(props.item);
  return (
    <Modal title={`Colar "${s().label}"`} onClose={props.onClose}>
      <p class="confirm-text" style={{ "margin-bottom": "8px" }}>Este é o script inteiro que será digitado no terminal escolhido:</p>
      <div class="code-block" style={{ "margin-bottom": "14px", "max-height": "220px", overflow: "auto" }}>{s().script}</div>
      <div class="list">
        <For each={sessions().filter((x) => x.alive)}>
          {(t) => (
            <button class="row row-button" onClick={async () => { await pasteSnippet(s(), t.id); props.onClose(); }}>
              <Icon name={t.kind.kind === "ssh" ? "server" : "terminal"} />
              <span class="meta"><span class="name">{t.title}</span></span>
            </button>
          )}
        </For>
      </div>
    </Modal>
  );
}

function History(props: { item: SnippetItem | null; onClose: () => void; onEdit: (s: SnippetItem) => void }) {
  const [runs, { refetch }] = createResource(() => api.snippetRuns(props.item?.id));
  const [open, setOpen] = createSignal<string | null>(null);
  // New runs of this snippet show up while the window is open.
  createEffect(on(lastRuns, () => refetch(), { defer: true }));
  const s = () => (props.item ? snippetData(props.item) : undefined);
  const clear = async () => {
    if (!(await confirmAction(props.item ? `Apagar o histórico de "${s()!.label}" neste computador?` : "Apagar todo o histórico de snippets deste computador?"))) return;
    await tryAction(() => api.clearSnippetRuns(props.item?.id), "Histórico apagado");
    refetch();
  };
  return (
    <Modal title={props.item ? s()!.label : "Histórico de snippets"} onClose={props.onClose} wide
      footer={<>
        <Show when={runs()?.length}><Button variant="danger" icon="trash" onClick={clear}>Apagar histórico</Button></Show>
        <span style={{ flex: 1 }} />
        <Show when={props.item}><Button icon="edit" onClick={() => props.onEdit(props.item!)}>Editar</Button></Show>
        <Show when={props.item && s()!.target.kind !== "none"}><Button variant="primary" icon="play" onClick={() => runSnippet(props.item!)}>Rodar agora</Button></Show>
      </>}>
      <Show when={props.item}>
        <div class="code-block" style={{ "margin-bottom": "14px", "max-height": "140px", overflow: "auto" }}>{s()!.script}</div>
      </Show>
      <p class="field-hint" style={{ margin: "0 0 10px" }}>O histórico fica só neste computador, cifrado, e não é sincronizado.</p>
      <Show when={runs()?.length} fallback={<p class="confirm-text">{runs.loading ? "Carregando…" : "Nenhuma execução ainda."}</p>}>
        <div class="list">
          <For each={runs()}>
            {(r) => (
              <div class="run-row">
                <button class="row row-button" aria-expanded={open() === r.id} onClick={() => setOpen(open() === r.id ? null : r.id)}>
                  <StatusDot status={r.status} />
                  <span class="meta">
                    <span class="name">{props.item ? new Date(r.started_at * 1000).toLocaleString("pt-BR") : r.label}</span>
                    <span class="sub">
                      {props.item ? "" : `${new Date(r.started_at * 1000).toLocaleString("pt-BR")} · `}
                      {r.results.length ? r.target : r.note ?? r.target}{r.finished_at && r.results.length ? ` · ${Math.max(...r.results.map((t) => t.duration_ms)) / 1000} s` : ""}
                    </span>
                  </span>
                  <Badge tone={r.origin === "agent" ? "blue" : "gray"}>{originText[r.origin]}</Badge>
                  <Badge tone={statusTone[r.status]}>{statusText[r.status]}</Badge>
                </button>
                <Show when={open() === r.id}>
                  <div class="run-detail">
                    <Show when={r.note}><p class="confirm-text">{r.note}</p></Show>
                    <For each={r.results}>
                      {(t) => (
                        <div class="run-target">
                          <div class="run-target-head">
                            <strong>{t.target}</strong>
                            <span class="sub">{t.timed_out ? "tempo esgotado" : t.exit_code !== null ? `código ${t.exit_code}` : ""}{t.truncated ? " · saída cortada em 64 KB" : ""}</span>
                          </div>
                          <Show when={t.error}><p class="run-error">{t.error}</p></Show>
                          <Show when={t.stdout}><pre class="code-block run-output">{t.stdout}</pre></Show>
                          <Show when={t.stderr}><pre class="code-block run-output run-stderr" aria-label="Saída de erro">{t.stderr}</pre></Show>
                          <Show when={!t.stdout && !t.stderr && !t.error}><p class="field-hint">Sem saída.</p></Show>
                        </div>
                      )}
                    </For>
                  </div>
                </Show>
              </div>
            )}
          </For>
        </div>
      </Show>
    </Modal>
  );
}

const presets: { label: string; cron: string }[] = [
  { label: "A cada 15 minutos", cron: "*/15 * * * *" },
  { label: "A cada hora", cron: "0 * * * *" },
  { label: "Todo dia às 07:00", cron: "0 7 * * *" },
  { label: "Dias úteis às 07:00", cron: "0 7 * * 1-5" },
  { label: "Domingo às 03:00", cron: "0 3 * * 0" },
];

function ScheduleRow(props: { value: Schedule; onChange: (s: Schedule) => void; onRemove: () => void; disabled: boolean }) {
  const [preview, setPreview] = createSignal<{ times?: number[]; error?: string }>({});
  let timer: number | undefined;
  createEffect(on(() => props.value.cron, (cron) => {
    clearTimeout(timer);
    timer = window.setTimeout(async () => {
      if (!cron.trim()) return setPreview({});
      try { setPreview({ times: await api.cronPreview(cron) }); } catch (e) { setPreview({ error: (e as { message?: string }).message ?? String(e) }); }
    }, 250);
  }));
  onCleanup(() => clearTimeout(timer));
  return (
    <div class="schedule-row">
      <div class="schedule-inputs">
        <input class="mono" value={props.value.cron} disabled={props.disabled} aria-label="Expressão cron" placeholder="0 7 * * 1-5"
          onInput={(e) => props.onChange({ ...props.value, cron: e.currentTarget.value })} spellcheck={false} />
        <select aria-label="Modelos de horário" disabled={props.disabled} value="" onChange={(e) => { if (e.currentTarget.value) props.onChange({ ...props.value, cron: e.currentTarget.value }); }}>
          <option value="">Modelos…</option>
          <For each={presets}>{(p) => <option value={p.cron}>{p.label}</option>}</For>
        </select>
        <label class="check"><input type="checkbox" checked={props.value.enabled} disabled={props.disabled} onChange={(e) => props.onChange({ ...props.value, enabled: e.currentTarget.checked })} />Ativo</label>
        <button class="icon-btn" aria-label="Remover agendamento" title="Remover" onClick={props.onRemove} disabled={props.disabled}><Icon name="trash" size={15} /></button>
      </div>
      <div class="field-hint" classList={{ "schedule-error": !!preview().error }} aria-live="polite">
        {preview().error ?? (preview().times ? `Próximos: ${preview().times!.map(when).join(" · ")}` : "minuto hora dia mês dia-da-semana, no horário deste computador")}
      </div>
    </div>
  );
}

function SnippetEditor(props: { item: SnippetItem | null; onClose: () => void }) {
  const [d, setD] = createSignal<Snippet>(props.item ? snippetData(props.item) : blank.snippet());
  const [tags, setTags] = createSignal(d().tags.join(", "));
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const [hostFilter, setHostFilter] = createSignal("");
  const [shells] = createResource(() => api.detectShells());
  const hosts = itemsOf("host");
  const personal = () => vault() === personalVaultId();
  const set = <K extends keyof Snippet>(k: K, v: Snippet[K]) => setD({ ...d(), [k]: v });
  const setTarget = (t: SnippetTarget) => set("target", t);
  const localShells = createMemo(() => (shells() ?? []).filter((s) => s.shell.kind !== "custom"));
  const hostIds = () => (d().target.kind === "hosts" ? (d().target as { host_ids: string[] }).host_ids : []);
  const shellKey = (s: Shell) => JSON.stringify(s);
  const filteredHosts = () => {
    const q = hostFilter().toLowerCase();
    return hosts().filter((h) => !q || `${h.entity.data.label} ${h.entity.data.address}`.toLowerCase().includes(q));
  };
  const save = async () => {
    if (!d().script.trim()) return;
    if (d().target.kind === "hosts" && hostIds().length === 0) {
      await confirmAction({ title: "Escolha os hosts", message: "Marque pelo menos um host, ou escolha outro destino.", confirm: "Entendi", danger: false });
      return;
    }
    const data: Snippet = {
      ...d(),
      label: d().label.trim() || d().script.split("\n")[0].slice(0, 40),
      tags: splitTags(tags()),
      timeout_secs: Math.min(Math.max(Math.round(d().timeout_secs) || 60, 1), 21_600),
      schedules: d().schedules.filter((s) => s.cron.trim()),
      agent_allowed: d().agent_allowed && d().target.kind !== "none" && !d().confirm,
    };
    if (await saveEntity(props.item, vault(), { type: "snippet", data })) props.onClose();
  };
  return (
    <Modal title={props.item ? "Editar snippet" : "Novo snippet"} onClose={props.onClose} wide
      footer={<EditorFooter onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "o snippet"); props.onClose(); } : undefined} />}>
      <div class="form">
        <div class="form-row">
          <Field label="Nome"><input value={d().label} onInput={(e) => set("label", e.currentTarget.value)} autofocus placeholder="Ligar o PC do escritório" /></Field>
          <Field label="Cofre"><VaultSelect value={vault()} onChange={setVault} /></Field>
        </div>
        <Field label="Script"><textarea rows={7} class="mono" value={d().script} spellcheck={false} onInput={(e) => set("script", e.currentTarget.value)} placeholder="sudo systemctl restart nginx" /></Field>

        <fieldset class="fieldset">
          <legend class="field-label">Onde roda</legend>
          <div class="chips" role="radiogroup" aria-label="Destino">
            <button class="chip" role="radio" aria-checked={d().target.kind === "none"} classList={{ on: d().target.kind === "none" }} onClick={() => setTarget({ kind: "none" })}><Icon name="terminal" size={14} />Colar num terminal</button>
            <button class="chip" role="radio" aria-checked={d().target.kind === "local"} classList={{ on: d().target.kind === "local" }}
              onClick={() => d().target.kind !== "local" && setTarget({ kind: "local", shell: localShells()[0]?.shell ?? { kind: "pwsh" } })}><Icon name="monitor" size={14} />Este computador</button>
            <button class="chip" role="radio" aria-checked={d().target.kind === "hosts"} classList={{ on: d().target.kind === "hosts" }}
              onClick={() => d().target.kind !== "hosts" && setTarget({ kind: "hosts", host_ids: [] })}><Icon name="server" size={14} />Hosts SSH</button>
          </div>
          <Show when={d().target.kind === "none"}>
            <p class="field-hint">O snippet é digitado num terminal que você escolhe. Para rodar com um clique ou agendar, escolha um destino.</p>
          </Show>
          <Show when={d().target.kind === "local"}>
            <Field label="Shell" hint="Roda sem janela, na sua pasta de usuário. A saída fica no histórico.">
              <select value={shellKey((d().target as { shell: Shell }).shell)} onChange={(e) => setTarget({ kind: "local", shell: JSON.parse(e.currentTarget.value) })}>
                <For each={localShells()}>{(s) => <option value={shellKey(s.shell)}>{s.label}</option>}</For>
                <Show when={!localShells().some((s) => shellKey(s.shell) === shellKey((d().target as { shell: Shell }).shell))}>
                  <option value={shellKey((d().target as { shell: Shell }).shell)}>{shellLabel((d().target as { shell: Shell }).shell)} (não encontrado aqui)</option>
                </Show>
              </select>
            </Field>
          </Show>
          <Show when={d().target.kind === "hosts"}>
            <Field label={`Hosts (${hostIds().length} marcados)`} hint="Roda em todos ao mesmo tempo, sem terminal. Um host cuja chave ainda não foi confirmada é recusado: conecte-se a ele uma vez antes.">
              <Show when={hosts().length > 6}><input placeholder="Filtrar hosts…" value={hostFilter()} onInput={(e) => setHostFilter(e.currentTarget.value)} aria-label="Filtrar hosts" /></Show>
              <div class="host-picks">
                <For each={filteredHosts()} fallback={<p class="field-hint">Nenhum host. Cadastre hosts na tela Hosts.</p>}>
                  {(h) => (
                    <label class="check">
                      <input type="checkbox" checked={hostIds().includes(h.id)}
                        onChange={(e) => setTarget({ kind: "hosts", host_ids: e.currentTarget.checked ? [...hostIds(), h.id] : hostIds().filter((x) => x !== h.id) })} />
                      <span>{h.entity.data.label || h.entity.data.address}</span><span class="field-hint">{h.entity.data.address}</span>
                    </label>
                  )}
                </For>
              </div>
            </Field>
          </Show>
        </fieldset>

        <Show when={d().target.kind !== "none"}>
          <div class="form-row">
            <Field label="Tempo máximo (segundos)" hint="Passou disso, a execução é interrompida."><input type="number" min="1" max="21600" value={d().timeout_secs} onInput={(e) => set("timeout_secs", Number(e.currentTarget.value))} /></Field>
            <div class="field" style={{ "justify-content": "center" }}>
              <label class="check"><input type="checkbox" checked={d().confirm} onChange={(e) => setD({ ...d(), confirm: e.currentTarget.checked, agent_allowed: e.currentTarget.checked ? false : d().agent_allowed })} />Pedir confirmação antes de rodar</label>
            </div>
          </div>

          <fieldset class="fieldset">
            <legend class="field-label">Agendamentos</legend>
            <Show when={personal()} fallback={<p class="field-hint">Agendamentos só valem para snippets do seu cofre pessoal: num cofre compartilhado, outro membro poderia mudar o script que roda sozinho no seu computador.</p>}>
              <p class="field-hint">Rodam enquanto o Vessel está aberto (também minimizado na bandeja) e o cofre desbloqueado. Um horário que passar com o app fechado não roda depois.</p>
            </Show>
            <For each={d().schedules}>
              {(sch, i) => (
                <ScheduleRow value={sch} disabled={!personal()}
                  onChange={(v) => set("schedules", d().schedules.map((x, j) => (j === i() ? v : x)))}
                  onRemove={() => set("schedules", d().schedules.filter((_, j) => j !== i()))} />
              )}
            </For>
            <div><Button small icon="plus" disabled={!personal()} onClick={() => set("schedules", [...d().schedules, { cron: "0 7 * * 1-5", enabled: true }])}>Adicionar horário</Button></div>
          </fieldset>

          <label class="check">
            <input type="checkbox" checked={d().agent_allowed} disabled={!personal() || d().confirm} onChange={(e) => set("agent_allowed", e.currentTarget.checked)} />
            Agentes de IA podem rodar este snippet
          </label>
          <p class="field-hint" style={{ "margin-top": "-8px" }}>
            {!personal() ? "Só para snippets do seu cofre pessoal." : d().confirm ? "Snippets que pedem confirmação não podem ser rodados por agentes." : "O agente roda o snippet como está, sem poder mudar o script nem o destino, mesmo com a digitação em terminais desligada."}
          </p>
        </Show>

        <Field label="Tags"><input value={tags()} onInput={(e) => setTags(e.currentTarget.value)} /></Field>
      </div>
    </Modal>
  );
}

