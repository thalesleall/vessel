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
| Logo | Placa na cor de estrutura com um bloco de cursor na cor de sinal |
| Item ativo do menu | Barrinha vertical na cor de sinal |
| Foco | Anel na cor de sinal |
| Terminal vivo | Ponto na cor de sinal |

**Regra:** a cor de sinal só marca o que está vivo, focado ou ativo. Nunca preenche áreas grandes.

## Cores

Cada tema tem três cores definidas pelo cliente e neutros derivados delas.

| Papel | Escuro ("Noite") | Claro ("Dia") |
|---|---|---|
| Base | preto `#030407` (fundo) | `#37B0DE` (seleção, ilustração, info) |
| Estrutura: botões primários, item ativo, seleção | azul cobalto `#1558C9` | berinjela `#400849`, que também é a cor do texto |
| Sinal: cursor, vivo, foco | verde lima `#D4FF24` | laranja sinal `#FF5A0A` |
| Superfícies | `#0A0D16` / `#10151F` / `#161C29` | `#F7F4F8` / `#FFFFFF` / `#FBF9FC` |
| Linhas | `#1D2433` / `#2A3346` | `#E8E0EB` / `#D3C6D8` |
| Texto | `#EDF0F7` / `#A3ACC2` / `#687392` | `#400849` / `#6D5673` / `#9C8BA1` |
| Perigo | `#FF6B5E` | `#C2183A` (diferente do laranja de sinal) |

Observação: o cliente chamou `#37B0DE` de "pastel orchid pink", mas o código é um azul-celeste. Usamos o código.

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

## Onde está a ousadia

Na **tela de boas-vindas**: duas escolhas ilustradas em traço, com um único momento de animação, o desenho dos traços ao abrir. Respeita "reduzir movimento". O resto do app fica quieto.

## Revisão contra o padrão genérico

- Fundo quase preto com verde ácido é um clichê, mas foi pedido pelo cliente. Para não virar o clichê, o cobalto domina a estrutura e o lima fica restrito ao papel de "sinal".
- Removidos:
  - gradiente nos botões e no logo, e brilho no logo;
  - lavagem radial no fundo das telas de login;
  - títulos de seção em caixa alta espaçada;
  - metadados com ponto médio;
  - sombra igual em todos os cartões;
  - a fonte Inter, usada por padrão em todo lugar.
- A fonte mono foi mantida só onde o conteúdo é código.
