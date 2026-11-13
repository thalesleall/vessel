# Arquitetura e Protocolo — Terminal + Agente de Sync Self-Hosted

> Status: **v0.1 implementada** · Data: 2026-11-12

---

## 0. O que a v0.1 implementa, e onde ela difere deste documento

Este documento continua sendo a referência do desenho. A primeira implementação seguiu-o com estas diferenças deliberadas:

| Tema | Desenho original | Implementado na v0.1 | Motivo |
|---|---|---|---|
| Banco do agente | Postgres ou SQLite | **Só SQLite** (WAL), um escritor | `rusqlite` e `sqlx` não convivem no mesmo build; SQLite atende bem um servidor self-hosted. Postgres fica para depois. |
| Organizações | Orgs com cofres | **Compartilhamento por cofre** (convite → aprovação → grant). Não há entidade "org". | Mesmo poder com menos conceitos; orgs podem agrupar cofres depois. |
| Segredos locais do dispositivo | Windows Credential Manager | Device key e refresh token **cifrados com a chave da conta** no SQLite local | Funciona igual em Windows/macOS/Linux; só abrem depois do desbloqueio. |
| Serialização do texto claro | CBOR | **JSON** (`{"type": ..., "data": {...}}`) dentro do envelope | Depuração mais simples; o envelope é versionado, dá para trocar. |
| `entity_type` | Na AAD | **Dentro do texto cifrado** | Quem lê só conhece o tipo depois de decifrar (§5.3). |
| Recuperação de senha | Só a chave de recuperação embrulha o AK | Também um **token de autenticação derivado da chave** (`HKDF(RK)`); o servidor guarda `SHA-256(token)` e libera o re-registro OPAQUE | Sem isso, quem esqueceu a senha não consegue nem se autenticar. |
| Divisão de painéis no terminal | Previsto | Ainda não (abas, sim) | Próxima iteração. |
| Agentes de IA | Não previsto | **Servidor MCP nativo no desktop** (§14) | Pedido durante a implementação. |

---

## 1. Visão geral

Dois produtos que falam um único protocolo:

| Componente | O que é | Onde roda |
|---|---|---|
| **Desktop** | App de terminal em Rust (Tauri 2). Terminais locais (PowerShell, CMD, WSL, Git Bash) e conexões SSH. UI no estilo "hosts + abas + painéis". | Cada PC do usuário |
| **Agente** | Servidor de sincronização *zero-knowledge*. Guarda apenas blobs cifrados, chaves embrulhadas e metadados mínimos. | Container Docker self-hosted |

O usuário configura o app com: **URL do servidor + e-mail + senha mestre**. Todo dado de usuário (hosts, grupos, identidades, chaves SSH, snippets, perfis de terminal, configurações) é cifrado **no dispositivo** antes de sair dele. Três PCs logados na mesma conta veem o mesmo ambiente; os terminais em si continuam sendo processos locais de cada máquina.

### 1.1 Objetivos

- Ambiente idêntico entre dispositivos, com sync quase em tempo real.
- Times e cofres compartilhados com papéis.
- O agente comprometido **não** revela conteúdo (nem endereços, nem labels, nem portas).
- App leve e rápido (alvo: < 20 MB instalado, abrir terminal em < 150 ms).
- Self-hosting simples: `docker compose up` com Postgres ou SQLite.

### 1.2 Não-objetivos (v1)

- Sincronizar sessões de terminal em execução ou histórico de saída.
- Gravação/replay de sessões, IA no terminal, SFTP (avaliar depois).
- Federação entre servidores diferentes.
- Clientes mobile (o protocolo não deve impedir, mas não é foco).

---

## 2. Princípios de desenho

| Tema | Decisão |
|---|---|
| Onde mora a criptografia | Num crate Rust compartilhado por app e agente (o agente só usa tipos e validação, nunca chaves). |
| Chaves de cofre | Uma chave simétrica por **cofre**, entregue a cada membro por HPKE e assinada por quem concede o acesso. |
| Registros | Cada registro aponta para `vault_id` + `key_version`. |
| O que fica cifrado | **O registro inteiro.** O servidor vê só `id`, `vault_id`, versão e tamanho. |
| Login | OPAQUE: o servidor nunca vê a senha nem um verificador dela. |
| Sincronização | Cursor **monotônico do servidor** por cofre (`seq`), sem depender do relógio do cliente. |
| Tempo real | WebSocket nativo no agente, sem serviço extra. |
| Saída de membros | Rotação de chave versionada (`key_version`) ao remover um membro. |
| Evolução do formato | Envelope com campo de versão desde o dia 1. |

---

## 3. Estrutura do repositório

```
/
├─ crates/
│  ├─ vessel-crypto/      # primitivas, envelopes, embrulho de chaves, OPAQUE (cliente)
│  ├─ vessel-model/       # entidades de domínio + (de)serialização + migrações de schema
│  ├─ vessel-protocol/    # tipos da API HTTP/WS (serde), códigos de erro, versões
│  └─ vessel-sync/        # motor de sync do cliente (fila de push, pull, merge)
├─ apps/
│  ├─ desktop/            # Tauri 2 (src-tauri em Rust + ui/ em SolidJS)
│  └─ agent/              # axum + sqlx, binário único, Dockerfile
├─ docs/
│  └─ ARCHITECTURE.md
└─ Cargo.toml             # workspace
```

Regra: **app e agente dependem de `vessel-protocol`**; só o app depende de `vessel-crypto` com chaves privadas. O agente usa de `vessel-crypto` apenas o lado servidor do OPAQUE e validação de formato.

---

## 4. Modelo de ameaças

| Adversário | Pode | Não deve conseguir |
|---|---|---|
| Servidor/agente comprometido (ou operador curioso) | Ler o banco, ver tráfego após TLS, alterar respostas | Ler qualquer conteúdo; obter a senha mestre; fazer ataque offline de dicionário eficiente |
| Atacante de rede | Interceptar tráfego | Nada além do que TLS protege |
| Membro removido de um cofre | Ter guardado a chave antiga | Ler registros criados/alterados após a remoção |
| Dispositivo roubado e bloqueado | Copiar disco | Decifrar o cofre local sem senha mestre ou credencial do SO |
| Servidor malicioso trocando chaves públicas | Entregar uma chave pública falsa num convite | Passar despercebido se a impressão digital for verificada (ver §7.4) |

Fora do escopo: dispositivo desbloqueado com malware; servidor que nega serviço ou apaga dados (mitigado por backups e cache local).

---

## 5. Criptografia

### 5.1 Primitivas

| Uso | Algoritmo | Crate |
|---|---|---|
| Cifra simétrica de registros | XChaCha20-Poly1305 (nonce aleatório de 24 bytes) | `chacha20poly1305` |
| Embrulho de chave para um destinatário | HPKE RFC 9180: X25519 + HKDF-SHA256 + ChaCha20-Poly1305 | `hpke` |
| Assinaturas (concessões, dispositivos) | Ed25519 | `ed25519-dalek` |
| Login sem enviar senha | OPAQUE (RFC 9807) com Argon2id como KSF | `opaque-ke`, `argon2` |
| Derivação | HKDF-SHA256 | `hkdf` |
| Aleatoriedade | CSPRNG do SO | `rand_core::OsRng` |
| Zeragem de segredos em memória | — | `zeroize`, `secrecy` |

Parâmetros Argon2id iniciais: `m = 64 MiB, t = 3, p = 1` (ajustáveis por conta e versionados).

### 5.2 Hierarquia de chaves

```
senha mestre
   │  OPAQUE (Argon2id no cliente)
   ▼
export_key (32 B, só o cliente conhece)
   │  HKDF("vessel/v1/master-kek")
   ▼
MK  — Master KEK ───────────────┐
                                │ embrulha
Recovery Key (RK, 32 B aleatória, mostrada ao usuário uma vez)
                                │ embrulha
                                ▼
AK  — Account Key (32 B aleatória, simétrica)
   │ cifra
   ├─► chave privada X25519 do usuário  (UXK)  → recebe chaves de cofre
   └─► chave privada Ed25519 do usuário (USK)  → assina concessões

VK  — Vault Key (32 B aleatória, uma por cofre e por key_version)
   │ embrulhada com HPKE para a UXK pública de cada membro
   ▼
registros do cofre (XChaCha20-Poly1305)
```

- O **cofre pessoal** é só um cofre com um único membro. Não existe caminho especial.
- Trocar a senha mestre = reembrulhar o AK com um novo MK. Nenhum dado é recifrado.
- A Recovery Key permite recuperar a conta sem a senha mestre. Ela também embrulha o AK.

### 5.3 Envelope de registro

Todo blob armazenado no agente segue um formato binário versionado:

```
 0      1        2            26                         N
 ├──────┼────────┼────────────┼──────────────────────────┤
 │ ver  │ alg    │ nonce(24)  │ ciphertext + tag(16)     │
 │ 0x01 │ 0x01   │            │                          │
```

AAD (dados associados, não transmitidos, recomputados pelos dois lados):

```
"vessel/v1/record" || ver || alg || vault_id(16) || record_id(16) || key_version(u32 BE)
```

Isso impede que o servidor troque blobs entre registros, cofres ou versões de chave sem detecção. O cabeçalho (`ver`, `alg`) também é autenticado, o que bloqueia *downgrade*. O `entity_type` fica **dentro** do texto claro e não na AAD, porque quem lê só conhece o tipo depois de decifrar.

O texto claro é o registro serializado com **CBOR** e com *padding* até múltiplo de 256 bytes, para reduzir o vazamento por tamanho.

### 5.4 Concessão de chave de cofre (grant)

```
Grant {
  vault_id, key_version,
  recipient_user_id, recipient_xpub_fingerprint,
  hpke_enc,            // encapsulated key
  wrapped_vk,          // HPKE.Seal(recipient_xpub, info="vessel/v1/vk-grant"||vault_id||key_version, VK)
  granter_user_id,
  signature            // Ed25519(USK_granter, todos os campos acima)
}
```

O destinatário só aceita o grant se a assinatura for de um membro com papel `admin` ou `owner` naquele cofre, segundo o histórico assinado de membros (§7.3).

---

## 6. Autenticação, dispositivos e sessões

### 6.1 Cadastro

1. O cliente roda o registro OPAQUE e obtém `export_key`.
2. Deriva o MK. Gera AK, RK, UXK e USK.
3. Envia ao servidor:
   - o registro OPAQUE;
   - `AK` embrulhado por MK e `AK` embrulhado por RK;
   - as chaves públicas UXK e USK, e as privadas cifradas pelo AK;
   - o cofre pessoal com o grant para si mesmo.
4. Mostra a Recovery Key para o usuário guardar.

### 6.2 Login num dispositivo novo

1. URL do servidor + e-mail + senha → OPAQUE login (2 round-trips) → `export_key` + chave de sessão.
2. O servidor emite um `access_token` (15 min) e um `refresh_token` vinculado ao `device_id`.
3. O dispositivo gera um par Ed25519 próprio (**device key**) e registra a chave pública. O refresh exige assinatura com essa chave, o que torna um token roubado inútil sem o dispositivo.
4. O cliente baixa o AK embrulhado, desembrulha com o MK, decifra UXK/USK, baixa os grants e, por fim, o pull dos cofres.

### 6.3 Desbloqueio local

O MK depende do OPAQUE, que exige o servidor. Para o app abrir **offline**, existe uma chave de desbloqueio local separada:

- No login, o cliente gera um `local_salt` e deriva `LK = Argon2id(senha mestre, local_salt)`. O AK é guardado no disco embrulhado por LK.
- O app abre bloqueado. A senha mestre deriva o LK e desembrulha o AK sem precisar da rede; se o desembrulho falhar, a senha está errada.
- A device key e o refresh token ficam no SQLite local, **cifrados com a chave da conta**. Só abrem depois do desbloqueio (ver §0).
- Numa fase posterior, o Windows Hello pode guardar uma segunda cópia do AK protegida pelo TPM, para desbloqueio sem digitar a senha.
- Se a senha mestre mudar em outro dispositivo, o desbloqueio local continua com a senha antiga até o próximo login online, que regenera o LK.
- Bloqueio automático por inatividade (configurável).

### 6.4 Gestão de dispositivos

- Listar e revogar dispositivos. Revogar invalida tokens imediatamente.
- Revogar não recifra dados, já que o dispositivo tinha as chaves. Para isso existe a rotação do §7.5.

---

## 7. Cofres, times e permissões

### 7.1 Conceitos

- **Organização (time):** grupo de usuários com um dono. Agrupa cofres e convites.
- **Cofre:** unidade de compartilhamento e de chave. Um usuário pode ter acesso a vários.
- **Papéis por cofre:**

| Papel | Ler | Editar registros | Gerenciar membros | Rotacionar/excluir cofre |
|---|---|---|---|---|
| `viewer` | ✓ | | | |
| `editor` | ✓ | ✓ | | |
| `admin` | ✓ | ✓ | ✓ | ✓ |
| `owner` | ✓ | ✓ | ✓ | ✓ (único que transfere posse) |

O servidor **aplica** as permissões (ACL) sobre quem pode escrever blobs e grants. A criptografia garante a **confidencialidade**: sem grant, não há VK.

### 7.2 Convite

1. Um admin cria o convite → link `https://<servidor>/invite/<token>` (token de uso único, expira em 7 dias).
2. O convidado abre o link no app, cria conta ou faz login e aceita.
3. O admin recebe a notificação "pendente de concessão" e o app mostra a **impressão digital** da chave do convidado (§7.4).
4. O app do admin cria o grant (§5.4) e envia. O convidado passa a ver o cofre.

O passo 3–4 exige um admin online. Isso é intencional: o servidor nunca pode conceder acesso sozinho.

### 7.3 Histórico assinado de membros

Cada mudança de membro (adicionar, mudar papel, remover) é um evento assinado pelo admin que a fez, encadeado por hash:

```
MembershipEvent { vault_id, seq, prev_hash, action, subject_user_id, role, actor_user_id, signature }
```

Os clientes validam a cadeia desde a criação do cofre. Um servidor que injete um membro falso é detectado, porque não consegue assinar o evento.

### 7.4 Verificação de chave pública

- Impressão digital = os primeiros 20 bytes de `SHA-256(UXK_pub || USK_pub)`, mostrados como 5 grupos de palavras ou números.
- No convite, o admin pode confirmar a impressão por um canal externo (opcional, mas recomendado e destacado na UI).
- Os clientes fixam a chave conhecida de cada colega (TOFU). Uma troca de chave gera alerta.

### 7.5 Remoção e rotação

Quando alguém é removido de um cofre, ou um dispositivo é revogado por suspeita de comprometimento:

1. O servidor bloqueia imediatamente o acesso do removido (ACL).
2. O app do admin gera `VK` nova (`key_version + 1`), cria grants para os membros restantes e recifra todos os registros do cofre em lote.
3. Enquanto isso, o cofre fica marcado como `rotating`: as escritas novas já usam a versão nova, e as leituras aceitam as duas.
4. Concluída a rotação, os grants antigos são apagados.

---

## 8. Modelo de dados (cliente)

Entidades sincronizadas, cada uma com um tipo e um schema versionado:

| Entidade | Campos principais |
|---|---|
| `host` | label, address, port, protocol (`ssh`/`telnet`/`local`), group_id, identity_id, tags, jump_host_ids, opções de terminal |
| `group` | label, parent_group_id, configurações herdadas |
| `identity` | label, username, password?, ssh_key_id? |
| `ssh_key` | label, private_key, public_key, passphrase?, certificates |
| `known_host` | hostnames, key |
| `snippet` | label, script, tags, package_id |
| `port_forward` | host_id, tipo (L/R/D), endereços e portas |
| `terminal_profile` | label, shell (`pwsh`/`powershell`/`cmd`/`wsl:<distro>`/`gitbash`/custom), args, cwd, env, tema |
| `workspace` | layout salvo: abas, divisões de painel, perfis e hosts |
| `settings` | preferências sincronizáveis (tema, fonte, atalhos) |

**Perfis de terminal sincronizam; processos não.** Se um perfil apontar para `wsl:Ubuntu-24.04` e esse PC não tiver essa distro, o app mostra "indisponível neste dispositivo" em vez de falhar.

Identificadores: UUIDv7 gerado no cliente (ordenável, sem colisão, funciona offline). Referências entre entidades só por id, dentro do texto cifrado.

---

## 9. Protocolo de sincronização

### 9.1 Registro como o agente o vê

```json
{
  "vault_id": "0193…",
  "id": "0193…",
  "version": 7,            // incrementado pelo servidor a cada escrita aceita
  "seq": 18234,            // posição global no log do cofre (monotônica)
  "key_version": 2,
  "deleted": false,        // tombstone
  "blob": "base64(envelope)",
  "updated_at": "2026-11-12T13:01:22Z", // relógio do servidor, só informativo
  "updated_by_device": "0193…"
}
```

`entity_type` **não** é exposto: fica dentro do texto cifrado (e na AAD). O agente não sabe se o registro é host, chave ou snippet.

### 9.2 Pull

```
GET /v1/vaults/{vault_id}/changes?since=<seq>&limit=500
→ 200 { "changes": [Record…], "next_seq": 18234, "has_more": false }
```

O cliente guarda `next_seq` por cofre. Primeiro sync: `since=0`.

### 9.3 Push

```
POST /v1/vaults/{vault_id}/changes
{ "ops": [ { "id", "base_version", "key_version", "deleted", "blob" } … ] }   // até 500 ops
→ 200 { "results": [ { "id", "status": "ok", "version", "seq" }
                   | { "id", "status": "conflict", "current": Record } ] }
```

- `base_version` = a versão que o cliente tinha quando editou. `0` significa criação.
- O servidor aceita só se `base_version == version atual` (controle otimista). Senão, devolve `conflict` com o registro atual.
- Cada op é atômica individualmente; o lote não é transacional como um todo.

### 9.4 Resolução de conflito (no cliente)

O cliente tem as três versões: **base** (cache local), **local** e **remota**. Ele faz merge de três vias **campo a campo**:

- Campos alterados só de um lado → aplica.
- Mesmo campo alterado nos dois lados → vence a edição local, que é a que vai ser enviada agora. Na prática, vence quem envia por último, sem depender do relógio de nenhum dispositivo. O valor perdedor vai para o "histórico de conflitos" visível na UI.
- Exclusão vs. edição → a edição vence e o registro é restaurado. Excluir algo que outra pessoa acabou de editar costuma ser engano.

Depois do merge, faz um novo push com `base_version` = versão remota.

### 9.5 Tempo real

```
GET /v1/events  (WebSocket, autenticado pelo access_token)
servidor → { "type": "vault_changed", "vault_id", "seq" }
servidor → { "type": "grant_added" | "grant_revoked" | "member_changed", "vault_id" }
servidor → { "type": "device_revoked" }
```

O evento só avisa, e o cliente faz o pull. Isso mantém um único caminho de dados e torna o WebSocket opcional: sem ele, o cliente faz pull a cada 60 s e quando a janela ganha foco.

### 9.6 Offline

- Toda edição vai primeiro para o SQLite local e para a fila `pending_ops`.
- O app funciona totalmente offline com os dados já sincronizados.
- Ao reconectar: push da fila, depois pull.

---

## 10. Agente (servidor)

### 10.1 Stack

- Rust, `axum` (HTTP + WebSocket), `sqlx`, `tokio`.
- Banco: **SQLite** em modo WAL na v0.1 (ver §0). Postgres é a evolução prevista para servidores com muitos times.
- Binário único, imagem `distroless`/`scratch` de aproximadamente 20 MB.

### 10.2 API (v1)

| Método | Rota | Descrição |
|---|---|---|
| POST | `/v1/auth/register/start` · `/finish` | Cadastro OPAQUE |
| POST | `/v1/auth/login/start` · `/finish` | Login OPAQUE → tokens |
| POST | `/v1/auth/refresh` | Novo access token (assinado pela device key) |
| POST | `/v1/auth/logout` | Revoga a sessão atual |
| GET/PUT | `/v1/account/keys` | AK embrulhado, chaves públicas e privadas cifradas |
| GET/DELETE | `/v1/devices[/{id}]` | Lista/revoga dispositivos |
| GET/POST | `/v1/orgs` | Organizações |
| GET/POST | `/v1/orgs/{id}/invites` | Convites |
| POST | `/v1/invites/{token}/accept` | Aceitar convite |
| GET/POST | `/v1/vaults` | Listar/criar cofres |
| GET/POST | `/v1/vaults/{id}/members` | Membros + eventos assinados |
| GET/POST | `/v1/vaults/{id}/grants` | Grants de chave |
| POST | `/v1/vaults/{id}/rotate` | Inicia/conclui rotação |
| GET/POST | `/v1/vaults/{id}/changes` | Pull/push (§9) |
| GET | `/v1/users/{id}/public-keys` | Chaves públicas para concessão |
| GET | `/v1/events` | WebSocket |
| GET | `/v1/server/info` | Versão, limites e capacidades (o app consulta ao adicionar a URL) |

### 10.3 Schema (resumo)

```sql
users(id, email, opaque_record, wrapped_ak_mk, wrapped_ak_rk,
      uxk_pub, usk_pub, enc_uxk_priv, enc_usk_priv, kdf_params, created_at)
devices(id, user_id, name, device_pub, last_seen_at, revoked_at)
sessions(id, device_id, refresh_hash, expires_at)
orgs(id, name, owner_id)
org_members(org_id, user_id, role)
vaults(id, org_id NULL, current_key_version, state)          -- state: active|rotating
vault_members(vault_id, user_id, role)
membership_events(vault_id, seq, prev_hash, payload, signature)
grants(vault_id, key_version, recipient_id, payload)         -- payload = Grant assinado
records(vault_id, id, version, seq, key_version, deleted, blob, updated_at, updated_by_device,
        PRIMARY KEY (vault_id, id))
vault_seq(vault_id, last_seq)                                 -- contador por cofre
invites(token_hash, org_id, vault_id NULL, role, expires_at, accepted_by NULL)
audit_log(id, actor_id, action, target, at)                   -- sem conteúdo
```

O `seq` é atribuído dentro da transação de escrita (`UPDATE vault_seq … RETURNING`), o que garante ordem total por cofre.

### 10.4 Deploy

```yaml
# docker-compose.yml (exemplo)
services:
  agent:
    image: ghcr.io/<org>/vessel-agent:0.1
    environment:
      VESSEL_DATABASE_URL: postgres://vessel:***@db/vessel
      VESSEL_PUBLIC_URL: https://vessel.exemplo.com
      VESSEL_SIGNUPS: invite-only        # open | invite-only | disabled
    depends_on: [db]
  db:
    image: postgres:17
    volumes: [pgdata:/var/lib/postgresql/data]
  caddy:                                  # TLS automático
    image: caddy:2
    ports: ["443:443"]
volumes: { pgdata: {} }
```

- TLS fica no reverse proxy (Caddy ou Traefik). O agente também aceita certificado próprio.
- Backup = dump do banco. Como tudo está cifrado, o backup pode ir para storage não confiável.
- Rate limit no login e no cadastro. O primeiro usuário cadastrado vira administrador do servidor.

---

## 11. App desktop

### 11.1 Stack

- **Tauri 2** (WebView2 no Windows) com backend em Rust.
- **UI:** SolidJS + TypeScript + Tailwind, componentes próprios. Tema claro/escuro e densidade compacta.
- **Terminal:** xterm.js com addons `webgl`, `fit`, `search`, `unicode11` e `web-links`.
- **Armazenamento local:** SQLite (`rusqlite`), com os registros guardados **ainda cifrados** (o mesmo envelope do servidor) e decifrados só em memória após o desbloqueio.

### 11.2 Terminais locais

- **PTY:** `portable-pty` (ConPTY no Windows 10 1809+).
- **Descoberta automática de shells:**
  - `pwsh.exe` (PowerShell 7) e `powershell.exe` (5.1), `cmd.exe`;
  - distros WSL via `wsl.exe --list --quiet` (ou registro `HKCU\…\Lxss`), abertas com `wsl.exe -d <distro>`;
  - Git Bash, se encontrado.
- O stream do PTY vai do Rust para a UI por um canal Tauri em binário (sem JSON por byte), com *batching* de ~4 ms para manter a fluidez em saídas grandes.
- Integração de shell (OSC 133 / OSC 7) para saber o diretório atual e marcar prompts, numa fase posterior.

### 11.3 SSH

- Crate `russh` (cliente SSH puro Rust), com:
  - autenticação por chave, senha e agente (Pageant/OpenSSH agent no Windows);
  - jump hosts e port-forwarding local, remoto e dinâmico (SOCKS);
  - verificação de `known_host`, sincronizado como entidade.
- As chaves privadas nunca vão para disco em claro: ficam no SQLite cifrado e são decifradas em memória só no momento da conexão.

### 11.4 Interface

```
┌─────────────┬──────────────────────────────────────────────────────┐
│ Hosts       │  [PowerShell] [Ubuntu (WSL)] [prod-db ⚡] [+]          │
│ Terminais   ├──────────────────────────────────────────────────────┤
│ Chaves      │                                                      │
│ Snippets    │                 terminal / divisões                   │
│ Port fwd    │                                                      │
│ Cofres      │                                                      │
│ ─────────── │                                                      │
│ ⚙ Config    │                                                      │
│ ● sync ok   ├──────────────────────────────────────────────────────┤
└─────────────┴──────────────────────────── barra de status ────────┘
```

- Paleta de comandos (`Ctrl+K`) para abrir host, perfil ou snippet.
- Painéis divididos e workspaces salvos.
- Indicador de sync com estado offline, sincronizando e conflito.

---

## 12. Roadmap

| Fase | Entrega | Critério de pronto |
|---|---|---|
| **0 — Fundação** | Workspace Cargo; `vessel-crypto` com envelopes, HPKE grant, OPAQUE; testes e vetores de teste | Round-trip cifra/decifra, grant entre dois usuários, OPAQUE cliente↔servidor em teste |
| **1 — Terminal local** | App Tauri com abas, PowerShell/CMD/WSL, perfis locais, tema | Abrir 10 abas, `cat` de 50 MB sem travar a UI |
| **2 — Agente pessoal** | Cadastro/login, cofre pessoal, pull/push, WebSocket, Docker | Mesmo host criado no PC A aparece no PC B em < 2 s |
| **3 — SSH** | Hosts, identidades, chaves, known hosts, jump host, port-forward | Conectar via jump host com chave sincronizada |
| **4 — Times** | Orgs, convites, papéis, grants, histórico assinado, rotação | Remover um membro e verificar que ele não lê registros novos |
| **5 — Polimento** | Snippets, workspaces, paleta, Windows Hello, auto-update, import de `~/.ssh/config` | — |

---

## 13. Questões em aberto

1. **Nome do projeto** e da organização no registry de imagens.
2. **Ofuscar o número de registros por cofre?** Hoje o servidor sabe quantos registros cada cofre tem e o tamanho aproximado de cada um. Dá para mitigar com registros-isca, ao custo de complexidade.
3. **SSO / OIDC para times:** é incompatível com zero-knowledge puro sem uma etapa extra de desbloqueio. Deixar para depois da v1?
4. **Suporte a macOS/Linux** no app: o desenho permite (Tauri + `portable-pty`), mas a v1 testa só Windows?
5. **Limite de tamanho de registro** (sugestão: 256 KiB) e de cofre.

---

## 14. Integração com agentes de IA (MCP)

O app desktop embute um servidor **MCP** (Model Context Protocol) para que agentes como Claude Code, Claude Desktop ou Cursor operem o Vessel. O servidor fica no desktop, e não no agente de sync, porque só ali os dados estão decifrados.

### 14.1 Transporte e acesso

- **Streamable HTTP** com respostas JSON em `http://127.0.0.1:<porta>/mcp` (padrão 47011). Escuta só em loopback.
- **Token Bearer** obrigatório (`vsl_…`, 256 bits aleatórios), gerado no app e regenerável.
- **Proteção contra DNS rebinding:** requisições com `Origin` que não seja localhost são recusadas.
- Só funciona com o app **desbloqueado**. Bloqueado, toda ferramenta responde "Vessel is locked".
- Para clientes que só falam stdio existe a ponte `vessel-mcp` (`apps/mcp-bridge`), configurada com `VESSEL_MCP_URL` e `VESSEL_MCP_TOKEN`.
- Toda chamada aparece no app (Agentes de IA → Atividade), e as sessões abertas por agentes recebem o selo "IA" na aba.
- Duas chaves no app: **permitir edições** e **permitir digitar nos terminais**.
- Com "digitar" desligado, o agente ainda abre e lê terminais, mas nada que execute comandos sozinho: só shells embutidos sem argumentos nem variáveis de ambiente (sem `custom`, sem perfis com `args`/`env`), e nenhum host com "Snippet ao conectar".

### 14.2 O que os agentes podem fazer

| Área | Ferramentas |
|---|---|
| Estado | `vessel_status`, `list_vaults`, `list_vault_members`, `list_conflicts`, `sync_now` |
| Itens | `list_items`, `get_item`, `create_item`, `update_item`, `delete_item`, `move_item` |
| Terminais locais | `list_shells`, `open_terminal` (perfil, PowerShell, CMD, WSL, Git Bash) |
| SSH | `connect_ssh` (com as credenciais salvas, sem vê-las) |
| Uso do terminal | `terminal_run` (envia e espera a saída estabilizar), `terminal_write`, `terminal_read`, `terminal_close`, `run_snippet`, `list_terminals` |
| Redirecionamentos | `list_port_forwards`, `start_port_forward`, `stop_port_forward` |

### 14.3 O que os agentes **não** podem fazer

- **Ler credenciais:** `password`, `private_key` e `passphrase` são removidos de toda saída e trocados por `has_password`, `has_private_key` e `has_passphrase`. Conflitos de sincronização nesses campos mostram só o nome do campo.
- **Gravar credenciais:** qualquer `create_item` ou `update_item` que contenha esses campos é recusado. Em edições, os segredos existentes são preservados.
- **Alterar identidades, chaves SSH e hosts conhecidos:** são só leitura para agentes. Os dois primeiros são credenciais, e o terceiro decide em quais servidores confiar. Agentes podem referenciá-los por id ao criar hosts.
- **Gestão de conta e de time:** senha, chave de recuperação, dispositivos, convites, aprovação de membros e rotação de chave ficam de fora.
- **Aceitar uma chave de host *alterada*:** é sempre recusado. Uma chave *nova* só é aceita com `trust_new_host_key: true`, e as instruções do servidor pedem que o agente confirme isso com o usuário.

Limitação conhecida: um agente com permissão de digitar no terminal pode rodar comandos que leiam arquivos de credenciais *no servidor remoto* (ex.: `cat ~/.ssh/id_rsa`). A proteção do Vessel cobre os segredos guardados no cofre, não o que o terminal remoto expõe. Desligue "permitir digitar nos terminais" quando isso importar.
