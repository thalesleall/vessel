# Design do Vessel

Referência para quem mexer na interface. Os tokens estão em `apps/desktop/src/styles.css`, no início do arquivo.

## Briefing

- **Produto:** gerenciador de terminais e SSH, com sync criptografado e agentes de IA.
- **Público:** devs e pessoal de infraestrutura que passam o dia no terminal.
- **Função principal:** chegar ao terminal certo em poucos segundos, confiando que as credenciais estão seguras.

## Motivo visual: o cursor

O cursor do terminal é o elemento que se repete em todo o app:

| Onde | Como aparece |
|---|---|
| Logo | Um barco feito do prompt: o `>` e o bloco do cursor (na cor de sinal) são as velas, sobre um casco escuro, numa placa azul cobalto. Desenhado numa grade de 32 px (`apps/desktop/logo.svg`), com versões redesenhadas para 16, 24 e 48 px em `apps/desktop/icon-src/`, para que toda borda caia num pixel inteiro. Gere todos os ícones com `node scripts/build-icons.mjs` |
| Item ativo do menu | Barrinha vertical na cor de sinal |
| Foco | Anel na cor de sinal |
| Terminal vivo | Ponto na cor de sinal |

**Regra:** a cor de sinal só marca o que está vivo, focado ou ativo. Nunca preenche áreas grandes.

## Cores

Cada tema tem três cores principais e neutros derivados delas.

| Papel | Escuro ("Noite") | Claro ("Dia") |
|---|---|---|
| Base | preto `#030407` (fundo) | `#37B0DE` (seleção, ilustração, info) |
| Estrutura: botões primários, item ativo, seleção | azul cobalto `#1558C9` | berinjela `#400849`, que também é a cor do texto |
| Sinal: cursor, vivo, foco | verde lima `#D4FF24` | laranja sinal `#FF5A0A` |
| Superfícies | `#0A0D16` / `#10151F` / `#161C29` | `#F7F4F8` / `#FFFFFF` / `#FBF9FC` |
| Linhas | `#1D2433` / `#2A3346` | `#E8E0EB` / `#D3C6D8` |
| Texto | `#EDF0F7` / `#A3ACC2` / `#687392` | `#400849` / `#6D5673` / `#9C8BA1` |
| Perigo | `#FF6B5E` | `#C2183A` (diferente do laranja de sinal) |

## Tipografia

- **IBM Plex Sans** para a interface e **IBM Plex Mono** para o que é literalmente código: caminhos, impressões digitais, comandos e símbolos de shell. Uma família só, com engenharia como herança.
- Escala: 12 / 13,5 / 15 / 19 / 26 px. Corpo em 13,5/1,5. Títulos com peso 600 e espaçamento -0,01em.
- Sem rótulos em caixa alta, sem separadores com ponto médio e sem setas "→" decorativas.

## Layout e forma

- Menu lateral de 232 px e barra de abas; páginas alinhadas à esquerda.
- A hierarquia vem de passos de superfície e linhas, não de sombras.
- Raio por papel:

  | Elemento | Raio |
  |---|---|
  | Modais e cartões da tela inicial | 14 px |
  | Painéis e cartões | 10 px |
  | Campos e botões | 8 px |
  | Selos | 6 px |

## Movimento

A **tela de boas-vindas** tem o único momento de animação longa: duas escolhas ilustradas em traço, desenhadas ao abrir. No resto do app as transições são curtas e discretas. Tudo respeita a preferência "reduzir movimento" do sistema.

## O que evitar

- A cor de sinal em áreas grandes: ela é do cursor, do foco e do que está vivo.
- Gradientes e brilho em botões e no logo, e fundos com lavagem radial.
- Títulos de seção em caixa alta espaçada e metadados separados por ponto médio.
- A mesma sombra em todos os cartões: a hierarquia vem das superfícies e das linhas.
- Fonte mono fora de conteúdo que é código.
