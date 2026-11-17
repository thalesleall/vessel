# Brief: menu AWS (inventário e acesso via SSM)

Status: implementado.

## Objetivo
Um menu novo, **AWS**, que:
- descobre os perfis do AWS CLI da máquina;
- mapeia as instâncias de todas as contas e regiões;
- deixa o acesso SSM a cada uma a um clique.

## Decisões tomadas
1. **O mapeamento é sob demanda.** Ele roda num botão "Mapear tudo", com o aviso de que pode demorar e vai consultar todas as contas e regiões. Não há atualização automática. O trabalho pesado é feito em Rust, com limites para não estourar a cota da API da AWS, a CPU ou a memória.
2. **Login pelo perfil mestre.** Os acessos são, em geral, por assume role a partir de um perfil mestre. O botão "Entrar na AWS" roda `aws login --profile <mestre>`, e os perfis que dependem dele passam a funcionar.
3. **Erros explícitos na tela,** por perfil e por região, sem falhar em silêncio.
4. **Feature só local.** Tudo depende do AWS CLI e dos perfis desta máquina. Nada da AWS vai para o cofre sincronizado nem para o servidor.
5. **Liberada para agentes de IA (MCP),** como o SSH: o agente usa a sessão, e o usuário vê no terminal tudo o que ele executa.
6. **Tudo fica no menu AWS.**

## Credenciais
- O Vessel **não lê nem guarda** chaves da AWS, e não lê `~/.aws/credentials`.
- Os perfis vêm de `~/.aws/config`, e o Vessel monta o grafo de quem é assume-role de quem (`source_profile`, `role_arn`) para mostrar o perfil mestre e os perfis que dependem dele.
- Para o inventário, as credenciais de cada perfil vêm de `aws configure export-credentials --profile X`. Assim o próprio CLI resolve SSO, `aws login`, assume role e MFA. As credenciais temporárias ficam **só na memória**, durante o mapeamento, e são descartadas ao fim.
- As sessões (terminal) rodam pelo AWS CLI com o Session Manager Plugin.

## Inventário em Rust
- Usa os crates `aws-sdk-ec2` e `aws-sdk-ssm` com as credenciais temporárias acima.
- Por perfil:
  1. `ec2:DescribeRegions` (só as regiões habilitadas);
  2. em cada região, `ssm:DescribeInstanceInformation` e `ec2:DescribeInstances`, com paginação.
- **Contra a cota da API:**
  - um limite global de consultas simultâneas (padrão 4) e no máximo uma por região de cada conta;
  - o modo de retry adaptativo do SDK, com backoff exponencial quando a AWS responde `Throttling`.
- **Contra o consumo de CPU e memória:**
  - um único runtime assíncrono, sem uma thread por consulta;
  - os resultados vão para a tela aos poucos, conforme cada região termina, e são guardados em formato enxuto (só os campos usados);
  - o botão **Cancelar** interrompe o mapeamento.
- **Na tela:** uma barra de progresso ("conta 3/8 · região 12/17") e o tempo decorrido.
- **Opção por perfil:** escolher as regiões, para quem usa só algumas.

## Cache
- O resultado do último mapeamento fica guardado neste dispositivo, cifrado, com a data. Não é sincronizado.
- A tela mostra "Mapeado há 3 dias" e o botão para mapear de novo.

## Tela (menu AWS)
**Topo**
- Perfil mestre, estado da sessão (ativa ou expirada), botão **Entrar na AWS** e botão **Mapear tudo**.

**Árvore**
- Perfil (conta e papel) → região → instâncias.

**Cada instância mostra**
- nome (tag `Name`), ID, estado (running ou stopped), status no SSM (online, offline ou não gerenciada), sistema operacional e IP privado.

**Ações**
- **Abrir terminal (SSM)**;
- **SSH via SSM**, com chave do cofre, verificação da chave do host e port forwarding;
- **Port forward via SSM**;
- **Copiar ID**.

**Busca e filtros**
- Busca por nome ou ID.
- Filtros: "só online no SSM", "por perfil", "por região".

**Favoritos**
- Fixa a instância no topo da tela, e ela passa a valer como destino de snippets (`send-command` ou sessão).

## Erros na tela
Cada perfil ou região que falha mostra o erro no próprio nó da árvore, sem interromper os outros:

| Situação | O que a tela mostra |
|---|---|
| AWS CLI ou Session Manager Plugin ausente | Aviso no topo, com link de instalação e o comando de verificação |
| Sessão expirada ou ausente | "Sessão expirada", com o botão **Entrar na AWS** |
| Assume role negado | "Sem permissão para assumir <role_arn>" |
| Ação sem permissão (`AccessDenied`) | A permissão que falta (ex.: `ssm:DescribeInstanceInformation`) |
| Throttling depois de esgotar os retries | "Limite da API atingido, tente de novo depois", só naquela região |
| Região desabilitada ou rede fora | O erro original resumido, com um botão "Tentar de novo" no nó |
| Instância não gerenciada pelo SSM | Ações desativadas, com o motivo |

## Agentes de IA (MCP)
- Ferramentas novas: `aws_list_instances` (do cache, sem chamar a AWS) e `aws_open_session` (abre uma sessão SSM numa aba de terminal visível).
- Tudo o que o agente executa aparece na aba, como no SSH. O usuário pode fechar a aba a qualquer momento.
- O agente não pode disparar o mapeamento nem o login.
- Uma configuração no menu AWS liga ou desliga o acesso dos agentes; desligado por padrão.

## Segurança
- Nenhuma credencial da AWS é gravada. As temporárias vivem só na memória durante o mapeamento.
- O inventário revela a infraestrutura (IDs, IPs, nomes), por isso fica cifrado e só neste dispositivo.
- Os processos externos (`aws`, `session-manager-plugin`) recebem os argumentos como lista, sem shell, e os nomes de perfil são validados contra os perfis existentes.

## Testes
- **Unitários:**
  - parser de `~/.aws/config` e grafo de assume role;
  - classificação dos erros da AWS para as mensagens da tela;
  - o limitador de consultas simultâneas.
- **Integração:** um servidor falso que imita as respostas de EC2 e SSM, cobrindo paginação, throttling com retry, `AccessDenied`, cancelamento e consumo de memória num inventário grande (ex.: 5.000 instâncias).
- **Manual:** uma conta real com perfil mestre e assume role, instâncias online e offline no SSM, e uma sessão expirada.

## Fora do escopo
- Atualização automática do inventário.
- Sincronização entre dispositivos.
- Outras nuvens.
- Gerenciar instâncias (ligar, parar, criar).

## Favoritos como destino de snippets
- Decidido: o destino fica só neste dispositivo. No cofre o snippet continua "sem destino", e a instância escolhida fica cifrada no computador (decisão 4). Nos outros computadores, ele aparece para colar num terminal.
- Roda com `send-command` (Run Command); não há modo "sessão".

## Ainda não feito
- Teste manual com uma conta real (a lista está em `docs/TESTING.md`, §3.6).
