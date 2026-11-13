import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { api, on, type Activity, type McpInfo } from "../api";
import { attempt, copy } from "../store";
import { Badge, Button, Field, Icon, timeAgo } from "../ui";

export function AgentsView() {
  const [info, setInfo] = createSignal<McpInfo | null>(null);
  const [activity, setActivity] = createSignal<Activity[]>([]);
  const [port, setPort] = createSignal(47011);
  const [showToken, setShowToken] = createSignal(false);
  const [client, setClient] = createSignal<"claude-code" | "claude-desktop" | "cursor" | "stdio">("claude-code");

  onMount(async () => {
    const i = await api.mcpStatus();
    setInfo(i);
    setPort(Number(i.url.match(/:(\d+)\//)?.[1] ?? 47011));
    setActivity(await api.mcpActivity());
    const un = await on<Activity>("mcp://activity", (a) => setActivity((x) => [a, ...x].slice(0, 200)));
    onCleanup(un);
  });

  const configure = async (patch: Partial<{ enabled: boolean; allowTerminalInput: boolean; allowWrites: boolean }>) => {
    const i = info();
    if (!i) return;
    const r = await attempt(() => api.mcpConfigure(
      patch.enabled ?? i.enabled, port(), patch.allowTerminalInput ?? i.allow_terminal_input, patch.allowWrites ?? i.allow_writes,
    ));
    if (r) setInfo(r);
  };

  const snippet = () => {
    const i = info();
    if (!i) return "";
    const token = showToken() ? i.token : "<TOKEN>";
    switch (client()) {
      case "claude-code":
        return `claude mcp add --transport http vessel ${i.url} --header "Authorization: Bearer ${token}"`;
      case "cursor":
        return JSON.stringify({ mcpServers: { vessel: { url: i.url, headers: { Authorization: `Bearer ${token}` } } } }, null, 2);
      case "claude-desktop":
      case "stdio":
        return JSON.stringify({
          mcpServers: { vessel: { command: i.bridge_path ?? "vessel-mcp", args: [], env: { VESSEL_MCP_URL: i.url, VESSEL_MCP_TOKEN: token } } },
        }, null, 2);
    }
  };
  const realSnippet = () => {
    const prev = showToken();
    setShowToken(true);
    const s = snippet();
    setShowToken(prev);
    return s;
  };

  return (
    <>
      <div class="page-head">
        <h1>Agentes de IA (MCP)</h1>
        <Show when={info()}>
          <Badge tone={info()!.running ? "green" : "gray"}>{info()!.running ? "ativo" : "desligado"}</Badge>
        </Show>
      </div>
      <p class="subtitle">Deixe agentes como Claude Code, Claude Desktop ou Cursor gerenciarem hosts, snippets, perfis e terminais do Vessel.
        Credenciais (senhas, chaves privadas, passphrases) nunca são expostas nem podem ser alteradas por agentes.</p>

      <Show when={info()}>
        {(i) => (
          <>
            <div class="panel">
              <div style={{ display: "flex", "align-items": "center", gap: "12px" }}>
                <div style={{ flex: 1 }}>
                  <h3>Servidor MCP local</h3>
                  <p style={{ margin: 0 }}>Escuta só em 127.0.0.1, exige token e funciona apenas com o Vessel desbloqueado.</p>
                </div>
                <Button variant={i().enabled ? "subtle" : "primary"} icon={i().enabled ? "stop" : "zap"} onClick={() => configure({ enabled: !i().enabled })}>
                  {i().enabled ? "Desativar" : "Ativar"}
                </Button>
              </div>
              <Show when={i().error}><div class="callout callout-danger" style={{ "margin-top": "12px" }}>{i().error}</div></Show>
              <div class="form" style={{ "margin-top": "16px" }}>
                <div class="form-row">
                  <Field label="Porta"><input type="number" value={port()} onInput={(e) => setPort(Number(e.currentTarget.value))} onChange={() => configure({})} /></Field>
                  <Field label="Endereço"><div class="code-block" style={{ padding: "8px 12px" }}>{i().url}</div></Field>
                </div>
                <label class="check"><input type="checkbox" checked={i().allow_writes} onChange={(e) => configure({ allowWrites: e.currentTarget.checked })} />
                  Permitir criar, editar e excluir hosts, grupos, snippets, perfis e redirecionamentos</label>
                <label class="check"><input type="checkbox" checked={i().allow_terminal_input} onChange={(e) => configure({ allowTerminalInput: e.currentTarget.checked })} />
                  Permitir digitar e executar comandos nos terminais</label>
                <Field label="Token de acesso">
                  <div style={{ display: "flex", gap: "8px" }}>
                    <input class="mono" type={showToken() ? "text" : "password"} value={i().token} readOnly />
                    <Button icon="copy" onClick={() => copy(i().token, "Token copiado")} />
                    <Button onClick={() => setShowToken(!showToken())}>{showToken() ? "Ocultar" : "Mostrar"}</Button>
                    <Button icon="sync" onClick={async () => { const r = await attempt(() => api.mcpRegenerateToken(), "Novo token gerado"); if (r) setInfo(r); }}>Gerar novo</Button>
                  </div>
                </Field>
              </div>
            </div>

            <div class="panel">
              <h3>Conectar um agente</h3>
              <div class="segmented" style={{ margin: "6px 0 12px" }}>
                <button classList={{ on: client() === "claude-code" }} onClick={() => setClient("claude-code")}>Claude Code</button>
                <button classList={{ on: client() === "claude-desktop" }} onClick={() => setClient("claude-desktop")}>Claude Desktop</button>
                <button classList={{ on: client() === "cursor" }} onClick={() => setClient("cursor")}>Cursor / HTTP</button>
                <button classList={{ on: client() === "stdio" }} onClick={() => setClient("stdio")}>Outros (stdio)</button>
              </div>
              <p>{client() === "claude-code" ? "Rode no terminal:" : client() === "cursor" ? "Adicione ao mcp.json:" : "Adicione ao arquivo de configuração MCP do cliente (usa a ponte vessel-mcp):"}</p>
              <div class="code-block">{snippet()}<button class="icon-btn" title="Copiar com o token" onClick={() => copy(realSnippet(), "Configuração copiada")}><Icon name="copy" size={15} /></button></div>
            </div>

            <div class="section-title"><Icon name="bot" size={14} />Atividade dos agentes</div>
            <div class="list">
              <Show when={activity().length} fallback={<div class="row"><span class="sub">Nenhuma ação ainda.</span></div>}>
                <For each={activity()}>
                  {(a) => (
                    <div class="activity">
                      <span class="tool">{a.tool}</span>
                      <span class="mono" style={{ color: a.ok ? "var(--text-dim)" : "var(--red)", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>{a.summary}</span>
                      <span class="when">{timeAgo(a.at)}</span>
                    </div>
                  )}
                </For>
              </Show>
            </div>
          </>
        )}
      </Show>
    </>
  );
}
