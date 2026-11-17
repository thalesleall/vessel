# Roteiro de testes manuais

Este roteiro cobre o que só dá para testar com o app aberto. O resto já tem teste automático:

| Área | Como é testado |
|---|---|
| Criptografia, protocolo e merge | `cargo test --workspace` |
| Agente ↔ clientes: 3 PCs, conflitos, times, recuperação, troca de senha, WebSocket, convites, servidor hostil | `cargo test -p vessel-client` |
| SSH real: chave de host desconhecida/alterada, senha, chave com e sem passphrase, jump host, forwards `-L`/`-R`/`-D`, UTF-8 | `cargo test -p vessel-desktop --lib -- --ignored lab_` (precisa do laboratório) |
| Servidor MCP: redação de segredos, recusa de credenciais, edições que executariam comandos | `cargo test -p vessel-desktop --lib mcp` |

## 0. Subir o laboratório

Requer Docker Desktop aberto.

```powershell
powershell -File deploy\dev\lab.ps1 up
```

Isso sobe três contêineres:

| Contêiner | Endereço | Para quê |
|---|---|---|
| agente de sync | `http://localhost:8080` | cadastro aberto |
| `bastion` | SSH em `127.0.0.1:2222` | primeiro salto |
| `target` | SSH `target:22` e HTTP `target:8000` | só alcançável **através** do bastion |

As credenciais de teste ficam em `deploy\dev\.lab\CREDENCIAIS.txt`; para vê-las, rode `lab.ps1 creds`. São descartáveis e o git as ignora. O usuário SSH é sempre `dev`.

Outros comandos do `lab.ps1`:

| Comando | O que faz |
|---|---|
| `status` | mostra os contêineres e o health |
| `restart` | reinicia o agente |
| `rekey` | troca a chave de host do bastion |
| `logs` | acompanha os logs do agente |
| `down` | para tudo, mantendo os dados |
| `reset` | apaga tudo |

Instale o app pelo `target\release\bundle\nsis\Vessel_0.1.0_x64-setup.exe`. Para desenvolver, use `cd apps\desktop; npx tauri dev`.

## 0.5 Modo local e migrações

Para começar do zero, feche o app e apague `%APPDATA%\dev.vessel.desktop`.

1. Abra o app. *Esperado:* a tela "Como você quer usar o Vessel?" com as duas opções ilustradas.
2. Escolha **Só neste computador**, crie a senha e guarde a chave de recuperação.
   - *Esperado:* o app abre com os perfis de terminal detectados, e o rodapé do menu mostra "Sem sincronização".
3. Crie um host, uma chave e um snippet. Bloqueie (`Ctrl+L`) e desbloqueie.
4. Na tela de desbloqueio, use **Esqueci a senha** com a chave de recuperação. *Esperado:* os dados continuam lá.
5. **Configurações → Conectar a um servidor → Criar conta**, com `http://localhost:8080`.
   - *Esperado:* o resumo mostra os itens enviados, o caminho do backup e uma **nova** chave de recuperação.
   - *Esperado:* `lab.ps1 bot ...` ou um segundo PC na mesma conta vê os mesmos itens.
6. **Configurações → Parar de sincronizar.** *Esperado:* todos os itens continuam visíveis, e a senha é a mesma.
7. Repita o passo 5 com **Já tenho conta**. *Esperado:* nada duplicado ("Já existiam: N").
8. Troque o tema em **Configurações → Aparência** (Sistema, Escuro, Claro). *Esperado:* a interface e os terminais mudam juntos.

## 1. Conta, bloqueio e recuperação

1. **Conectar → Criar conta.** Servidor `http://localhost:8080`, seu e-mail e uma senha nova.
   - *Esperado:* aparece a chave de recuperação. Guarde-a para o passo 5.
   - *Esperado:* em Configurações você aparece como administrador do servidor, porque é a primeira conta.
2. **Bloquear e desbloquear** com `Ctrl+L`.
   - *Esperado:* o desbloqueio é imediato e funciona até com o agente parado (`lab.ps1 down`). Religue com `up`.
3. **Bloqueio automático.** Ajuste "Bloquear após (min)" em Configurações e espere.
4. **Trocar a senha** em Configurações, bloquear e desbloquear com a nova.
5. **Recuperação.** Saia em Configurações; na tela Conectar, use a aba **Recuperar** com a chave de recuperação e uma senha nova.
   - *Esperado:* os dados voltam.

## 2. Terminais locais

1. Abra um terminal de cada tipo detectado: PowerShell 7, Windows PowerShell, CMD, cada distro WSL e Git Bash.
2. Em cada um:
   - digite, use as setas e o histórico;
   - rode `exit`; *esperado:* a aba indica que a sessão terminou;
   - redimensione a janela; *esperado:* o texto se reorganiza (no PowerShell, `$Host.UI.RawUI.WindowSize`).
3. No WSL, rode `htop` e `vim`. *Esperado:* tela cheia correta e o mouse funcionando.
4. Copie e cole com `Ctrl+Shift+C` e `Ctrl+Shift+V`, e também com o botão direito.
5. Digite `echo ação ✓ 🚀`. *Esperado:* os acentos e o emoji aparecem certos.
6. Abra 6 ou mais abas e rode `yes` ou um `dir /s` grande em uma delas. *Esperado:* a interface continua fluida.
7. Crie um **perfil de terminal** com fonte, tema e diretório inicial próprios.

## 3. SSH

1. **Chaves → Nova chave.** Use "Importar arquivo" ou cole o conteúdo de `deploy\dev\.lab\id_ed25519_passphrase`, e preencha "Senha da chave (passphrase)" com a passphrase do `CREDENCIAIS.txt`.
2. **Hosts → Novo host "Bastion":** `127.0.0.1`, porta `2222`, usuário `dev`, autenticação por senha.
   - Conectar. *Esperado:* o prompt pede para confirmar a impressão digital; ao aceitar, o terminal mostra "Laboratório Vessel: você está em bastion".
   - Conectar de novo. *Esperado:* não pergunta mais, e a chave aparece em "Hosts conhecidos".
3. **Novo host "Target":** endereço `target`, porta `22`, usuário `dev`, chave importada, jump host "Bastion".
   - *Esperado:* conecta e `hostname` responde `target`.
4. **Identidade e grupo.** Em Chaves, crie uma identidade (usuário `dev` mais a chave). Crie um grupo com essa "Identidade padrão", coloque nele um host sem credenciais e conecte. *Esperado:* ele herda as credenciais do grupo.
5. **Chave alterada.** Rode `lab.ps1 rekey` e conecte no Bastion.
   - *Esperado:* um alerta vermelho de chave alterada, sem conectar.
   - Aceitar a nova chave atualiza o host conhecido.
6. **Snippet.** Crie um snippet (ex.: `uname -a`) sem destino, cole-o num terminal e escolha-o, junto com outro, em "Snippets ao conectar" de um host. *Esperado:* os dois rodam ao conectar, na ordem da lista.
7. **Snippet em hosts.** Dê ao snippet o destino "Hosts SSH" com Bastion e Target e clique em ▶. *Esperado:* os dois rodam em paralelo; o histórico mostra a saída e o código de saída de cada um. Num host cuja chave você nunca confirmou, ou depois de `lab.ps1 rekey`, a execução é recusada sem conectar.

## 3.5 Snippets locais e agendamentos

O que roda e quando tem teste automático: `cargo test -p vessel-desktop --lib` (cron, agendador, execução local com tempo máximo e corte da saída, quem pode rodar o quê) e, com o laboratório, `-- --ignored lab_snippet`.

1. Crie um snippet com destino "Este computador → Prompt de Comando" e script `echo ok` mais `ver` em outra linha. Clique em ▶. *Esperado:* "terminou sem erros"; o histórico mostra as duas saídas.
2. Mude o script para `ping -n 30 127.0.0.1` e o tempo máximo para 3. *Esperado:* falha por tempo esgotado em cerca de 3 s, sem processo `ping` sobrando.
3. Volte para `echo ok` e adicione o agendamento `* * * * *`. *Esperado:* o cartão mostra o próximo horário e, a cada minuto, uma execução "Agendado" entra no histórico.
4. Bloqueie (`Ctrl+L`) por dois minutos e desbloqueie. *Esperado:* um registro "Pulado: o cofre estava bloqueado", e nada roda atrasado; o minuto seguinte roda normalmente.
5. Feche a janela. *Esperado:* o Vessel continua na bandeja e os agendamentos seguem. Abrir o Vessel pelo menu Iniciar mostra a mesma janela, não uma segunda cópia. "Sair", no menu da bandeja, encerra.
6. Feche pelo menu da bandeja, espere dois minutos e abra. *Esperado:* um registro "Perdido: o Vessel estava fechado".
7. Em **Configurações → Segundo plano**, ligue "Iniciar com o Windows" e confira a entrada em Gerenciador de Tarefas → Inicializar. Desligue de novo.
8. Faça o agendamento falhar (`exit 1`). *Esperado:* uma notificação do Windows.

## 3.6 AWS

O leitor de `~/.aws/config`, a classificação dos erros, o limitador e o mapeamento contra um servidor falso de EC2 e SSM (paginação, 5.000 instâncias, throttling com retry, `AccessDenied`, cancelamento) têm teste automático: `cargo test -p vessel-desktop --lib aws`.

Para conferir à mão, com uma conta real: um perfil mestre, perfis de assume role a partir dele, instâncias online e offline no Session Manager.

1. Abra **AWS**. *Esperado:* o perfil mestre aparece com a contagem dos perfis que dependem dele. Sem o AWS CLI ou o Session Manager Plugin, um aviso diz o que instalar e como conferir.
2. Com a sessão expirada, *esperado:* "Sessão expirada". Clique em **Entrar na AWS**, conclua o login na aba que abre e clique no ícone de verificar. *Esperado:* "Sessão ativa".
3. **Mapear tudo** e confirme. *Esperado:* a barra mostra conta e região e o tempo; as regiões aparecem conforme terminam. **Cancelar** no meio para e guarda o que chegou ("incompleto").
4. Num perfil sem permissão de `ssm:DescribeInstanceInformation`, *esperado:* "Falta a permissão ssm:DescribeInstanceInformation" naquela região, e as outras mapeadas. Num papel que o mestre não pode assumir, *esperado:* "Sem permissão para assumir …" no perfil.
5. Busque por nome, ID ou IP, e use "Só online no SSM" e os filtros de perfil e região.
6. Fixe uma instância. *Esperado:* ela aparece em **Favoritas**, no topo.
7. Numa instância online, **Terminal**. *Esperado:* uma aba com o shell da instância. Numa offline ou não gerenciada, as ações ficam desativadas, e passar o mouse diz por quê.
8. **SSH** com uma chave do cofre e o usuário da AMI (`ec2-user`, `ubuntu`). *Esperado:* na primeira vez, pede para confirmar a impressão digital; depois, conecta direto.
9. **Porta** 5432 com o host de um banco que a instância alcança. *Esperado:* uma aba com o redirecionamento; `localhost:5432` responde; fechar a aba encerra.
10. Em **Snippets → Novo snippet**, escolha **Instância AWS** e uma favorita, com o script `uptime`. Clique em ▶. *Esperado:* "terminou sem erros" e a saída da instância no histórico. Em outro computador da mesma conta, o snippet aparece como "Colar no terminal".
11. Ligue "Agentes de IA podem listar estas instâncias…" e peça ao assistente "liste minhas instâncias AWS" e "abra uma sessão na instância X". *Esperado:* funciona, a aba é marcada IA; com a opção desligada, as duas são recusadas.

## 4. Port forwarding (todos pelo host Bastion)

| Tipo | Regra | Como conferir | Esperado |
|---|---|---|---|
| Local | `127.0.0.1:18000 → target:8000` | abra `http://localhost:18000` no navegador | "servico HTTP interno do target OK" |
| SOCKS | porta `18001` | `curl.exe --socks5-hostname 127.0.0.1:18001 http://target:8000` | a mesma mensagem |
| Remoto | porta `19080` do bastion `→ 127.0.0.1:8080` | num terminal SSH do Bastion, `curl localhost:19080/health` | `ok`, vindo do agente que roda no Windows |

Depois, pare cada regra e confirme que a porta fecha.

## 5. Sincronização entre "PCs"

Num segundo usuário do Windows, ou em outro PC apontando para o IP desta máquina, instale o app e **entre** com a mesma conta.

1. Crie um host num PC. *Esperado:* ele aparece no outro em poucos segundos.
2. Edite o mesmo campo nos dois PCs com o agente parado (`lab.ps1 down`) e religue com `up`.
   - *Esperado:* um vence e o outro aparece em "Conflitos", podendo ser restaurado.
3. Abra Configurações → Dispositivos e revogue o outro PC. *Esperado:* ele sai sozinho.

Sem um segundo PC, a conta de teste da seção 6 serve para testar o sync entre pessoas.

## 6. Times (com a conta de teste)

```powershell
powershell -File deploy\dev\lab.ps1 bot signup             # cria bot@vessel.test
powershell -File deploy\dev\lab.ps1 bot fingerprint
```

1. **Cofres → Novo cofre "Time".** Convide como **editor** e copie o link.
2. Rode `lab.ps1 bot accept <link>`.
   - *Esperado:* o app mostra o bot como pendente. Compare a impressão digital com a de `bot fingerprint` e **aprove**.
3. Crie um host no cofre "Time" e rode `lab.ps1 bot hosts`. *Esperado:* o bot vê o host, sem a senha.
4. Rode `lab.ps1 bot rename-host <label> novo-nome`. *Esperado:* o nome muda no app.
5. Mude o bot para **leitor** e rode `lab.ps1 bot add-host Time x 1.2.3.4`. *Esperado:* "read-only access".
6. **Remova** o bot. Rode `lab.ps1 bot vaults`.
   - *Esperado:* o cofre sumiu para o bot.
   - *Esperado:* o cofre mostrou a troca de chave e continua funcionando para você.
7. **Ao contrário:** rode `lab.ps1 bot create-vault Bot` e `lab.ps1 bot invite Bot viewer`. Aceite o link no app em **Cofres → Aceitar convite** e aprove com `lab.ps1 bot approve Bot <seu-email>`.

## 7. Agentes de IA (MCP)

A redação de segredos, a recusa de credenciais e as regras de permissão têm teste automático (`cargo test -p vessel-desktop --lib mcp`). Com o app aberto, confira o fluxo inteiro:

1. **Agentes de IA → Ativar.** As duas permissões começam desligadas. Copie o comando do seu assistente e rode-o.
2. Peça ao assistente "liste meus hosts" e "abra um terminal PowerShell".
   - *Esperado:* funciona, e cada ação aparece no log de atividade.
3. Peça "crie um snippet que mostra o uso de disco" e "rode `uptime` no terminal".
   - *Esperado:* as duas são recusadas, cada uma citando a permissão que falta.
4. Ligue **Permitir criar, editar e excluir** e deixe **Permitir digitar** desligada. Peça um host novo e depois um snippet novo.
   - *Esperado:* o host é criado; o snippet é recusado, porque você o executaria depois.
5. Ligue **Permitir digitar** e peça "abra um terminal no Bastion e rode `uptime`".
   - *Esperado:* funciona. Num host cuja chave você ainda não confirmou no app, o assistente é recusado e pede que você conecte uma vez.
6. Peça "qual a senha do host Bastion?" e "troque a senha do Bastion para X".
   - *Esperado:* ele vê só `has_password: true`, e a escrita é recusada.
7. Peça "liste os snippets que você pode rodar" e "rode o snippet X". *Esperado:* lista vazia e recusa. Marque "Agentes de IA podem rodar este snippet" no snippet e peça de novo. *Esperado:* roda no destino salvo, mesmo com "Permitir digitar" desligada, e entra no histórico como "Agente de IA". Peça que ele mude o script desse snippet. *Esperado:* recusa.
8. Bloqueie o app. *Esperado:* as chamadas respondem que o Vessel está bloqueado.
9. Com `curl`, chame `http://127.0.0.1:47011/mcp` sem token e depois com `-H "Origin: http://exemplo.com"`.
   - *Esperado:* 401 e 403.

## 8. Instalador

1. Instale, abra, feche e reabra. *Esperado:* os dados continuam.
2. Instale por cima com o mesmo instalador. *Esperado:* os dados continuam.
3. Desinstale. Os dados ficam em `%APPDATA%\dev.vessel.desktop`; apague a pasta se quiser zerar.

## 9. Atualização

Com uma versão instalada e um release mais novo publicado:

1. Abra o app e espere uns 15 segundos. *Esperado:* o rodapé do menu mostra "Versão X disponível".
2. **Configurações → Atualizações → Procurar atualizações.** *Esperado:* a mesma versão, com o botão para instalar.
3. Instale. *Esperado:* o app pede confirmação, avisa que terminais serão encerrados, baixa, instala e reabre já na versão nova, com os dados intactos.

As recusas (assinatura de outro arquivo, de outra versão, sem versão ou inválida) foram testadas contra um feed local com uma chave descartável. Para repetir, compile com `npx tauri build --debug --no-bundle` e rode o app com `VESSEL_UPDATE_ENDPOINT` apontando para o seu feed; essa variável só vale em builds de depuração.
