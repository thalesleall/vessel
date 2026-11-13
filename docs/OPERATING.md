# Operar um servidor Vessel

Guia para quem roda o agente de sincronização, seja para uso próprio ou como serviço vendido. Arquitetura: `docs/ARCHITECTURE.md`.

## Subir

```bash
cd deploy
cp .env.example .env    # defina VESSEL_DOMAIN e o resto
docker build -f ../apps/agent/Dockerfile -t vessel-agent:latest ..
docker compose up -d
docker compose logs agent    # mostra o código de instalação da primeira conta
```

Abra o app, aponte para `https://SEU_DOMINIO` e crie a primeira conta com o código de instalação que aparece no log. Essa conta é a administradora do servidor.

## Variáveis

| Variável | Padrão | O que faz |
|---|---|---|
| `VESSEL_PUBLIC_URL` | obrigatória | Endereço público. Também é a identidade OPAQUE do servidor: se mudar, todas as senhas deixam de funcionar. |
| `VESSEL_SIGNUPS` | `invite-only` | `open`, `invite-only` ou `disabled`. |
| `VESSEL_BOOTSTRAP_TOKEN` | gerado | Código da primeira conta. Se ficar vazio, o agente gera um e o mostra no log enquanto não houver contas. |
| `VESSEL_VAULT_INVITE_SIGNUPS` | `true` | Use `false` para que convites de cofre não criem contas novas, por exemplo numa nuvem vendida. |
| `VESSEL_QUOTA_MB` | `0` | Espaço por conta (soma dos cofres de que ela é dona). `0` é sem limite. |
| `VESSEL_MAX_VAULTS` | `0` | Cofres compartilhados por conta. `0` é sem limite. |
| `VESSEL_TRUST_PROXY` | `0` | Quantos proxies reversos ficam na frente do agente: `true` ou `1` só com o Caddy, `2` com uma CDN antes dele. |
| `VESSEL_AUTH_RATE_PER_MIN` | `30` | Tentativas de autenticação por IP (ou /64 de IPv6) por minuto. Por conta, o limite fixo é 20. |
| `VESSEL_NAME` | `Vessel` | Nome mostrado no app. |
| `VESSEL_DATABASE` | `/data/vessel.db` | Arquivo SQLite. |
| `VESSEL_BIND` | `0.0.0.0:8080` | Endereço interno. Não publique essa porta: o tráfego passa pelo Caddy. |
| `RUST_LOG` | `info` | Não use `debug` em produção, porque as URLs com tokens de convite aparecem nos logs. |

## Administração

No app, com uma conta administradora, abra **Configurações → Administração do servidor**. Lá você pode:
- gerar convites de cadastro;
- ver o uso do servidor;
- listar contas;
- suspender e reativar contas;
- dar ou tirar o papel de admin (o servidor sempre mantém pelo menos um);
- definir a cota de cada conta;
- excluir contas.

Os administradores não veem o conteúdo de nenhum cofre.

A exclusão de conta pode partir do próprio usuário (Configurações → Conta → Excluir minha conta) ou do operador. Em qualquer caso:
- os cofres de que a conta é dona são apagados, com os itens, as chaves e o histórico;
- os dispositivos saem;
- o e-mail fica livre.

O registro da conta fica **pseudonimizado** (`deleted-…@deleted.invalid`), mas as chaves públicas são mantidas, porque assinam eventos no histórico dos cofres de outras pessoas. Quando é o operador quem exclui, a conta continua aparecendo como membro nos cofres de terceiros até um gerente removê-la.

## Backup

O banco guarda o segredo OPAQUE do servidor ao lado dos arquivos de senha. **Um backup em claro permite testar senhas offline contra todas as contas.** Por isso o agente só produz backups cifrados:

```bash
docker compose exec -e VESSEL_BACKUP_PASSPHRASE agent vessel-agent backup /data/backup-$(date +%F).vsl
```

Restaurar, sempre num arquivo novo:

```bash
docker compose exec -e VESSEL_BACKUP_PASSPHRASE agent vessel-agent restore /data/backup.vsl /data/restored.db
```

- **Passphrase:** pelo menos 16 caracteres. Ela vem da variável de ambiente e nunca de um argumento, que ficaria no histórico do shell. Guarde-a fora do servidor e longe dos backups.
- **Cópia consistente:** o backup é tirado com o servidor no ar (`VACUUM INTO`).
- **Formato:** cifrado em blocos autenticados. Um arquivo truncado, reordenado ou adulterado é recusado.

## Escala

- **Limite de um processo:** um único processo com SQLite em modo WAL, uma conexão de escrita e quatro de leitura. Isso atende bem até alguns milhares de contas ativas.
- **Isolamento entre contas:** cada conta tem limite de escrita (240 envios por minuto) e cota de espaço. Os eventos em tempo real usam um canal por usuário, então uma conta muito ativa não derruba os sockets das outras.
- **Para crescer além disso:** trocar o SQLite por Postgres. A camada `state.db()` / `state.db_read()` concentra o acesso ao banco.

## Release

- **Imagens:** as imagens base estão fixadas por digest (`apps/agent/Dockerfile`, `deploy/docker-compose.yml`, `deploy/dev/sshd/Dockerfile`). Para atualizar, rode `docker buildx imagetools inspect <imagem>` e troque o digest num commit próprio.
- **Instalador do Windows:** precisa ser assinado com um certificado de assinatura de código (EV ou OV). No `apps/desktop/src-tauri/tauri.conf.json`, preencha `bundle.windows.certificateThumbprint` (ou use `signCommand`) na máquina de build que tem o certificado. Sem assinatura, o SmartScreen avisa os usuários e não há como provar a origem do instalador.
- **Antes de publicar:** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `npx tsc --noEmit` em `apps/desktop`, e os testes do laboratório SSH (`docs/TESTING.md`).
