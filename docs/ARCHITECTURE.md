# Arquitetura e protocolo

Este documento descreve o Vessel como ele está implementado. Para operar um servidor, veja `docs/OPERATING.md`; para testar, `docs/TESTING.md`.

## 1. Visão geral

| Componente | O que é | Onde roda |
|---|---|---|
| **App** (`apps/desktop`) | Terminal e cliente SSH em Rust (Tauri 2) com interface em SolidJS. Faz toda a criptografia. | No computador de cada pessoa |
| **Agente** (`apps/agent`) | Servidor de sincronização. Guarda registros cifrados, chaves embrulhadas e metadados mínimos. | Num contêiner que você hospeda |
| **Ponte MCP** (`apps/mcp-bridge`) | Liga assistentes de IA que falam stdio ao servidor MCP do app. | No computador de cada pessoa |

O app pode ser usado sem agente (modo local, §11). Com um agente, a pessoa informa **endereço do servidor, e-mail e senha**. Todo dado de usuário (hosts, grupos, identidades, chaves SSH, snippets, perfis de terminal, redirecionamentos, configurações) é cifrado no dispositivo antes de sair dele. Os terminais continuam sendo processos locais de cada máquina: o que sincroniza é a configuração, não as sessões.

Objetivos:

- o mesmo ambiente em todos os dispositivos, com sincronização quase em tempo real;
- cofres compartilhados com papéis;
- um agente comprometido não revela conteúdo, nem endereços, nomes ou portas;
- hospedagem simples: um binário com SQLite atrás de um proxy com HTTPS.

Fora do escopo: sincronizar sessões de terminal ou histórico de saída, gravação de sessões, SFTP, federação entre servidores e clientes para celular.

## 2. Modelo de ameaças

| Adversário | Pode | Não deve conseguir |
|---|---|---|
| Agente comprometido ou operador curioso | Ler o banco, ver o tráfego depois do TLS, alterar ou reter respostas | Ler conteúdo; obter a senha; trocar, falsificar ou devolver versões antigas de registros sem ser recusado; entregar a chave de um cofre a quem não é membro |
| Atacante de rede | Interceptar tráfego | Nada além do que o TLS protege |
| Membro removido de um cofre | Ter guardado a chave antiga | Ler o que for escrito depois da remoção |
| Outro membro de um cofre compartilhado | Editar itens do cofre | Fazer o app usar credenciais ou confirmações de confiança de outro cofre sem avisar (§9.3) |
| Dispositivo roubado e bloqueado | Copiar o disco | Decifrar o cofre local sem a senha |
| Assistente de IA conectado por MCP | Chamar as ferramentas liberadas | Ler ou gravar credenciais; aceitar chaves de servidor; executar comandos sem a permissão de digitar (§10) |

O que o agente **consegue** fazer e o desenho não impede:

- negar serviço, reter alterações ou parar de listar um cofre (o app então remove a cópia local dele, como faria numa remoção legítima);
- encerrar a sessão de um dispositivo, o que faz o app apagar os dados locais daquela conta;
- ver metadados: e-mails, nomes de dispositivos, quem é membro de qual cofre e com qual papel, datas, quantidade e tamanho aproximado dos registros.

Um dispositivo desbloqueado com malware também está fora do escopo.

## 3. Criptografia

Tudo fica em `crates/vessel-crypto`. O agente usa desse crate só o lado servidor do OPAQUE e as validações de formato e assinatura; nunca tem uma chave que decifre dados.

### 3.1 Primitivas

| Uso | Algoritmo |
|---|---|
| Registros e dados locais | XChaCha20-Poly1305, nonce aleatório de 24 bytes |
| Entrega da chave do cofre a um membro | HPKE (RFC 9180): X25519, HKDF-SHA256, ChaCha20-Poly1305 |
| Assinaturas (histórico de membros, concessões, dispositivos) | Ed25519 |
| Login | OPAQUE (RFC 9807) com Argon2id |
| Derivação de chaves | HKDF-SHA256 e Argon2id |

Argon2id usa 64 MiB, 3 passadas e 1 fio. O app recusa parâmetros mais fracos que esses vindos do servidor, e nunca aceita menos que 19 MiB e 2 passadas em nenhum contexto.

### 3.2 Hierarquia de chaves

```
senha ── OPAQUE (Argon2id no cliente) ──► chave mestra ──┐
chave de recuperação (32 bytes, mostrada uma vez) ───────┤ embrulham
senha ── Argon2id com sal local ──► chave de desbloqueio ┘
                                                         ▼
                                              chave da conta (32 bytes)
                                                         │ cifra
                              ┌──────────────────────────┴───────────┐
                   chave privada X25519                    chave privada Ed25519
                (recebe chaves de cofre)              (assina histórico e concessões)

chave do cofre (32 bytes, uma por cofre e por versão)
   │ entregue a cada membro por HPKE, assinada por quem gerencia o cofre
   ▼
registros do cofre
```

- O cofre pessoal é um cofre com um único membro; não tem caminho especial.
- Trocar a senha reembrulha a chave da conta. Nenhum dado é recifrado.
- A chave de desbloqueio existe para o app abrir sem rede: a chave mestra depende do OPAQUE, que precisa do servidor.

### 3.3 Envelope de registro

```
versão(1) ‖ algoritmo(1) ‖ nonce(24) ‖ texto cifrado ‖ tag(16)
```

Dados autenticados junto (não transmitidos; cada lado os recalcula):

```
formato 1: "vessel/v1/record" ‖ versão ‖ algoritmo ‖ id do cofre ‖ id do registro ‖ versão da chave
formato 2: o mesmo ‖ versão do registro (u64) ‖ marca de exclusão (1 byte)
```

- O texto claro é JSON (`{"type": ..., "data": {...}}`), preenchido até um múltiplo de 256 bytes para esconder o tamanho exato. O tipo do item fica dentro do texto cifrado.
- O cabeçalho é autenticado: trocar a versão ou o algoritmo faz a decifragem falhar.
- O **formato 2** é o que todo app escreve. Ele amarra a versão do registro e a marca de exclusão, então o servidor não consegue devolver uma cópia antiga com um número novo nem inventar ou desfazer uma exclusão.
- O **formato 1** só é usado para o nome do cofre (que não tem versão de registro) e ainda é lido em registros antigos. O app regrava esses registros no formato 2 e, quando um cofre não tem mais nenhum, passa a recusar o formato 1 nele.

### 3.4 Concessão da chave do cofre

Uma concessão leva a chave de um cofre, numa versão, a um membro. A chave é selada com HPKE para a chave X25519 do destinatário; o `info` do HPKE amarra cofre, versão da chave e destinatário; e o conjunto é assinado com a chave Ed25519 de quem concede. O app só abre concessões assinadas por quem o histórico verificado (§5.3) mostra como gerente do cofre, e nunca troca uma chave já instalada por outra diferente na mesma versão.

## 4. Contas, dispositivos e sessões

- **Cadastro.** O app roda o registro OPAQUE, gera a chave da conta, a chave de recuperação e os pares X25519 e Ed25519, e envia: o registro OPAQUE, a chave da conta embrulhada (pela chave mestra e pela de recuperação), as chaves públicas, as privadas cifradas e o cofre pessoal já com a concessão para si. A primeira conta de um servidor vira administradora e precisa do código de instalação mostrado no log do agente.
- **Login.** OPAQUE em duas idas e voltas. A identidade do servidor (o host do endereço público) entra na transcrição, então a senha de um servidor não serve em outro. Para e-mails que não existem o agente responde com uma credencial falsa determinística, e a resposta fica igual à de uma conta real.
- **Sessão.** O agente emite um token de acesso (15 minutos) e um de renovação (30 dias), guardados como SHA-256. A renovação exige uma assinatura da chave Ed25519 do dispositivo sobre `dispositivo ‖ hora ‖ token`, com tolerância de 5 minutos, e troca os dois tokens. Reapresentar um token de renovação já trocado encerra a sessão.
- **Desbloqueio local.** O app abre bloqueado. A senha deriva a chave de desbloqueio e abre a chave da conta sem rede. A chave do dispositivo e o token de renovação ficam no SQLite local cifrados com a chave da conta.
- **Recuperação.** A chave de recuperação abre a chave da conta. Para o servidor aceitar um novo registro OPAQUE sem a senha, o app apresenta um token derivado da chave de recuperação; o agente guarda só o SHA-256 dele. Toda recuperação emite uma chave de recuperação nova e encerra as sessões dos outros dispositivos.
- **Troca de senha e exclusão de conta.** Exigem a senha atual (um login OPAQUE na mesma operação), não só uma sessão aberta.
- **Dispositivos.** Dá para listar e revogar. Revogar encerra as sessões na hora; não recifra dados, porque o dispositivo já tinha as chaves. Para isso existe a rotação (§5.5).

## 5. Cofres e times

### 5.1 Papéis

| Papel | Ler | Editar itens | Gerenciar membros | Trocar a chave | Excluir o cofre |
|---|---|---|---|---|---|
| leitor | ✓ | | | | |
| editor | ✓ | ✓ | | | |
| admin | ✓ | ✓ | ✓ (até editor) | ✓ | |
| dono | ✓ | ✓ | ✓ | ✓ | ✓ |

O agente aplica as permissões sobre quem pode escrever. A criptografia garante o sigilo: sem concessão não há chave.

### 5.2 Convite e aprovação

1. Um gerente cria um convite: um link com um token de uso único, válido por 7 dias, opcionalmente restrito a um e-mail.
2. O convidado aceita no app e entra na lista de pendentes do cofre.
3. O gerente pede ao convidado, por outro canal, a **impressão digital** dele e a digita no app.
4. O app confere a impressão com as chaves que vão receber a chave do cofre, assina o evento de entrada e envia a concessão.

A impressão digital são 25 dígitos derivados de `SHA-256(chave X25519 ‖ chave Ed25519)`. O agente nunca concede acesso sozinho: sem um gerente aprovando não existe concessão.

### 5.3 Histórico assinado de membros

Cada mudança (criar o cofre, adicionar, mudar papel, remover ou sair) é um evento assinado por quem a fez e encadeado ao anterior por hash. O app revalida a cadeia inteira e nunca confia na lista de membros do servidor. O agente usa a mesma função para recusar eventos inválidos antes de guardá-los.

Regras de papel verificadas na cadeia: só o dono adiciona ou promove admins, e uma entrada não pode ter papel maior que o do convite aceito.

Cada app guarda uma **âncora** do que já verificou: o tamanho da cadeia, o hash da ponta e o criador. Uma cadeia nova só é aceita se estender a anterior, então o servidor não consegue desfazer uma remoção servindo só o começo, nem trocar o histórico por outro.

### 5.4 Chaves de outras pessoas

A primeira chave pública que o app vê de cada pessoa fica fixada (confiança no primeiro uso), junto com o e-mail. Uma chave diferente para a mesma pessoa, ou outra conta com um e-mail já fixado, interrompe a sincronização com um alerta. As chaves fixadas e as âncoras sincronizam pelo cofre pessoal, num registro cifrado, para que um dispositivo novo parta do que os outros já verificaram.

### 5.5 Remoção e rotação

Quando alguém sai ou é removido:

1. O agente bloqueia o acesso na hora e marca o cofre como pendente de rotação.
2. O app de um gerente gera uma chave nova (versão seguinte), cria concessões para os membros restantes e recifra todos os registros.
3. Durante a rotação o cofre aceita leituras nas duas versões e escritas só na nova.
4. Com tudo recifrado, as concessões antigas são apagadas.

Isso vale mesmo se o app fechar no meio: a marca só sai quando uma rotação nova começa, e a sincronização de qualquer gerente a faz. Uma rotação que estava em andamento é concluída e logo substituída, porque a chave dela foi entregue a quem era membro quando começou. Rebaixar um gerente também força a rotação. O app nunca escreve com uma versão de chave menor que a mais nova que já verificou.

## 6. Itens sincronizados

`crates/vessel-model` define os tipos: `host`, `group`, `identity`, `ssh_key`, `known_host`, `snippet`, `port_forward`, `terminal_profile`, `workspace`, `settings` e `trust` (o registro de confiança do §5.4). Os ids são UUIDv7 gerados no cliente. As referências entre itens são ids dentro do texto cifrado. Campos que um app mais novo acrescentar são preservados pelos mais antigos.

Um perfil de terminal que aponta para um shell que não existe naquele computador aparece como indisponível, em vez de falhar.

## 7. Sincronização

### 7.1 O registro como o agente o vê

```json
{ "id": "…", "version": 7, "seq": 18234, "key_version": 2, "deleted": false,
  "blob": "<envelope>", "updated_at": 1794700000, "updated_by_device": "…" }
```

`version` cresce a cada escrita aceita daquele registro; `seq` é a posição no log do cofre e define a ordem.

### 7.2 Pull e push

```
GET  /v1/vaults/{id}/changes?since=<seq>     → { changes, next_seq, has_more }
POST /v1/vaults/{id}/changes  { ops: [{ id, base_version, key_version, deleted, blob }] }   (até 500)
     → { results: [ ok | conflict (com o registro atual) | rejected (com o motivo) ] }
```

O agente aceita uma escrita só se `base_version` for a versão atual e a versão da chave for a atual do cofre. Cada operação é atômica; o lote não.

### 7.3 O que o app confere em tudo o que vem do servidor

- Um registro só substitui a cópia local depois de decifrado e autenticado com a chave do cofre.
- Um registro sob uma versão de chave que o dispositivo não tem nunca substitui uma cópia local, e uma exclusão nessa situação é recusada. Se a causa for uma rotação em andamento, o app busca as concessões e tenta de novo; se continuar sem a chave, o pull para naquele ponto e recomeça na próxima sincronização.
- Um envelope no formato 1 nunca substitui um registro que o dispositivo tem no formato 2.
- A cópia local nunca volta para uma versão menor.
- A resposta de um push tem que falar da operação enviada, com a versão para a qual o envelope foi selado. Caso contrário a edição continua na fila.

### 7.4 Conflitos

Com as três versões em mãos (base, local e remota), o app mescla campo a campo: o que mudou só de um lado é aplicado; se o mesmo campo mudou dos dois, vence a edição local e a outra vai para a lista de conflitos; uma edição vence uma exclusão.

### 7.5 Tempo real e uso sem rede

`GET /v1/events` é um WebSocket autenticado que só avisa que algo mudou; os dados sempre chegam pelo pull. Sem ele, o app sincroniza a cada 60 segundos. Toda edição vai primeiro para o SQLite local e para uma fila cifrada, e sobe na próxima sincronização.

## 8. Agente

Rust com `axum` e `tokio`, um binário, SQLite em modo WAL com uma conexão de escrita e quatro de leitura.

### 8.1 API

| Rota | Para quê |
|---|---|
| `GET /v1/server/info` | Versão, identidade OPAQUE e modo de cadastro |
| `POST /v1/auth/register/{start,finish}` | Cadastro |
| `POST /v1/auth/login/{start,finish}` | Login |
| `POST /v1/auth/refresh` · `/logout` | Renovar e encerrar a sessão |
| `POST /v1/auth/recovery/{start,finish}` | Recuperação com a chave de recuperação |
| `POST /v1/account/password/{start,finish}` · `/delete/{start,finish}` | Trocar a senha; excluir a conta |
| `GET /v1/account` · `GET /v1/devices` · `DELETE /v1/devices/{id}` | Conta e dispositivos |
| `GET, POST /v1/vaults` · `GET, DELETE /v1/vaults/{id}` · `PUT …/meta` | Cofres |
| `GET, POST /v1/vaults/{id}/membership` | Histórico de membros e pendentes |
| `GET /v1/vaults/{id}/grants` | Concessões de quem chama |
| `POST /v1/vaults/{id}/rotate/{start,finish}` | Rotação de chave |
| `POST /v1/vaults/{id}/invites` · `POST /v1/invites/{token}/accept` | Convites |
| `GET, POST /v1/vaults/{id}/changes` | Pull e push |
| `GET /v1/events` | WebSocket |
| `POST /v1/admin/invites` · `GET /v1/admin/users` · `PATCH, DELETE /v1/admin/users/{id}` · `GET /v1/admin/stats` | Administração do servidor |

Não existe diretório de usuários: as chaves dos colegas chegam com os cofres compartilhados, então ninguém consegue enumerar as contas de um servidor. Quem não é membro de um cofre recebe 404, não 403.

### 8.2 Limites

- Autenticação: 30 tentativas por minuto por endereço (ou /64 de IPv6) e 20 por conta.
- Escrita: 240 envios por minuto por conta; lotes de até 500 registros de até 256 KiB.
- Cota de espaço e de cofres por conta, configuráveis.
- 50 dispositivos por conta, 16 sockets por usuário, 100 convites abertos e 10.000 eventos de membros por cofre.
- Toda requisição tem prazo de 60 segundos; sessões e convites vencidos são apagados de hora em hora.

### 8.3 Implantação

O TLS fica num proxy reverso (o `deploy/` traz o Caddy configurado). O agente escuta em HTTP e não deve ser exposto diretamente. O backup sai sempre cifrado (`vessel-agent backup`), porque o banco guarda o segredo OPAQUE do servidor ao lado dos arquivos de senha: com os dois, quem tivesse um backup em claro poderia testar senhas offline. Detalhes em `docs/OPERATING.md`.

## 9. App

### 9.1 Base

Tauri 2 (WebView2 no Windows) com backend em Rust; interface em SolidJS e TypeScript; terminal com xterm.js. O armazenamento local é SQLite, com os registros guardados ainda cifrados, exatamente como no servidor, e decifrados só em memória depois do desbloqueio. A política de conteúdo da janela só permite recursos do próprio app.

### 9.2 Terminais locais

`portable-pty` (ConPTY no Windows). O app detecta PowerShell 7, Windows PowerShell, Prompt de Comando, distribuições WSL e Git Bash. A saída vai do Rust para a interface por um canal binário e fica num buffer circular de 512 KiB por sessão.

Um perfil guardado num cofre compartilhado que rode um programa próprio, argumentos ou variáveis de ambiente pede confirmação antes de abrir, mostrando o comando completo.

### 9.3 SSH

`russh`, com autenticação por chave (com ou sem passphrase) e por senha, jump hosts encadeados e redirecionamentos local, remoto e SOCKS.

- **Chave do servidor.** Na primeira conexão o app mostra a impressão digital e só segue com a confirmação da pessoa. A chave confirmada vira um `known_host` no cofre pessoal. Uma chave diferente da confirmada é sempre recusada, com alerta.
- **De onde vêm as referências.** Uma referência feita por um item (credencial, jump host, snippet de conexão) é resolvida no cofre do próprio item ou no cofre pessoal. As confirmações de chave de servidor vêm só do cofre pessoal. Assim, outro membro de um cofre compartilhado não consegue redirecionar as referências de alguém para registros que ele controla.
- **Credencial de outro cofre.** Quando um host de um cofre usa uma credencial guardada em outro, a primeira conexão avisa qual credencial será enviada.
- **Redirecionamentos.** Uma regra que escuta fora de `127.0.0.1` fica aberta para a rede; o app avisa antes, e só a pessoa pode iniciá-la.

As chaves privadas nunca vão para o disco em claro: são decifradas em memória no momento da conexão.

### 9.4 Snippets

Um snippet tem um destino: nenhum (é colado num terminal), um shell deste computador ou um ou mais hosts SSH. Com destino, ele roda com um clique, pela paleta, por agendamento ou por um assistente de IA liberado.

- **Local.** O shell roda sem janela, na pasta do usuário, com tempo máximo; ao estourar, o processo e tudo o que ele abriu são encerrados. PowerShell e Prompt de Comando recebem o script num arquivo temporário apagado ao fim; WSL e Git Bash, pela entrada padrão.
- **SSH.** Um `exec` por host, em paralelo, pela mesma conexão do terminal (credenciais, jump hosts, referências resolvidas como em §9.3). Só conecta a servidores com chave já confirmada: uma chave desconhecida ou diferente recusa a execução, e nada aqui confirma chave nova.
- **Quem pode rodar.** Por agendamento ou por um assistente, só snippets do cofre pessoal: num cofre compartilhado, outro membro poderia mudar o script que rodaria no computador de alguém. Rodar à mão um snippet de cofre compartilhado, ou marcado "pedir confirmação", mostra o script antes.
- **Histórico.** Cada execução guarda origem (manual, agendada, agente), destino, código de saída e saída (até 64 KB por execução). Fica no próprio dispositivo, cifrado com a chave da conta, e não é sincronizado.
- **Agendamento.** Expressões cron de cinco campos, no horário local, avaliadas a cada 15 s com o app aberto (fechar a janela deixa o Vessel na bandeja). Só com o cofre desbloqueado; um horário com o cofre bloqueado é registrado como pulado, e um que passou com o app fechado ou o computador suspenso, como perdido. Nenhum dos dois roda depois. O mesmo snippet nunca roda duas vezes ao mesmo tempo, e um botão (e o menu da bandeja) pausa todos os agendamentos. Uma falha agendada gera uma notificação do Windows.
- **Bloqueio automático.** Por padrão continua valendo e para os agendamentos. A pessoa pode escolher não bloquear por inatividade enquanto houver agendamentos ativos, com aviso do risco.

### 9.5 AWS

A tela AWS mapeia as instâncias EC2 das contas que a pessoa acessa pelo AWS CLI e abre sessões nelas pelo Session Manager. É só local: nada da AWS vai para o cofre sincronizado nem para o servidor.

- **Perfis.** O app lê `~/.aws/config` (ou `AWS_CONFIG_FILE`), nunca `credentials`, e monta o grafo de assume role (`source_profile`, `role_arn`): cada perfil aponta para o perfil mestre no fim da sua cadeia, e cadeias quebradas ou circulares aparecem como erro no perfil. "Entrar na AWS" abre, numa aba de terminal, `aws login` (ou `aws sso login`, para perfis SSO) do perfil mestre.
- **Credenciais.** Abrir o AWS CLI custa quase um segundo, então, para mapear centenas de perfis, ele só é chamado para o que precisa resolver sozinho: o perfil mestre (SSO, `aws login`, chaves, `credential_process`) e papéis com MFA ou `credential_source`, até 6 processos por vez, com `aws configure export-credentials` e a região do perfil (ou a do mestre). Um perfil de assume role simples (`role_arn` + `source_profile`, com `external_id`, `role_session_name` e `duration_seconds` opcionais) é assumido pelo próprio app, numa chamada `sts:AssumeRole` com as credenciais da origem. Cada perfil é resolvido uma vez por mapeamento. As credenciais ficam só na memória durante o mapeamento e nunca são gravadas nem mostradas.
- **Mapeamento.** Sob demanda ("Mapear tudo", com aviso), nunca automático. Por perfil: `ec2:DescribeRegions` (ou as regiões escolhidas para ele) e, em cada região, `ssm:DescribeInstanceInformation` e `ec2:DescribeInstances`, paginados. Cada conta é mapeada uma vez: perfis que chegam à mesma conta viram um só trabalho, pelo perfil escolhido para o cliente, e os outros servem de reserva se ele não conseguir entrar. Tudo roda num só runtime assíncrono, com um cliente HTTPS compartilhado, no máximo 64 consultas ao mesmo tempo e uma por conta, região e serviço (o limite da AWS é por conta e região), com Session Manager e EC2 de cada região em paralelo, e o retry adaptativo do SDK contra `Throttling`. Cada região vai para a tela quando termina, num formato enxuto (nome, ID, estado, status no SSM, sistema e IP privado); nós registrados no Session Manager que não são EC2 também entram. Cancelar interrompe tudo e guarda o que já chegou, marcado como incompleto.
- **Erros.** Cada perfil e região guarda o próprio erro, sem parar os outros: sessão expirada, assume role negado, permissão que falta (com o nome da ação), limite da API, região desabilitada, rede. A tela os reúne num aviso no topo, cada um com "Tentar de novo".
- **Clientes.** A tela agrupa por conta AWS: perfis que chegam à mesma conta viram um cliente só, e cada instância aparece uma vez. Contas e regiões sem instâncias não aparecem. O nome de cada cliente e o perfil usado nas sessões ficam nas preferências locais.
- **Cache.** O último mapeamento, os favoritos, as regiões escolhidas e as chaves SSH das instâncias ficam no dispositivo, cifrados com a chave da conta, e não sincronizam.
- **Sessões.** Terminal (`aws ssm start-session`), redirecionamento de porta (`AWS-StartPortForwardingSession`, ou `…ToRemoteHost` para um host que a instância alcança) e SSH pelo túnel `AWS-StartSSHSession`, com credencial do cofre e a chave do servidor confirmada na primeira vez. Os processos recebem os argumentos como lista, sem shell; perfis, regiões e IDs são conferidos, e uma sessão só vai a uma instância que o último mapeamento encontrou naquele perfil e região e que está no Session Manager.
- **Snippets.** Um snippet pode ter como destino uma instância favorita. No cofre ele fica "sem destino": a instância (perfil, região, ID, se é Windows) fica no dispositivo, cifrada, e vale só ali; nos outros computadores o snippet só pode ser colado num terminal. Roda com Run Command (`AWS-RunShellScript`, ou `AWS-RunPowerShellScript` em Windows), com as credenciais temporárias do perfil e o tempo máximo do snippet; ao estourar, o comando é cancelado na instância. Saída e código de saída vão para o histórico, como nos outros destinos. Agendamentos valem neste computador; um assistente só roda esse snippet se a opção de agentes da tela AWS estiver ligada.
- **Ferramentas.** O app procura o AWS CLI e o Session Manager Plugin no PATH e nas pastas padrão de instalação, e mostra como instalar o que faltar. Ele respeita `AWS_ENDPOINT_URL`, como o CLI.

### 9.6 Atualizações

O app consulta o `latest.json` do último release no GitHub ao abrir e a cada 10 minutos. Uma versão nova só é instalada depois que a pessoa confirma, e só se for exatamente a versão que ela viu. O instalador baixado tem de estar assinado com a chave do atualizador do projeto (Ed25519, formato minisign), cuja parte pública vem embutida no app; a assinatura cobre também o número da versão, então um instalador antigo e legítimo anunciado como versão nova é recusado, assim como qualquer versão que não seja mais nova que a instalada. Essa chave é independente do certificado de assinatura do Windows.

## 10. Assistentes de IA (MCP)

O app embute um servidor MCP para que assistentes operem o Vessel. Ele fica no app, e não no agente, porque só ali os dados estão decifrados.

- Escuta só em `127.0.0.1` (porta padrão 47011), exige um token de 256 bits comparado em tempo constante e recusa requisições com `Origin` que não seja local.
- Só funciona com o app desbloqueado. Bloqueado, informa apenas isso.
- Cada chamada aparece no app, e as sessões abertas por um assistente são marcadas.
- Duas permissões, **desligadas por padrão**: editar itens, e digitar ou executar comandos nos terminais.

O que um assistente nunca pode fazer:

- **Ler credenciais.** Senhas, chaves privadas e passphrases saem de toda resposta, em qualquer profundidade, trocadas por `has_password` e semelhantes.
- **Gravar credenciais.** Qualquer criação ou edição com esses campos é recusada, e uma edição preserva os segredos que o item já tinha.
- **Alterar identidades, chaves SSH e hosts conhecidos.** São só leitura.
- **Aceitar a chave de um servidor.** Só conecta a servidores que a pessoa já confirmou no app. Sem isso, um assistente poderia apontar um host com senha salva para um servidor dele.
- **Abrir uma porta para a rede**, mexer em conta, dispositivos, convites, membros ou rotação de chave, ou mudar o tempo de bloqueio automático.

**Snippets liberados.** O assistente lista (`list_runnable_snippets`) e roda (`execute_snippet`) só os snippets do cofre pessoal em que a pessoa marcou a permissão para agentes, e nunca um que pede confirmação. Roda como está, no destino salvo, mesmo sem a permissão de digitar, e a execução vai para o histórico com a origem "agente". Nenhum assistente define destino, agendamento, confirmação, tempo máximo ou essa permissão, nem troca o script de um snippet que roda sozinho ou o endereço e as credenciais de um host em que ele roda.

**AWS.** Com a opção ligada na tela AWS (desligada por padrão), o assistente lista as instâncias do último mapeamento (`aws_list_instances`, do cache, sem chamar a AWS) e abre uma sessão do Session Manager numa delas (`aws_open_session`), numa aba marcada como aberta por agente. Ele não mapeia nem entra na AWS.

Sem a permissão de digitar, o assistente ainda abre e lê terminais, mas nada que execute comandos por conta própria: só shells embutidos sem argumentos, nenhum host com snippet de conexão. Também não pode **salvar** algo que a pessoa executaria depois sem reler: snippets, perfis com programa, argumentos ou ambiente próprios, e snippet de conexão num host.

Limitação: com a permissão de digitar, um assistente pode rodar comandos que leiam credenciais no servidor remoto (por exemplo `cat ~/.ssh/id_rsa`). A proteção cobre o que está no cofre, não o que o terminal remoto mostra.

## 11. Modo local e migrações

Sem agente, a conta usa a mesma hierarquia de chaves e os registros são cifrados no disco do mesmo jeito. A senha abre a chave da conta por Argon2id e a chave de recuperação redefine a senha no próprio dispositivo. Tudo o que depende de servidor responde que não está disponível.

| Caminho | O que acontece | Senha e chave de recuperação |
|---|---|---|
| Local → conta nova | A conta é registrada com as mesmas chaves; os itens vão para o cofre pessoal | A senha escolhida vira a da conta, e é emitida uma chave de recuperação nova |
| Local → conta existente | Login normal; os itens locais são somados ao cofre pessoal | Passam a valer as da conta |
| Servidor → local | O dispositivo fica com uma cópia de tudo o que enxerga, num cofre pessoal local | Continuam as mesmas |

Para não perder dados: o app grava um backup cifrado do banco antes de migrar; os itens passam por uma fila de importação cifrada que continua de onde parou se o app fechar; itens idênticos a um já existente não são duplicados, e as referências a eles passam a apontar para a cópia que ficou. A conta no servidor nunca é apagada por "parar de sincronizar".

## 12. Limitações conhecidas

- A confiança nas chaves de outras pessoas começa no primeiro contato; a conferência da impressão digital na aprovação é o que fecha essa janela.
- O servidor vê quantos registros cada cofre tem e o tamanho deles arredondado para 256 bytes.
- Registros antigos no formato 1 não protegem a versão nem a marca de exclusão até serem regravados, o que acontece sozinho na primeira sincronização de quem pode escrever no cofre.
- O instalador do Windows ainda não tem certificado de assinatura de código (as atualizações são assinadas pela chave do atualizador, §9.6). macOS e Linux não têm instalador.
- Snippets agendados só rodam com o app aberto e o cofre desbloqueado; não há agendamento com o app fechado nem crontab no servidor.
- A tela AWS depende do AWS CLI deste computador, e as sessões, do Session Manager Plugin. O destino AWS de um snippet vale só no computador em que foi escolhido.
- Não há verificação de e-mail no cadastro. Num servidor com cadastro aberto, a tela de cadastro revela se um e-mail já tem conta.
