# Contratos obrigatórios e análise estática contra erros de concorrência: quanto pegam, quanto custam

**Pergunta.** Até que ponto contratos obrigatórios (efeito declarado, chave de
idempotência, política de incerteza, `requires`, `compensate`, empréstimo de
recursos) e a análise estática do compilador reduzem erros de concorrência em
workflows de agentes, sem sacrificar demais a flexibilidade e o desempenho?

**Resposta curta.**

- **Erros:** dos 25 bugs de concorrência do corpus, a Calyx impede **22 (88%)**.
  - **14** são pegos antes de rodar (11 erros, 3 avisos);
  - **6** o runtime impede (diário, chaves, travas, prazos);
  - **2** não podem ser escritos na linguagem.
  - Pyright e mypy não pegam **nenhum** dos de concorrência da amostra portada para Python (Q2).
- **Flexibilidade:** de 21 padrões **corretos** de concorrência, **16 (76%)** passam como escritos.
  - **2** recebem um aviso sem haver bug.
  - **2** são recusados, por falta de dois recursos que ainda não existem (`fork` e recursão de grafos).
  - **1** não tem como ser escrito (o quórum).
  - Com reescrita, 20 dos 21 passam, a 0 a 4 linhas a mais.
  - Os contratos são **8,4%** das linhas dos exemplos.
- **Desempenho:** a análise custa **2,5 a 3,5 ms** por programa, e o runtime custa o que E2 e W8 já mediram. O custo real está em **três lugares**:
  - a reescrita que o aviso de ordem pede (`after`) põe duas escritas independentes em série: **2× a latência**;
  - o quórum só pode esperar todas as respostas: **6,7×** no exemplo;
  - as chamadas a um mesmo servidor MCP vão uma de cada vez.

O equilíbrio se sustenta porque as regras mais caras em flexibilidade são
**avisos**, e não erros: o programa roda, e o programador decide. Os custos que
sobram vêm de construtos que faltam, não de regras erradas.

Dados: `bench/run_e5.py` → `bench/results/e5.json`; padrões em `bench/e5_flex/`.

## Método

**O que conta como erro de concorrência:** um erro que só aparece quando duas
atividades se sobrepõem ou quando a ordem entre elas importa:
- ramos ou itens paralelos;
- corridas;
- execuções simultâneas;
- entregas duplicadas.

Ficam de fora os erros de uma atividade só: queda e retomada, tipos, declarações, laço de um agente. Do corpus de 54 bugs (`tests/state_bugs/`), 25 são de concorrência. A classificação de cada um está no `run_e5.py`, e o estágio que o pega foi conferido rodando o compilador em todos.

**Flexibilidade:** 21 idiomas comuns em workflows de agentes, todos corretos:
- fan-out e fan-in, escritas paralelas, contador compartilhado;
- *hedging*, corrida com escrita, ordem por dados e sem dados;
- atualização atômica, conferir e agir, leituras paralelas de um repositório, estratégias em cópias separadas;
- agente com tools, laço de novas tentativas, rodadas, espera humana;
- decomposição recursiva, quórum, limite de paralelismo.

Cada um foi escrito **do jeito natural**, antes de rodar o compilador. Quando o compilador recusou ou avisou sem haver bug, a reescrita que passa (`.ok.clyx`) mede o custo.

**Desempenho:**
- **Análise:** tempo de `calyx check`.
- **Runtime:** E2 e W8, já publicados.
- **Reescritas:** latência de cada versão, com tools que levam 0,5 s (um servidor MCP falso, um processo por tool) e modelos falsos de latência fixa. Mediana de 5.

## 1. Quantos erros as regras pegam

| Tipo | Bugs | Compilador | Runtime | Linguagem | Passa |
|---|---|---|---|---|---|
| Escritas em conflito (ramos, itens, repositório) | 7 | 6 | | | 1 (22) |
| Ordem entre efeitos | 4 | 3 | | 1 | |
| Atomicidade (conferir, depois agir) | 4 | 2 | 1 | | 1 (20) |
| Execuções simultâneas | 3 | | 2 | | 1 (41) |
| Corridas | 7 | 3 | 3 | 1 | |
| **Total** | **25** | **14** (11 erros, 3 avisos) | **6** | **2** | **3** |

**O compilador** pega o que depende só da forma do programa:
- dois ramos gravando o mesmo nome (`E0501`);
- itens de um `for each` editando o mesmo repositório (`E0644`);
- leitura e edição ao mesmo tempo (`E0645`);
- empréstimo no modo errado (`E0643`);
- ordem circular (`E0506`);
- `send` calculado de um `ask` à mesma entidade (`W0603`);
- escrita sem ordem (`W0602`);
- escrita num ramo de corrida sem compensação (`W0604`);
- corrida sem saída (`E0683`);
- condição da corrida que gasta (`E0682`).

**O runtime** pega o que só existe ao rodar:
- execuções simultâneas da mesma entidade (`flock` numa máquina, lock de linha no PostgreSQL);
- resposta entregue duas vezes;
- corrida decidida de novo depois de uma queda (o vencedor fica no diário);
- perdedor que continua gastando (cancelamento);
- resposta depois do prazo;
- estado que mudou durante a espera (`requires`, avaliado pelo serviço no momento da escrita).

**A linguagem** impede dois por construção. Não há estado mutável compartilhado dentro de uma execução, e as rodadas têm barreira.

**Os 3 que passam** (20, 22, 41) têm a mesma causa: a Calyx não sabe **qual recurso externo** uma tool toca, nem **o que o usuário quis dizer**.
- **20:** uma leitura seguida de uma escrita no mesmo pedido, sem `requires`.
- **22:** itens paralelos gravando o mesmo arquivo por uma tool.
- **41:** o mesmo depósito mandado por duas execuções, por um clique duplo.

O 20 e o 22 seriam pegos com anotação de recurso nas tools (`resource order`). O 41 é idempotência de negócio: a chave tem que vir do pedido do usuário, não da execução.

**Comparação:** na amostra do corpus portada para Python + LangGraph (Q2, em `comparacao.md`), pyright e mypy pegam 2 de 16, e nenhum é de concorrência. O LangGraph para em 3 ao rodar, dois deles depois do dano.

## 2. Quanto custam em flexibilidade

| Padrão (correto) | Natural | Reescrita | Linhas a mais |
|---|---|---|---|
| p01 fan-out e fan-in | aceito | | |
| p02 escritas paralelas em recursos diferentes | aceito | | |
| p03 escritas paralelas que comutam, no mesmo recurso externo | aceito | | |
| p04 contador compartilhado (entidade) | aceito | | |
| p05 ramos calculam partes de um resultado | aceito | | |
| p06 *hedging* entre dois modelos | aceito | | |
| p07 corrida com escrita compensada (saga) | aceito | | |
| p08 corrida em que cada ramo grava num cache (inofensivo) | **aviso `W0604`** | gravar depois da corrida | −2 |
| p09 dois efeitos em ordem por dados | aceito | | |
| p10 duas escritas independentes, em qualquer ordem | **aviso `W0602`** | `after` | +1 |
| p11 atualização atômica num handler | aceito | | |
| p12 conferir e agir atômico (`requires`) | aceito | | |
| p13 duas leituras do mesmo repositório ao mesmo tempo | aceito | | |
| p14 estratégias em cópias separadas do repositório | **recusado `E0645`** | quem chama passa duas cópias | 0 (o custo vai para quem chama) |
| p15 agente com leituras e uma escrita com chave | aceito | | |
| p16 laço de novas tentativas com escrita idempotente | aceito | | |
| p17 rodadas com barreira | aceito | | |
| p18 espera humana com prazo | aceito | | |
| p19 decomposição recursiva | **recusado `E0101`** | profundidade escrita à mão | +4, e a profundidade fica fixa |
| p20 quórum (2 primeiras de 3 respostas) | **não expressável** | esperar as 3 | — |
| p21 limite de paralelismo | aceito | | |

- **16 de 21 passam como escritos.**
- **Os dois falsos alarmes são avisos, não erros.** O `W0602` não sabe que as duas escritas comutam, e o `W0604` não sabe que a escrita do perdedor é inofensiva. Os dois programas rodam como estão.
- **As duas recusas vêm de construtos que faltam.** `fork` dá uma cópia da sandbox a cada ramo. A recursão de grafos com `decreases` está descrita na spec, mas o compilador ainda responde `E0101`. A regra `E0645` está certa: dois ramos editando o mesmo repositório **é** o bug 47.
- **O quórum não tem construto.** `race first` dá o primeiro, e `take(lista, 2)` pega por posição, não por chegada.
- **Custo de escrita dos contratos:**
  - efeitos e contratos (`effect`, `idempotency_key`, `on_uncertain`, `checks`, `requires`, `compensate`, `after`) são 42 das 498 linhas dos 12 exemplos, ou **8,4%**;
  - limites (`max_output`, `timeout`, `retry_on`, `limits`) são mais 31 linhas (6%).

## 3. Quanto custam em desempenho

**A análise é de graça na prática:** 2,5 ms de mediana por padrão, 3,5 ms no
pior, e 2,6 ms para o maior exemplo (103 linhas), contando o início do processo.

**O runtime** foi medido antes:
- E2: o diário custa cerca de 9% num fan-out de 1.000 itens com modelos instantâneos, e nada mensurável a 10.000. O runtime gasta 0,12 a 0,16 ms por item, contra 0,02 a 0,08 do asyncio.
- W1: com modelos de verdade (1 s por chamada), o paralelismo é o mesmo do asyncio escrito à mão.
- W8: uma entidade disputada aguenta 740 a 880 mensagens/s em arquivos e 116/s com o banco a 7 ms.

**As reescritas** são onde as regras custam tempo (tools de 0,5 s, um processo por tool):

| Caso | Latência |
|---|---|
| p10 duas escritas independentes, como escritas (com o aviso) | 0,55 s |
| p10 reescrita com `after` (sem aviso) | **1,05 s (2×)** |
| p08 cache gravado em cada ramo da corrida (com o aviso) | 1,03 s |
| p08 reescrita: gravar depois da corrida | 0,53 s |
| p20 quórum ideal (2ª resposta, de 0,3 s) | 0,3 s |
| p20 o programa possível: esperar as 3 (até 2 s) | **2,0 s (6,7×)** |

- **O `W0602` empurra para a série.** Silenciar o aviso com `after` custa a latência de uma escrita inteira. Hoje não há como dizer "estas duas comutam" sem impor uma ordem.
- **O `W0604` empurrou para o lado certo.** A corrida espera a escrita em andamento do perdedor terminar (é o que permite desfazê-la), então gravar depois da corrida ficou mais rápido, além de não deixar lixo.
- **O quórum** paga a resposta mais lenta.
- **Achado do runtime:** as chamadas a um mesmo servidor MCP vão uma de cada vez, porque um pipe stdio é usado por uma chamada por vez. Duas escritas independentes em tools do mesmo servidor rodaram em série (1,03 s) mesmo sem `after`. O MCP permite várias chamadas em andamento no mesmo pipe, identificadas por id, então isso é limitação da implementação, não do protocolo.

## 4. Onde fica o equilíbrio, e o que mudaria ele

As regras que **recusam** (erros) são as que só pegam programas errados nos dois
conjuntos: nenhuma recusa nos 21 padrões corretos veio de uma regra que errou.
As que podem errar ficaram como **aviso**. É essa divisão que mantém a
flexibilidade: 76% passam como escritos, e o resto passa com 0 a 4 linhas, ou
com um aviso que o programador pode ignorar.

O que falta, em ordem de retorno:

1. **Declarar independência sem impor ordem:** algo como `unordered sent, logged`. Silencia o `W0602` sem pôr as escritas em série, e acaba com o único custo de 2× do estudo.
2. **Várias chamadas em andamento por servidor MCP.** É desempenho puro, sem mudar a linguagem.
3. **`race first N`:** o quórum, de 2,0 s para 0,3 s no exemplo.
4. **`fork`**, para estratégias em cópias separadas, sem passar o custo para quem chama.
5. **Recursão de grafos com `decreases`.** A spec descreve, mas não está implementada.
6. **Recurso nas tools** (`resource order`): pegaria os bugs 20 e 22, que hoje passam.

## Ameaças à validade

- **Corpus e padrões do mesmo autor.** O autor da Calyx escreveu os 25 bugs e os 21 padrões. Os padrões foram escritos antes de rodar o compilador, mas quem escolhe os idiomas escolhe também o que fica de fora. O estudo de issues reais (`bugs-reais.md`) quase não tem bugs de concorrência do programador: os 5 de escrita concorrente eram bugs dos próprios frameworks. O kit da E4 (outra pessoa porta os bugs) continua sendo o teste independente.
- **Contagem, não taxa de campo.** 88% é a fração de um corpus construído, não a fração de bugs que um time teria em produção.
- **Latências falsas e fixas.** Tools de 0,5 s e modelos de latência fixa: as razões (2×, 6,7×) dependem delas.
- **"Natural" é julgamento.** Outra pessoa escreveria alguns padrões de outro jeito, e talvez caísse em outros avisos.
