# Comparação: Calyx × Python × LangGraph × Temporal

Medição contra baselines. Os mesmos workflows, escritos em Calyx e em Python
(sequencial, asyncio escrito à mão, LangGraph 1.2.12 e Temporal, SDK Python
1.34.0 com servidor 1.32.0), rodando contra o mesmo mundo falso: modelos com
latência fixa e as mesmas tools MCP. O código, as regras para ser justo e os
comandos para repetir estão em [`bench/`](../../bench/README.md); os números
crus, em `bench/results/`.

**Resumo.**

| Pergunta | Calyx | Melhor baseline | Conclusão |
|---|---|---|---|
| **Q3: recuperação com efeitos externos** (W2, 6 pontos de queda) | 6 de 6 certos, sem código de recuperação | Temporal e LangGraph `sync`: 4 de 6; 6 de 6 só com cuidado manual. LangGraph no padrão: 3 de 6 | **A diferença é o padrão, não o teto:** com cuidado manual os baselines empatam; na Calyx o cuidado é obrigatório |
| **Espera por humano com prazo** (W3, 6 cenários com o processo parado) | 6 de 6 (5 de 6 antes de corrigir um bug que a W3 achou) | Temporal: 5 de 6; LangGraph: 3 de 6 (sem prazo); com cuidado manual, 6 de 6 | Mesmo padrão da W2: a diferença é o padrão, não o teto |
| **Memória compartilhada** (W7, execuções simultâneas e quedas) | 3 de 3 | LangGraph *Store*: 0 de 3 (perde atualizações em metade das rodadas, repete sempre na retomada); com cuidado manual, 3 de 3 | Idem: com cuidado manual empata; na Calyx é o padrão |
| **Q2: bugs antes de rodar** (14 bugs que a Calyx pega) | 14 de 14 | pyright + mypy: 2 de 14; LangGraph para 3 ao rodar, 2 depois do dano | Forte, mas o corpus foi escrito por quem fez o compilador |
| **Bugs reais** (44 issues de LangGraph, CrewAI, AutoGen; [`bugs-reais.md`](bugs-reais.md)) | Dos 15 de workflow: evita 7 (runtime 5, construção 2), deixa passar 8; compilador: 0 | — (29 dos 44 são bugs dos próprios frameworks) | Issues relatam o framework errando, não o programador: não confirmam a Q2. Achada uma lacuna (`write once` em laço), que virou o aviso `W0605` |
| **Q1: paralelismo** (W1) | A 30–50 ms do limite teórico | asyncio à mão: a 80–95 ms; LangGraph: +0,8 s | Empate com asyncio. O ganho é não escrever o paralelismo, não ser mais rápido |
| **Custo do runtime** (W1 sem latência, E2) | Linear até 10⁵ itens, 0,12–0,16 ms por item; agentes quadráticos nas voltas (eram cúbicos: corrigido) | asyncio: 0,025 ms; LangGraph: 7–8 ms e crescendo | Desprezível perto de uma chamada de modelo; o LangGraph cresce mais que linearmente |

## W2: recuperação com efeitos externos (Q3)

O reembolso `pedido → decisão (modelo) → pagamento → resposta (modelo) →
e-mail`, contra a loja falsa (MCP), que guarda os pagamentos e os e-mails
num arquivo. O processo morre em **6 pontos** e é retomado do jeito que cada
sistema oferece:

- **depois de cada passo registrado** (pedido, decisão, pagamento,
  resposta): o passo terminou e a execução o registrou; o processo morre
  antes do seguinte. Na Calyx, `CALYX_CRASH_AFTER=k`; no Python, o processo
  sai quando o passo seguinte começa;
- **efeito em andamento** (pagamento, e-mail): a loja pagou ou enviou, mas a
  resposta ainda não chegou quando o processo leva `kill -9`.

O certo é **1 pagamento, 1 e-mail e 2 chamadas de modelo**. "Cuidado manual"
é o que a documentação dos frameworks recomenda e um programador atento
escreve: chave de idempotência no pagamento e conferir se o e-mail já saiu
antes de reenviar. No Temporal, cada passo é uma *activity*; retomar é subir
um worker novo, e o workflow continua do histórico guardado no servidor.

| Sistema | Efeitos certos | Pagamentos duplicados | E-mails duplicados | Chamadas de modelo refeitas | Tempo da retomada |
|---|---|---|---|---|---|
| **Calyx** | **6 de 6** | 0 | 0 | 0 | 0,03–2,0 s |
| Temporal | 4 de 6 | 1 | 1 | 0 | 11–15 s |
| Temporal + cuidado manual | 6 de 6 | 0 | 0 | 0 | 11–13 s |
| LangGraph `durability="sync"` | 4 de 6 | 1 | 1 | 0 | 0,9–4,8 s |
| LangGraph `sync` + cuidado manual | 6 de 6 | 0 | 0 | 0 | 0,8–2,9 s |
| LangGraph padrão (`durability="async"`) | 3 de 6 | 2 | 1 | 2 | 1,8–4,9 s |
| LangGraph padrão + cuidado manual | 6 de 6 | 0 | 0 | 2 | 0,8–2,8 s |
| Python sem checkpoint | 2 de 6 | 4 | 1 | 7 | 2,1–5,1 s |
| Python sem checkpoint + cuidado manual | 6 de 6 | 0 | 0 | 7 | 2,1 s |

Por ponto de queda (✅ = 1 pagamento e 1 e-mail):

| Sistema | Depois do pedido | Depois da decisão | Depois do pagamento | Depois da resposta | Pagamento em andamento | E-mail em andamento |
|---|---|---|---|---|---|---|
| **Calyx** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Temporal | ✅ | ✅ | ✅ | ✅ | ❌ 2 pagamentos | ❌ 2 e-mails |
| LangGraph `sync` | ✅ | ✅ | ✅ | ✅ | ❌ 2 pagamentos | ❌ 2 e-mails |
| LangGraph padrão | ✅ | ✅ (modelo refeito) | ❌ 2 pagamentos | ✅ (modelo refeito) | ❌ 2 pagamentos | ❌ 2 e-mails |
| Python sem checkpoint | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ |
| Qualquer um + cuidado manual | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

**O que explica cada linha:**

- **Entre passos, Temporal e LangGraph `sync` acertam sozinhos, como a
  Calyx.** O histórico (Temporal) e o checkpoint síncrono (LangGraph)
  guardam cada passo antes do seguinte.
- **LangGraph no padrão perde o último checkpoint.** Desde a 1.x, o padrão é
  `durability="async"`: o checkpoint de um passo é gravado enquanto o passo
  seguinte já roda. Se o processo morre nesse intervalo, o passo é refeito:
  duas chamadas de modelo refeitas e um pagamento duplicado numa queda
  *depois* do pagamento.
- **Com o efeito em andamento, nenhum registro por passo resolve, nem o
  histórico do Temporal.** O efeito aconteceu e a resposta não voltou; o
  Temporal repete a activity depois do timeout, como deve. Só evitam a
  duplicata uma chave de idempotência que o provedor respeite (pagamento) e
  conferir antes de repetir (e-mail). Nos três baselines, as duas são código
  que o programador precisa lembrar de escrever, e nada avisa quando ele
  esquece.
- **Na Calyx, as duas vêm do contrato da tool:**
  - `refund` declara `idempotency_key request`; a chave vai para a loja e a
    retomada repete com a mesma chave.
  - `email` é `write once` com `on_uncertain verify(email_sent(...))`; a
    retomada encontra o `begin` sem resposta no diário e pergunta antes de
    reenviar.

  O compilador recusa um `write once` sem política (`E0304`) e avisa sobre
  uma escrita sem chave (`W0601`, um aviso, não um erro).
- **Tempo da retomada:** o Temporal leva 11–15 s porque uma activity que
  estava rodando quando o worker morreu só é repetida depois do seu
  `start_to_close_timeout` (10 s aqui). *Heartbeats* encurtariam isso; a
  comparação justa de tempo de retomada fica para o plano. A Calyx retoma na
  hora porque a queda é do processo inteiro: não há worker para esperar.
- **Sem checkpoint, o cuidado manual protege os efeitos, mas tudo é
  refeito:** 7 chamadas de modelo refeitas nos 6 pontos.

**Leitura honesta:** a recuperação da Calyx não é melhor que a do Temporal
ou a do LangGraph `sync` com cuidado manual: os três chegam a 6 de 6. A
diferença é que **na Calyx o cuidado manual não é opcional**. O contrato do
efeito é parte da declaração da tool, e o compilador recusa (ou avisa sobre)
a tool que não o tem. Nos baselines, a versão sem cuidado é a que roda por
padrão e erra 2 a 3 de 6 pontos, em silêncio.

**Custo de escrever:** a versão em Calyx tem 42 linhas efetivas, já com os
tipos e os contratos das tools. A versão em LangGraph tem 69 (fluxo + grafo),
a do Temporal, 101 (41 do fluxo + 60 do workflow e do worker); o "cuidado manual" são 3
linhas em cada. Linhas de código são uma métrica fraca aqui. O ponto é
outro: as 3 linhas são **opcionais** no Python e **obrigatórias** na Calyx.

## W3: aprovação humana com prazo

O mesmo reembolso, agora com uma pessoa aprovando: `pedido → decisão
(modelo) → espera pela resposta (prazo de 3 s) → paga e manda e-mail, ou
manda a recusa`. Na Calyx, a espera é um `receive` com `timeout`; no
LangGraph, um `interrupt()` num nó só dele, como a documentação recomenda;
no Temporal, um *signal* com `wait_condition(..., timeout=...)`. Em todos os
cenários **o processo termina enquanto a execução espera**, e os passos
seguintes são feitos do jeito que cada sistema oferece: entregar a resposta
(`calyx deliver`, `Command(resume=...)`, o *signal*) e continuar pelo
agendador (`calyx tick`, um worker do Temporal; o LangGraph não tem prazo,
então só a versão com cuidado manual tem o que fazer).

"Cuidado manual" é o que um programador atento escreve: no LangGraph, o
prazo guardado no estado, uma rotina que retoma as threads vencidas, e a
resposta carimbada com a hora em que chegou; no Temporal, o carimbo da hora
no *signal* e a comparação com o prazo dentro do workflow.

| Cenário | Calyx | LangGraph | LangGraph + cuidado | Temporal | Temporal + cuidado |
|---|---|---|---|---|---|
| Aprovada | ✅ | ✅ | ✅ | ✅ | ✅ |
| Prazo vence com tudo parado | ✅ | ❌ espera para sempre | ✅ | ✅ | ✅ |
| Resposta em dobro | ✅ | ✅ | ✅ | ✅ | ✅ |
| Resposta atrasada, depois da recusa | ✅ | ❌ paga | ✅ | ✅ | ✅ |
| Resposta atrasada, antes da retomada | ✅ \* | ❌ paga | ✅ | ❌ paga | ✅ |
| Resposta no prazo, retomada depois | ✅ | ✅ | ✅ | ✅ | ✅ |
| **Total** | **6 de 6** | 3 de 6 | 6 de 6 | 5 de 6 | 6 de 6 |

Em todos, uma chamada de modelo só (a proposta não é refeita na retomada).

- **O LangGraph não tem prazo para um `interrupt()`.** Sem código a mais, a
  execução espera para sempre, e uma resposta de qualquer hora é aceita. O
  cuidado manual resolve, com uma rotina agendada escrita à mão.
- **O Temporal tem prazo de verdade** (o timer roda no servidor, sem
  worker), mas tem um caso sutil: se nenhum worker estava rodando quando o
  prazo venceu, o timer e uma resposta atrasada chegam juntos ao próximo
  worker, e o SDK entrega os *signals* antes dos timers. A resposta atrasada
  vale e o pagamento sai.
- **\* A Calyx tinha o mesmo bug**, e a W3 o achou: o `calyx deliver`
  aceitava uma resposta depois do prazo enquanto ninguém tinha rodado o
  `tick`, e a retomada a tomava. Agora o `deliver` recusa a resposta atrasada,
  a entrega guarda a hora em que chegou e o runtime confere essa hora contra
  o prazo (teste `an_answer_after_the_deadline_is_not_taken`). Antes da
  correção, a Calyx fazia 5 de 6, como o Temporal.
- **A resposta em dobro não pegou ninguém:** a Calyx e o Temporal recusam a
  segunda entrega quando a execução já terminou, e o LangGraph ignora um
  `resume` numa thread terminada.

O que a W3 mostra é o mesmo padrão da W2: **com cuidado manual todos
acertam; a diferença é o padrão.** Na Calyx o prazo é obrigatório (`E0671`)
e a regra de quando uma resposta vale fica no runtime, não em cada programa.

## W7: memória compartilhada entre execuções

Um turno de conversa: `lembrar a memória do usuário → responder (modelo) →
extrair 3 fatos (modelo) → somar os fatos à memória e contar a conversa`.
Cada execução é um processo, como quando chegam juntos vários pedidos do
mesmo usuário. Na Calyx, a memória é uma entidade (`ask`/`send`); no
LangGraph, o *Store* (SQLite), do jeito que a documentação mostra:
`store.get`, juntar, `store.put`. A queda é o `kill -9` logo depois de a
memória ser gravada e antes de a execução registrar isso (na Calyx,
`CALYX_CRASH_IN_SEND`; no LangGraph, antes do checkpoint do nó); depois a
execução é retomada.

"Cuidado manual" no LangGraph: nada de ler, mudar e gravar; cada fato é um
item com chave tirada do id da mensagem, então duas execuções nunca gravam
por cima uma da outra e uma mensagem repetida grava as mesmas chaves.

| Cenário | Calyx | LangGraph (Store) | LangGraph + cuidado manual |
|---|---|---|---|
| 20 execuções juntas | ✅ 20 conversas, 60 fatos | ❌ às vezes: 17 conversas, 9 fatos perdidos (na rodada do harness) | ✅ |
| Queda depois de gravar, e retomada | ✅ 1 conversa, 3 fatos | ❌ 2 conversas, 3 fatos repetidos | ✅ |
| 10 juntas, todas caem e são retomadas | ✅ 10 conversas, 30 fatos | ❌ 20 conversas, 30 fatos repetidos | ✅ |

- **Execuções simultâneas perdem atualizações** no LangGraph sem cuidado: duas
  leem a mesma memória e a última a gravar apaga o que a outra somou. **Não
  acontece sempre:** em 6 rodadas, 3 saíram certas e 3 perderam de 1 a 3
  conversas (com seus fatos). É o pior tipo de bug, o que passa nos testes.
  A Calyx acertou em todas.
- **A retomada repete a memória:** o nó que gravou é refeito, porque o
  checkpoint dele não chegou a ser escrito. A documentação do LangGraph pede
  nós idempotentes; o *Store* não ajuda a fazer isso.
- **Na Calyx não há o que escrever:** a entidade aplica uma mudança por vez
  por chave (`flock`) e cada mensagem uma vez só (o id dela fica gravado com
  o estado), e o compilador avisa quando um programa lê a entidade e manda
  de volta um valor calculado com o que leu (`W0603`).
- O tempo não é comparável: no LangGraph, cada processo paga a subida do
  Python e das bibliotecas.

## Q2: bugs de estado antes de rodar

16 bugs do corpus (`tests/state_bugs/`, mesmo número) reescritos em Python +
LangGraph **no melhor caso para o Python**: estado em `TypedDict`, funções
anotadas, pyright 1.1.414 e mypy 2.3.1, sem nenhum erro de tipo nas versões
sem o bug. Para cada um: aparece no pyright ou no mypy? Ao rodar? Causa dano?

| # | Bug | Calyx | pyright / mypy | Ao rodar (Python) |
|---|---|---|---|---|
| 01 | Dois ramos paralelos gravam a mesma chave | `E0501` | — | `InvalidUpdateError` (LangGraph para) |
| 02 | E-mail sem plano para queda no meio do envio | `E0304` | — | nada (latente) |
| 03 | Agente com tool de e-mail | `E0640` | — | 6 e-mails, depois `GraphRecursionError` |
| 04 | E-mail pode sair antes do pagamento | `W0602` | — | nada (latente) |
| 06 | Precondição com nome de campo errado | `E0603` | ✅ os dois | `KeyError` |
| 13 | Pagamento sem chave de idempotência | `W0601` | — | **2 pagamentos** |
| 15 | Duas escritas esperando uma pela outra | `E0506` | — | 5 pagamentos e 5 e-mails, depois `GraphRecursionError` |
| 16 | Grafo "só de leitura" que paga | `E0701` | — | 1 pagamento indevido |
| 17 | Resultado que pode falhar usado como sucesso | `E0608` | ✅ os dois | nada nesta execução |
| 23 | Itens paralelos editam o mesmo repositório | `E0644` | — | o arquivo final depende da ordem das edições |
| 33 | Ler, somar e gravar (atualização perdida) | `W0603` | — | saldo 10 em vez de 20 |
| 36 | Mensagem com nome errado para a entidade | `E0655` | — | depósito perdido em silêncio |
| 42 | Espera de aprovação sem prazo | `E0671` | — | nada (espera para sempre) |
| 52 | Condição da corrida chama um modelo | `E0682` | — | uma chamada paga a mais por ramo |
| 20 | Conferir e agir separados, sem `requires` | ninguém | — | reembolso de pedido cancelado |
| 21 | Chave de idempotência errada | ninguém | — | 2º reembolso descartado em silêncio |

- **Dos 14 bugs que a Calyx pega antes de rodar, o pyright e o mypy pegam 2**
  (06 e 17). Os dois são erros de tipo comuns, que um checador já sabe
  procurar. Nenhum bug de efeito, ordem, concorrência ou espera aparece.
- **O LangGraph para 3 ao rodar**, e em dois deles (03, 15) só depois do
  dano: 6 e-mails e 5 pagamentos duplicados antes do limite de recursão.
- **Os 2 bugs que ninguém pega** (20, 21) passam em silêncio nos dois.
  Ficam no corpus de propósito.
- **Viés de seleção:** os 14 primeiros foram escolhidos entre os que a Calyx
  pega, para ver o que o Python faz com eles. A taxa da Calyx no corpus
  inteiro é **35 de 54**, não 14 de 14. O plano do paper pede o corpus
  inteiro portado por outra pessoa e bugs tirados de issues reais. O estudo
  das issues está em [`bugs-reais.md`](bugs-reais.md): o compilador não pegou
  nenhum dos 44, porque as issues relatam erros dos frameworks, não do
  programador.

## W1: paralelismo e escala (Q1)

N perguntas → busca + resumo de cada → relatório. No máximo 8 chamadas ao
mesmo tempo em todas as versões paralelas. Tempo de parede do processo
inteiro (com a inicialização), mediana de 3 repetições.

**Com latência (cada chamada de modelo leva 1 s).** O limite teórico é
`⌈N/8⌉ + 1` s: as rodadas de 8 resumos e depois o relatório.

| N | Limite | Calyx | asyncio à mão | LangGraph | Python sequencial | Calyx `--deterministic` |
|---|---|---|---|---|---|---|
| 5 | 2 s | 2,032 s | 2,089 s | 2,802 s | 6,070 s | 6,035 s |
| 20 | 4 s | 4,037 s | 4,082 s | 4,878 s | 21,084 s | 21,060 s |
| 50 | 8 s | 8,046 s | 8,095 s | 8,826 s | — | — |

- **A Calyx fica a 30–50 ms do limite**, sem nenhuma palavra de paralelismo
  no programa (12 linhas). O asyncio escrito à mão fica a 80–95 ms (14
  linhas, com `gather` e `Semaphore`). O LangGraph fica a ~0,8 s: quase tudo
  é a inicialização da biblioteca (33 linhas, com `Send` e um redutor).
- **Empate com o asyncio.** O ganho da Calyx não é ser mais rápida, é não
  escrever o paralelismo, e o mesmo programa roda em sequência com
  `--deterministic`.
- **O diário não custa nada visível** quando o modelo leva 1 s (2,032 s com
  e sem diário).

**Sem latência (custo do runtime por item).**

| N | Calyx | Calyx sem diário | asyncio | LangGraph |
|---|---|---|---|---|
| 10 | 0,026 s | 0,030 s | 0,070 s | 0,843 s |
| 100 | 0,041 s | 0,042 s | 0,072 s | 0,844 s |
| 1.000 | 0,167 s | 0,174 s | 0,084 s | 2,413 s |
| 10.000 | 1,617 s (0,16 ms/item) | 1,402 s | 0,253 s (0,025 ms/item) | **84,3 s** (8,4 ms/item) |

- **A Calyx é linear:** de 1.000 para 10.000 itens, 9,7× o tempo. O LangGraph
  não é: 35× o tempo para 10× os itens.
- **A inicialização da Calyx é a menor:** 26 ms, contra 70 ms do Python e
  840 ms do LangGraph.
- **Por item, a Calyx é 6× mais cara que o asyncio**, mas a comparação a
  desfavorece: cada item da Calyx faz uma chamada MCP de verdade (um processo
  Python separado, por um pipe), enquanto as versões em Python chamam a busca
  como uma função. Mesmo assim, 0,16 ms por item é ~6.000 vezes menos que uma
  chamada de modelo de 1 s.

## E2: custo do runtime até 100.000 itens (A4)

A W1 sem latência, agora até 10⁵ itens (as perguntas vão num arquivo,
`--questions @arquivo.json`: 100.000 não cabem num argumento), e um agente
cujo modelo chama uma tool a cada volta (`fake-busy`), de 50 a 800 voltas.
Mediana de 3 repetições (1 com 100.000 itens e com o LangGraph a 10.000).

| N | Calyx | Calyx sem diário | asyncio | LangGraph |
|---|---|---|---|---|
| 1.000 | 0,162 s (0,16 ms/item) | 0,149 s | 0,077 s | 1,97 s (2,0 ms/item) |
| 10.000 | 1,220 s (0,12 ms/item) | 1,252 s | 0,226 s | 71,8 s (7,2 ms/item) |
| 30.000 | 3,818 s (0,13 ms/item) | 3,515 s | 0,583 s | — |
| 100.000 | 13,85 s (0,14 ms/item) | 12,12 s | 2,46 s (0,025 ms/item) | — |

- **O fan-out da Calyx é linear até 10⁵:** de 0,12 a 0,16 ms por item em toda
  a faixa. O diário custa até ~14% (com 100.000 itens), porque o `fsync`
  é feito no máximo uma vez por segundo, não por entrada.
- O LangGraph não foi além de 10.000: a 7 ms por item, e crescendo, 30.000
  levariam quase uma hora.
- O asyncio continua ~5× mais barato por item, pelo mesmo motivo de antes: a
  Calyx faz uma chamada MCP de verdade por item, o Python chama uma função.

**O custo que o plano temia apareceu nos agentes, e foi corrigido.** O passo
de um agente é avaliado de novo cada vez que uma chamada dele responde, e
cada avaliação recomeçava da primeira volta, refazendo a conversa e
procurando cada volta anterior com um pedido cada vez maior: o custo crescia
com o **cubo** das voltas. Agora o progresso do agente fica guardado entre
avaliações (e, numa retomada, vem do diário uma vez).

| Voltas | Antes | Depois |
|---|---|---|
| 100 | 1,27 s | 0,165 s |
| 200 | 9,27 s | 0,446 s |
| 400 | 69,7 s | 1,548 s |
| 800 | — | 5,791 s |

O que sobra cresce com o quadrado das voltas, e é do protocolo: cada volta
manda a conversa inteira ao modelo. Com as 6 a 20 voltas de um agente comum,
o custo é de milissegundos. O fan-out dentro de `rounds` (`for each` numa
expressão) também foi medido e é linear: 0,023 ms por item até 100.000.

## W1 e W2 com um modelo real (Gemini)

Os dois experimentos rodaram de novo com `gemini-3.5-flash-lite` no lugar
do modelo falso, em todos os sistemas. Os baselines em Python chamam o mesmo
endpoint (compatível com a OpenAI) que o runtime da Calyx, com as mesmas
novas tentativas. A chave de teste tem limite de requisições por minuto. Por
isso:
- no W1, N fica em 5 e 10;
- no W1, há 65 s de pausa entre rodadas, e a ordem dos sistemas muda a cada
  repetição;
- no W2, há 12 s de pausa entre rodadas.

Nenhuma rodada precisou de nova tentativa. Os scripts são
`bench/run_w1_real.py` e `bench/run_w2.py --real`; os dados estão em
`bench/results/w1_real.json` e `w2_real.json`.

**W1, tempo total, mediana de 3 repetições (mínimo e máximo):**

| Sistema | N = 5 | N = 10 |
|---|---|---|
| **Calyx** | **5,1 s** (4,6–5,6) | **6,1 s** (5,7–6,6) |
| Python asyncio | 5,2 s (5,2–5,7) | 6,4 s (6,2–8,2) |
| LangGraph | 7,3 s (6,2–9,5) | 8,1 s (6,9–10,2) |
| Python sequencial | 10,6 s (10,0–11,1) | 17,3 s (16,8–19,6) |

O resultado com o modelo falso se mantém. A Calyx empata com o asyncio
escrito à mão. O LangGraph fica 2 s atrás, quase tudo inicialização. O
sequencial cresce com N. A variância do provedor (1 a 2 s entre repetições)
é da mesma ordem que as diferenças entre Calyx e asyncio, que continuam
sendo um empate.

**W2, 6 pontos de queda, certos (1 pagamento, 1 e-mail, retomada sem erro):**

| Sistema | Certos | Com cuidado manual |
|---|---|---|
| **Calyx** | **6/6**, sem refazer chamada de modelo | — |
| Temporal | 4/6 | 6/6 |
| LangGraph `durability="sync"` | 4/6 | 6/6 |
| LangGraph padrão | 3/6, e refaz 2 chamadas de modelo | 6/6 |
| Python sem checkpoint | 2/6, e refaz até 2 chamadas | 6/6 |

São os mesmos números do modelo falso, caso a caso. As retomadas da Calyx
levam de 0,03 s a 2,7 s. As do Temporal levam de 11 s a 16 s, com os
timeouts padrão.

**O que o modelo real não mostrou.** Um modelo real pode responder outra
coisa quando é chamado de novo. Um sistema que refaz a chamada depois da
queda poderia então pagar um valor e escrever no e-mail outro. O harness
confere isso (`consistent`: o valor pago é o do e-mail). Neste cenário,
porém, o Gemini propôs o reembolso total (300) todas as vezes; conferimos
com 5 chamadas à parte. Então nenhuma incoerência apareceu, e a medida não
distinguiu os sistemas. Para mostrar o risco, seria preciso um pedido cuja
resposta varie de verdade.

## W2 contra o Stripe real

Toda a W2 acima roda contra uma loja falsa, escrita pelo autor da Calyx.
Para tirar essa dúvida, a matriz de quedas foi repetida contra a **API do
Stripe em modo de teste** (`bench/stripe/`, resultados em
`bench/results/stripe.json`):

- `stripe_server.py` é um servidor MCP na frente da API do Stripe. O
  reembolso (`refund`) repassa a chave de idempotência da Calyx para o
  cabeçalho `Idempotency-Key` do Stripe, e anuncia `idempotencyKeyHint: true`
  (a anotação proposta em [`docs/mcp/`](../mcp/idempotency-key-hint.md)).
  O crédito na conta do cliente (`credit`, uma transação de saldo) o Stripe
  não deduplica: a Calyx o declara `write once` com
  `verify(credit_given(...))`.
- Cada caso cria um pedido novo de US$ 300 (cliente + PaymentIntent pago com
  o cartão de teste), mata o processo (`kill -9`) e retoma com
  `calyx resume`. Os reembolsos e créditos são contados **perguntando ao
  Stripe**, não à Calyx.
- O controle é o mesmo programa sem os contratos (sem chave, o crédito como
  `write` comum); a Calyx avisa (`W0601`) e roda mesmo assim.

Duas rodadas, resultado idêntico:

| Ponto de queda | Calyx | Sem os contratos |
|---|---|---|
| depois de `get_order` | 1 reembolso, 1 crédito | 1, 1 |
| depois de `decide` | 1, 1 | 1, 1 |
| depois do reembolso | 1, 1 | 1, 1 |
| reembolso a caminho | 1, 1 | **2 reembolsos**, 1 |
| crédito a caminho | 1, 1 | 1, **2 créditos** |
| **Certos** | **5 de 5** | **3 de 5** |

No reembolso a caminho, a Calyx reenviou com a mesma chave e o Stripe
devolveu o reembolso já feito; no crédito a caminho, achou o `begin` sem
resposta, perguntou ao Stripe, achou o crédito e não reenviou. Sem os
contratos, os dois são reenviados às cegas e o Stripe aplica de novo.

**Limites.** Um serviço só, cinco pontos de queda, e o modelo é falso
(latência fixa); a W2 com o Gemini está acima. O resultado mostra que o
mecanismo funciona contra um serviço que a Calyx não controla, não que todo
serviço respeita chaves.

### O probe: testar a chave antes de confiar nela

A garantia do reembolso depende de o servidor MCP repassar a chave ao
Stripe. `calyx check --tools --probe` testa isso no ambiente de teste: chama
a escrita duas vezes com a mesma chave e conta os efeitos com uma tool de
leitura (`bench/stripe/probe.sh`, `bench/results/stripe_probe.txt`):

| Servidor | Reembolsos para 2 chamadas com a mesma chave | `calyx check --probe` |
|---|---|---|
| `stripe_server.py` como é | 1 | passa |
| o mesmo com `STRIPE_DROP_KEY=1` (não repassa a chave, mas anuncia `idempotencyKeyHint: true`) | 2 | `E0704` |

## W8: várias máquinas (o diário no PostgreSQL)

Com o diário no PostgreSQL, uma execução pode ser assumida por outra máquina
(spec, seção 9.3). A W8 (`bench/run_w8.py`, resultados em
`bench/results/w8.json`) mede quanto isso custa, contra o PostgreSQL 16 na
mesma máquina e "a alguns milissegundos": um proxy (`w8_machines/delay_proxy.py`)
atrasa cada byte nos dois sentidos, dando 2,8 e 7,0 ms de ida e volta.
Mediana de 5 rodadas. A medição achou três problemas na v0.3.5, corrigidos
antes dos números abaixo; a v0.3.5 aparece para comparar.

**O custo do diário.** Fan-out da W1 com 500 perguntas e modelos que
respondem na hora (1.003 linhas de 8 threads), e um agente de 100 voltas
(uma thread, uma chamada de modelo e uma de tool por volta):

| Diário | Fan-out local | 2,8 ms | 7,0 ms | Agente local | 2,8 ms | 7,0 ms |
|---|---|---|---|---|---|---|
| Arquivo | 0,12 s | — | — | 0,25 s | — | — |
| PostgreSQL, um commit por linha (v0.3.5) | 0,85 s | 6,9 s | 15,8 s | 0,47 s | 1,8 s | 3,5 s |
| PostgreSQL, *group commit* | **0,15 s** | **0,24 s** | **0,37 s** | **0,25 s** | **0,36 s** | **0,43 s** |

A v0.3.5 confirmava cada linha numa transação própria, sob o lock do
diário: o fan-out ficava preso a uma ida e volta por linha. Agora uma thread
com conexão própria confirma de uma vez as linhas que chegaram enquanto
isso, e o runtime espera por ela antes de toda escrita externa e no fim: a
mesma regra do arquivo (`fsync` no máximo uma vez por segundo, sempre antes
de uma escrita).

**Assumir uma execução.** A máquina A morre no meio do reembolso da W2
(modelos de 1 s); um `calyx worker --every 1` na máquina B a termina. Do
momento da morte até a execução terminar em B, sempre com um pagamento e um
e-mail:

| Como A morre | v0.3.5 | Agora |
|---|---|---|
| O processo é morto (o sistema fecha a conexão) | 2,1 s | 1,3 s |
| A máquina some (todo pacote da conexão é descartado, com `iptables`) | **não assumiu em 60 s** | **11,5 s** |

Quando a máquina some, nada avisa o servidor: ele só descobre pelo
*keepalive* do TCP, que por padrão começa depois de duas horas, e até lá a
sessão de A segura o lock da execução. Agora cada conexão ajusta o da
própria sessão (5 s ociosa, 3 sondas a cada 2 s, `tcp_user_timeout` de
11 s).

**Entidades disputadas.** P processos, metade em cada máquina, mandam 50
mensagens cada à mesma entidade. Em todos os casos a contagem final é
exata (nenhuma mensagem perdida):

| Onde | 1 processo | 2 | 4 | 8 |
|---|---|---|---|---|
| Arquivos (uma máquina, `flock`) | 743/s | 878/s | 884/s | 868/s |
| PostgreSQL local | 624/s | 736/s | 754/s | 639/s |
| PostgreSQL a 7,0 ms | 47/s | 93/s | 108/s | 116/s |

Na v0.3.5 uma mensagem custava umas 9 idas e voltas (BEGIN, e um *prepare*
e um *execute* por comando), com a linha travada na maior parte delas: 41
mensagens/s a 7,0 ms. Agora são duas, com a linha travada durante uma.

**Listar as execuções.** `calyx runs` (e cada passada do `calyx worker`)
com as 531 execuções e 156 mil linhas que a W8 deixa no banco: 0,90 s na
v0.3.5, que lia o diário inteiro de cada uma; 0,13 s agora, com o servidor
contando.

**Limites.** A latência vem de um proxy local, não de uma rede de verdade
(sem perda de pacotes nem variação), e o servidor está na mesma máquina que
os clientes. Os modelos são falsos.

## E5: contratos e concorrência, ganho e custo

Quanto os contratos obrigatórios e a análise estática reduzem erros de
concorrência, e quanto custam em flexibilidade e desempenho, está em
[`contratos-e-concorrencia.md`](contratos-e-concorrencia.md): 22 dos 25 bugs
de concorrência do corpus impedidos (14 antes de rodar), 16 de 21 padrões
corretos aceitos como escritos, e os custos concentrados em três lugares
(`after` que põe escritas em série, o quórum que não existe, as chamadas a um
mesmo servidor MCP em série).

## O que a comparação mostra e o que não mostra

**Mostra:**

1. **Recuperação com efeitos externos:** entre passos, Temporal e LangGraph
   `sync` acertam como a Calyx. Com o efeito em andamento, nenhum registro
   por passo resolve, nem o histórico do Temporal: é preciso um contrato do
   efeito. Os baselines chegam a 6 de 6 com cuidado manual; a Calyx chega lá
   **por padrão**, porque o contrato é obrigatório. A tese defensável é
   "o compilador exige o contrato", não "a Calyx recupera melhor".
2. **Bugs de efeito, ordem e concorrência:** checadores de tipo não os veem,
   e o LangGraph, quando os vê, é ao rodar, às vezes depois do dano.
3. **Paralelismo:** sai sozinho e tão bem quanto à mão. Não é uma vantagem de
   velocidade.

**Não mostra:**

- **Resultados com modelos reais em escala.** W1 e W2 foram repetidos com o
  Gemini (seção acima), mas com N pequeno, por causa do limite da chave de
  teste, e com um modelo que deu sempre a mesma resposta no W2.
- **Versões escritas por outras pessoas.** O mesmo autor escreveu todas as
  versões e o corpus de bugs.
- **Tempo de retomada justo contra o Temporal** (com *heartbeats* e timeouts
  ajustados).
- **Outras workloads com efeitos** (W3, W5, W7) na matriz de quedas.

O [plano de avaliação](plano-paper.md) diz como cobrir cada um desses pontos.
