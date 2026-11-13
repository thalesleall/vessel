# Vessel

Terminal e cliente SSH para desktop, com **sincronização criptografada de ponta a ponta por um servidor self-hosted** e **integração nativa com agentes de IA via MCP**.

- **Funciona sem servidor:** no modo "Só neste computador" tudo fica local e cifrado. Dá para conectar um servidor depois, ou voltar, sem perder dados.
- **Terminais locais:** PowerShell, CMD, distribuições WSL e Git Bash rodam na sua máquina (ConPTY).
- **O resto sincroniza** entre todos os seus computadores: hosts, grupos, identidades, chaves SSH, snippets, perfis de terminal, redirecionamentos de porta, hosts conhecidos e configurações. Os dados são cifrados no seu computador antes de sair dele.
- **Agente de sync self-hosted** em um container Docker. Ele guarda só blobs cifrados e não consegue ler nada: nem endereços, nem nomes, nem portas.
- **Times:** cofres compartilhados com papéis (leitor, editor, admin e dono), convites, aprovação com conferência da impressão digital e troca de chave ao remover alguém.
- **Agentes de IA (MCP):** Claude Code, Claude Desktop, Cursor e outros podem gerenciar hosts, snippets, perfis e terminais. Credenciais ficam sempre fora do alcance deles.

O desenho completo, com modelo de ameaças, protocolo e criptografia, está em [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). As decisões visuais (paleta, tipografia, temas claro e escuro) estão em [docs/DESIGN.md](docs/DESIGN.md).

## Estrutura

```
crates/
  vessel-crypto/     envelopes XChaCha20-Poly1305, OPAQUE+Argon2id, grants HPKE, histórico de membros assinado
  vessel-protocol/   tipos da API (compartilhados por agente e clientes)
  vessel-model/      entidades sincronizadas + merge de três vias por campo
  vessel-client/     núcleo do cliente: conta, store local cifrado, sync, cofres e times
apps/
  agent/             servidor de sync (axum + SQLite) e Dockerfile
  desktop/           app Tauri 2 (Rust) + interface SolidJS/xterm.js; inclui o servidor MCP
  mcp-bridge/        `vessel-mcp`: ponte stdio → MCP HTTP do app
deploy/              docker-compose + Caddy (HTTPS automático)
```

## Subindo o agente (servidor)

Num servidor com Docker e um domínio apontando para ele:

```bash
cd deploy
cp .env.example .env    # defina VESSEL_DOMAIN
docker compose up -d
```

A primeira conta criada vira administradora do servidor. Com `VESSEL_SIGNUPS=invite-only` (o padrão), as contas seguintes precisam de convite, gerado no app em Configurações → Administração do servidor. Convites para cofres também permitem o cadastro.

Variáveis de ambiente: `VESSEL_PUBLIC_URL` (obrigatória; é também a identidade OPAQUE do servidor, então não a troque depois), `VESSEL_SIGNUPS` (`open`, `invite-only` ou `disabled`), `VESSEL_DATABASE`, `VESSEL_BIND`, `VESSEL_NAME`, `VESSEL_TRUST_PROXY` e `VESSEL_AUTH_RATE_PER_MIN`.

Para testar localmente sem Docker:

```bash
VESSEL_PUBLIC_URL=http://127.0.0.1:8080 VESSEL_BIND=127.0.0.1:8080 VESSEL_DATABASE=./vessel.db VESSEL_SIGNUPS=open cargo run -p vessel-agent
```

## App desktop

Pré-requisitos: Rust (stable), Node 20+ e, no Windows, as Build Tools do Visual Studio com C++ e o WebView2.

```bash
cd apps/desktop
npm install
npx tauri dev       # desenvolvimento
npx tauri build     # instalador (NSIS) em target/release/bundle
```

Para ver a interface no navegador com um backend falso, sem o Tauri, rode `npm run dev` e abra `http://localhost:1420/?mock` (conta em servidor), `?mock=local` (modo local) ou `?mock=welcome` (primeira execução).

Atalhos: `Ctrl+K` abre a paleta de comandos, `Ctrl+T` abre um novo terminal e `Ctrl+L` bloqueia. No terminal: `Ctrl+Shift+C` e `Ctrl+Shift+V`, e o botão direito copia ou cola.

## Agentes de IA (MCP)

No app, abra **Agentes de IA → Ativar**. O servidor MCP escuta em `http://127.0.0.1:47011/mcp`, só com token e só enquanto o app está desbloqueado.

Claude Code:

```bash
claude mcp add --transport http vessel http://127.0.0.1:47011/mcp --header "Authorization: Bearer <TOKEN>"
```

Para clientes que só falam stdio, compile a ponte com `cargo build --release -p vessel-mcp` e configure:

```json
{ "mcpServers": { "vessel": { "command": "C:\\caminho\\vessel-mcp.exe", "env": { "VESSEL_MCP_TOKEN": "<TOKEN>" } } } }
```

O que os agentes podem e não podem fazer está em [ARCHITECTURE.md §14](docs/ARCHITECTURE.md). Em resumo: tudo, exceto ler ou alterar senhas, chaves privadas e passphrases, mexer em identidades, chaves e hosts conhecidos, e gerenciar conta, dispositivos e membros. Cada ação aparece no log de atividade do app.

## Testes

```bash
cargo test --workspace     # criptografia, protocolo, merge e testes de ponta a ponta agente↔clientes
cargo clippy --workspace --all-targets -- -D warnings
cd apps/desktop && npx tsc --noEmit
```

**Laboratório local** (Docker): `powershell -File deploy\dev\lab.ps1 up` sobe o agente em `http://localhost:8080` e dois servidores SSH de teste (bastion e um alvo só alcançável através dele). `cargo test -p vessel-desktop --lib -- --ignored lab_` testa o SSH e os port forwards do app contra eles, e `lab.ps1 bot <comando>` controla uma segunda conta (`vessel-dev`) para testar times. O roteiro manual completo está em [docs/TESTING.md](docs/TESTING.md).

Os testes de ponta a ponta (`crates/vessel-client/tests/e2e.rs`) sobem um agente real em memória e cobrem:
- três PCs com o mesmo ambiente;
- merge de edições simultâneas;
- desbloqueio offline;
- compartilhamento em time com aprovação e remoção com troca de chave;
- recuperação de conta pela chave de recuperação;
- troca de senha;
- notificações por WebSocket;
- cadastro só por convite.
