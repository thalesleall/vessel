# Brief: Snippets como central de comandos rápidos

Status: implementado (fases 1, 2 e 3). Detalhes em `docs/ARCHITECTURE.md` §9.4 e §10; roteiro de teste em `docs/TESTING.md` §3.5.

## Problema
Hoje um snippet é só um texto (nome, script, tags) que se cola num terminal já aberto. Não tem destino, não executa sozinho e não pode ser agendado. O botão "Executar" fica desativado sem explicação quando não há terminal aberto.

## Objetivo
Transformar Snippets numa central de comandos rápidos:
- clicar e executar, num destino local ou em hosts SSH (por exemplo o Wake-on-LAN de um PC);
- agendar comandos para rodarem sozinhos;
- manter o "Snippet ao conectar" de cada host.

## Decisões tomadas
1. Um snippet pode ter destino **local** (PowerShell, CMD ou WSL) ou **SSH** (um ou mais hosts). As duas formas valem igualmente.
2. O agendamento é **só local**: roda no Vessel, não instala crontab no servidor.
3. Fechar a janela deixa o Vessel **na bandeja**, para os agendamentos continuarem.
4. Agentes de IA (ponte MCP) só disparam os snippets **marcados como permitidos**.

## Modelo de dados (`crates/vessel-model`, `Snippet`)
Campos novos:

| Campo | Valores | Padrão |
|---|---|---|
| `target` | `none` · `local { shell }` · `hosts [ids]` | `none` |
| `confirm` | pede confirmação antes de rodar | `false` |
| `schedules` | lista de `{ cron, enabled }` | vazio |
| `agent_allowed` | agentes de IA podem disparar | `false` |
| `timeout_secs` | tempo máximo de execução | 60 |

- Os snippets existentes viram `target: none`, sem migração.
- O `extra` (`serde(flatten)`) preserva os campos novos quando uma versão antiga do app sincroniza.

## Fase 1: execução com um clique
**Backend (`src-tauri`)**
- Execução local: shell sem janela, com timeout e captura de stdout, stderr e código de saída.
- Execução SSH: usa a mesma conexão e verificação de chave do terminal. Uma chave desconhecida ou diferente recusa a execução.
- Vários hosts rodam em paralelo, cada um com o seu resultado.

**Histórico de execuções**
- Cada execução guarda data, origem (manual, agendada ou agente), destino, código de saída e a saída (cortada em 64 KB).
- O histórico é local e cifrado, e **não é sincronizado**.

**Interface (`views/snippets.tsx`)**
- Grade de cartões com nome, ícone do destino e botão ▶.
- O cartão mostra o estado: rodando, ✓ ou ✗.
- O formulário ganha o destino, a confirmação e o timeout.
- "Colar no terminal" continua para `target: none`, com o motivo explicado quando estiver desativado.

**Paleta de comandos**
- Rodar um snippet pela paleta (Ctrl+K).

**Snippet ao conectar**
- Passa a aceitar vários snippets, em ordem.

## Fase 2: agendamento local
**Bandeja**
- Fechar a janela deixa o Vessel na bandeja.
- O menu da bandeja tem Abrir, Pausar agendamentos e Sair.
- Opção "iniciar com o Windows", desligada por padrão.

**Agendador (backend)**
- Executa os horários definidos pelas expressões cron dos snippets.
- Só roda com o cofre desbloqueado. Bloqueado, a execução fica registrada como "pulada".
- Horário perdido com o app fechado não roda depois; fica registrado como "perdido".
- O mesmo snippet nunca roda duas vezes ao mesmo tempo.
- Um botão global pausa todos os agendamentos.

**Bloqueio automático do cofre**
- O cofre se bloqueia por inatividade, o que impediria os agendamentos.
- Proposta: opção "manter desbloqueado enquanto houver agendamentos", desligada por padrão e com aviso do risco.

**Na tela**
- O próximo horário de cada agendamento aparece no cartão.
- Uma falha gera uma notificação do Windows.

## Fase 3: agentes de IA
- A ponte MCP lista e executa **apenas** os snippets com `agent_allowed`.
- O agente dispara o snippet, mas não altera o script nem o destino.
- Cada disparo vai para o histórico com a origem "agente".

## Segurança
- O script continua cifrado de ponta a ponta no cofre.
- Uma execução sem o usuário na frente (agendada ou por um agente) nunca aceita uma chave de host nova.
- O histórico pode conter dados sensíveis, por isso fica só no dispositivo e cifrado.
- O usuário tem sempre um botão para pausar todos os agendamentos.

## Testes
- **Unitários:** o cálculo de horários a partir do cron; a compatibilidade do modelo (snippet antigo para novo e de volta).
- **Integração:**
  - execução local com timeout;
  - execução SSH num servidor de teste;
  - chave de host diferente recusada;
  - cofre bloqueado pulando o agendamento;
  - ponte MCP recusando um snippet não permitido.
- **E2E:** criar um snippet local, rodar pela grade e ver o resultado no histórico.

## Fora do escopo
- Crontab remoto no servidor.
- Agendamento com o app fechado.
- Sincronizar o histórico entre dispositivos.
- Instaladores de macOS e Linux (tratados à parte).
