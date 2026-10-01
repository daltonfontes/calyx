# Concorrência: três problemas diferentes

Concorrência não é um problema só. Para a Calyx, são três, com implicações bem distintas:

```text
CONCORRÊNCIA
│
├── 1. Como descobrir o que pode executar em paralelo?
│      → grafo de tarefas, escalonador
│
├── 2. Como executar em paralelo?
│      → roubo de trabalho, fork-join, atores, runtime
│
└── 3. Como lidar com estado compartilhado?
       → condições de corrida, transações, consistência
```

E a pergunta de fundo deixa de ser *"como faço multithreading na minha linguagem?"* e passa a ser *"qual modelo de concorrência a minha linguagem oferece?"*:

```text
                  PROGRAMA
                     │
                     ▼
                   GRAFO
                     │
          ┌──────────┴──────────┐
          │                     │
     dependências             estado
          │                     │
          ▼                     ▼
     escalonador        modelo de consistência
          │                     │
          └──────────┬──────────┘
                     ▼
                  RUNTIME
                     │
          ┌──────────┼──────────┐
          ▼          ▼          ▼
       worker     worker     worker
```

A thread vira **detalhe de implementação do runtime**, não uma abstração que o programador controla. É o que a D3 já decidiu ("você escreve o grafo, o runtime extrai as threads"); este documento organiza o resto em torno dessa ideia.

---

## Base da leitura

| Trabalho | Base |
|---|---|
| Taelin, *HVM2: A Parallel Evaluator for Interaction Combinators* | **Texto completo** ([GitHub](https://github.com/HigherOrderCO/HVM2/blob/main/paper/HVM2.typst)) |
| *Task Graph Scheduling* (verbete, *Encyclopedia of Parallel Computing*) | Conhecimento prévio (Springer bloqueado) |
| Kwok & Ahmad, *Benchmarking and Comparison of the Task Graph Scheduling Algorithms* (JPDC, 1999) | Resumo + conhecimento prévio |
| Cosnard & Jeannot, *Compact DAG Representation and Its Dynamic Scheduling* (JPDC, 1999) | Resumo |
| Bak et al., *Task-Graph Scheduling Extensions for Efficient Synchronization and Communication* (ICS 2021) | Resumo |
| De Koster et al., *Domains: Safe Sharing Among Actors* (SCP, 2015) e *Domains: Sharing State in the Communicating Event-Loop Actor Model* (2016) | Resumo + conhecimento prévio |
| Malewicz, *Scheduling Dags under Uncertainty* (SPAA 2005) | Resumo |
| Kayaaslan et al., *Scheduling Series-Parallel Task Graphs to Minimize Peak Memory* (TCS, 2018) | Resumo |
| *Directed Acyclic Task Graph Scheduling for Heterogeneous Computing Systems* (2009) e HEFT | Resumo + conhecimento prévio |
| Yu, Ding & Sato, *DynTaskMAS* (ICAPS 2025) | Resumo |
| Roubo de trabalho (Blumofe & Leiserson) e fork-join | Conhecimento prévio |

Todos os PDFs, exceto o do HVM2, estão em domínios bloqueados no ambiente (ScienceDirect, Springer, ACM, arXiv, sites de universidades).

---

## Problema 1: descobrir o que pode executar em paralelo

### O problema formal

Um **grafo de tarefas** é um DAG: nós são tarefas (com peso = duração) e arestas são dependências (com peso = custo de comunicação). **Escalonar** é decidir, para cada tarefa, **onde** (qual processador) e **quando** (instante de início), respeitando as dependências. O objetivo clássico é minimizar o ***makespan***: o tempo total, do início da primeira tarefa ao fim da última.

Fatos que importam para a Calyx:

- **Minimizar o makespan é NP-completo**, mesmo sem custo de comunicação. Por isso existem dezenas de heurísticas.
- **Mas o escalonamento por lista já é bom o bastante.** Mantém-se uma lista de tarefas prontas ordenada por prioridade; cada processador livre pega a primeira. O resultado clássico de Graham garante que **qualquer** escalonamento por lista, sem comunicação, termina em no máximo **2 vezes o tempo ótimo**. A prioridade só melhora a partir daí.
- **A prioridade mais usada é o caminho crítico:** o *b-level* de uma tarefa é o caminho mais longo dela até o fim do grafo. Tarefas com *b-level* maior vão primeiro.
- **Kwok & Ahmad (1999)** compararam 15 algoritmos num mesmo conjunto de testes. O vencedor geral foi o **DCP** (*Dynamic Critical Path*); entre os que trabalham com número limitado de processadores (o caso da Calyx, com o limite de threads), **MCP** e **DLS** foram os melhores, **por causa da forma como atribuem prioridade**. A lição: com número limitado de processadores, o que mais pesa é a prioridade.

**Para a Calyx:** confirma a D24 (lista com prioridade pelo caminho crítico) e dá uma garantia formal: não precisamos de nada caro.

### Grafo conhecido antes, ou que muda durante a execução?

Para agentes, o grafo frequentemente cresce durante a execução: o LLM decide uma tool, isso cria um nó novo, que cria um caminho novo.

**Cosnard & Jeannot (1999)** tratam um caso muito próximo. Eles propõem o **grafo de tarefas parametrizado** (*PTG*): uma representação **simbólica e compacta** do DAG, independente do tamanho do problema, cujos parâmetros só são conhecidos em tempo de execução. Um escalonador dinâmico trabalha sobre o PTG **sem nunca montar o DAG inteiro**.

**Para a Calyx, essa é exatamente a relação entre o template e o grafo realizado (D8):**

- o **template** compilado é um PTG: `node findings[q in plan.questions]` é um nó simbólico; quantos nós ele vira só se sabe quando `plan` termina;
- o **grafo realizado** é desenrolado **aos poucos**, à medida que os valores chegam;
- o runtime **não precisa** montar o grafo realizado inteiro antes de começar. Isso importa para o W5 (1000 documentos) e para laços longos.

Os grafos gerados por LLM (W7) são o caso mais dinâmico: um subgrafo inteiro aparece no meio da execução, é verificado, e passa a ser desenrolado como os outros.

**DynTaskMAS (ICAPS 2025)** é o exemplo do lado dos agentes: um gerador de grafo de tarefas dinâmico (o LLM decompõe a tarefa) e um motor de execução assíncrona e paralela. Reportam **21–33% menos tempo** de execução, uso de recursos de 65% para 88% e escala quase linear até 16 agentes (3,47× com 4× mais agentes). Valida a direção da Calyx, mas como framework, não como linguagem.

### Processadores diferentes entre si

O **HEFT** (*Heterogeneous Earliest Finish Time*) escalona um DAG em processadores com velocidades diferentes: ordena as tarefas por prioridade e põe cada uma no processador que a **termina mais cedo**, considerando dependências e a velocidade de cada um.

Para a Calyx, os "processadores" são diferentes por natureza:

| "Processador" | Latência | Custo | Capacidade | Chance de falha |
|---|---|---|---|---|
| LLM grande remoto | alta | alto | limite de requisições | baixa |
| LLM pequeno local | baixa | ~zero | memória da máquina | maior |
| Tool / API HTTP | variável | variável | limite de requisições | variável |
| Servidor MCP | variável | variável | variável | variável |
| Humano | horas ou dias | alto | muito baixa | — |

**Para a Calyx:** o roteador de modelos (D30) é um escalonador heterogêneo. Uma política "do mais rápido que cabe no orçamento" é um HEFT com custo como segundo objetivo, e o histórico do diário dá as estimativas de duração e de falha que o HEFT precisa.

### Incerteza e falhas

**Malewicz (2005)** estuda DAGs executados por trabalhadores **não confiáveis**: o trabalhador *i* acerta a tarefa *j* com probabilidade *p(i,j)*, e o escalonador pode pôr vários trabalhadores na mesma tarefa, **de forma redundante**, para reduzir o tempo esperado. O problema é NP-difícil no caso geral, e polinomial quando a largura do DAG e o número de trabalhadores são pequenos.

**Para a Calyx:** um LLM é um trabalhador não confiável (pode devolver saída inválida, estourar o tempo, errar). Rodar **a mesma chamada em dois provedores e ficar com a primeira resposta válida** (*hedging*) é um `race` (D12) sobre a mesma tarefa. Fica como **política possível do roteador** (D30), não como comportamento padrão, porque dobra o custo.

### Memória

**Kayaaslan et al. (2018)** mostram como escalonar grafos **série-paralelos** minimizando o **pico de memória**, com algoritmos polinomiais (antes só conhecidos para árvores).

O ponto: **mais paralelismo não é necessariamente melhor.** Se tudo roda ao mesmo tempo, todos os resultados intermediários vivem ao mesmo tempo.

Na Calyx, esse risco é **menor do que parece**: o contexto de um LLM remoto vive no provedor, não na memória local, e conteúdos grandes saem do diário e ficam referenciados por hash (D20). Mas ele existe para cópias de sandbox (`fork`), saídas grandes de tools e modelos locais.

Uma observação útil: as construções da Calyx (sequência, fan-out com junção, laço) geram grafos **série-paralelos** por construção. É exatamente a classe em que minimizar o pico de memória tem algoritmo polinomial. Uma aresta de dados arbitrária pode quebrar essa propriedade, e o compilador consegue detectar quando isso acontece.

**Para a Calyx:** a memória entra como **mais um limite** (`limits { memory: 4 GB }`), e o escalonador, quando perto do limite, prefere **terminar um ramo antes de abrir outro**.

---

## Problema 2: executar em paralelo

### Roubo de trabalho e fork-join

No **fork-join**, uma tarefa se divide em subtarefas (*fork*) e espera todas terminarem (*join*). É exatamente o fan-out com junção da Calyx.

No **roubo de trabalho** (*work stealing*), cada worker tem a sua fila de tarefas prontas; quem fica sem trabalho **rouba** da fila de outro. Para fork-join, o roubo de trabalho tem garantia formal (Blumofe & Leiserson): tempo esperado de no máximo *trabalho total / P* mais um termo proporcional ao caminho crítico. Na prática:

- cada worker empilha e desempilha na **própria ponta** da fila (barato, sem disputa);
- quem rouba pega da **outra ponta**, onde estão as tarefas **mais antigas**, que tendem a ser as maiores.

**HVM2 faz exatamente isso na CPU:** cada thread tem a sua sacola de redexes; uma thread ociosa rouba da ponta oposta da sacola de uma vizinha, com uma única operação atômica. Os autores relatam ocupação total da CPU em todos os casos testados, com baixo custo.

**Para a Calyx (decisão nova D31):** o runtime tem **N workers** (threads do sistema), cada um com a sua fila de nós prontos, e **roubo de trabalho** entre eles, roubando da ponta mais antiga. Duas adaptações:

1. **Um worker nunca bloqueia esperando E/S.** Quando um nó começa uma chamada de LLM ou tool, ele vira um pedido pendente no laço de eventos, e o worker segue para o próximo nó pronto. Quando a resposta chega, o nó seguinte entra na fila. Isso evita o problema clássico de *oversubscription* (threads paradas esperando enquanto outras deveriam trabalhar).
2. **O roubo de trabalho decide *qual worker* executa; a prioridade da D24 decide *a ordem*.** Cada worker pega da própria fila pela prioridade (caminho crítico), e o limite de chamadas simultâneas (D3) controla quantos pedidos de E/S ficam abertos.

### Descobrir trabalho sem trava

O HVM2 representa a computação como **nós ligados por fios**, e cada interação é **local**: só toca os nós envolvidos. Quando duas threads podem tocar o mesmo fio, um **mapa de substituição atômico** resolve sem trava (*lock-free*). E a **confluência forte** garante que o trabalho total não depende da ordem das interações, o que dá liberdade total para paralelizar.

**Para a Calyx**, o equivalente direto:

- **descobrir trabalho:** cada nó tem um **contador de dependências pendentes**. Quando um nó termina, entrega o valor no "slot" de cada sucessor e decrementa o contador dele **atomicamente**; quem leva o contador a zero coloca o sucessor na própria fila. Sem trava, sem fila global;
- **confluência:** o resultado de uma execução **não depende da ordem** em que os nós rodaram. Na Calyx isso é garantido pelas junções em ordem fixa (D7), pelos efeitos gravados no diário e pelos snapshots (D1).

### Uma lição do HVM2 que vale como regra: não avaliar ramos que não serão usados

O HVM2 avalia **tudo** que pode avaliar (é "ultra-ansioso"), e os autores listam isso como limitação: se uma estrutura grande for alocada e só um ramo for lido, tudo é calculado assim mesmo.

Na Calyx, isso seria caro de verdade: cada nó avaliado à toa é uma **chamada de LLM paga**. Regra (**D32**): **nós de um ramo de `if`/`match` só rodam quando o ramo é escolhido**, e nós cujo resultado deixou de ser necessário (o perdedor de um `race`) são **cancelados** (D12). Nada de execução especulativa por padrão.

### Sincronização dentro de tarefas e impasses

**Bak et al. (2021)** tratam runtimes de grafos de tarefas em que as tarefas **sincronizam ou se comunicam por dentro** (ex.: uma tarefa que espera outra no meio). Combinam pela primeira vez **escalonamento em grupo** (*gang scheduling*: tarefas que precisam rodar juntas são escalonadas juntas) e **roubo de trabalho**, evitando **impasses** (*deadlock*) e *oversubscription*.

**Para a Calyx:** os nós não sincronizam por dentro; toda sincronização está nas arestas do grafo e nas barreiras de `rounds` (D18), que são visíveis ao escalonador. **Mas há um ponto de impasse possível:** entidades (D15) que fazem `ask` (pergunta síncrona) umas às outras. Se a entidade A pergunta a B, e B, para responder, pergunta a A, as duas esperam para sempre.

Regra (**D33**): o compilador monta o grafo de quem faz `ask` a quem, e **recusa ciclos de `ask`**. Mensagens assíncronas (`send`) podem formar ciclos, porque não esperam resposta.

---

## Problema 3: estado compartilhado

O problema 3 já tem a maior parte das decisões (D1, D7, D13, D25, D26, D29), resumidas pelo princípio *"nunca segurar trava durante a inferência; validar no momento do efeito"*. A leitura nova aqui é a dos **Domains**.

### Domains: compartilhar estado entre atores sem perder as garantias

O modelo de atores evita condições de corrida porque cada ator é dono do próprio estado e só se comunica por mensagens. O preço: **compartilhar estado mutável fica difícil**. Para muitos leitores lerem o mesmo dado, todos precisam mandar mensagens a um único dono, que vira gargalo.

**De Koster et al.** propõem **domínios**: um pedaço de estado compartilhado que os atores **pedem acesso de forma assíncrona** (como uma trava assíncrona), com **visões** de **leitura compartilhada** ou de **escrita exclusiva**. O modelo continua **livre de impasses** e sem condições de corrida de baixo nível.

**Para a Calyx**, isso refina duas decisões:

- **Dentro de uma execução**, os recursos afins com `reads` / `edits` (D26) **já são domínios com visões verificadas na compilação**: várias leituras ao mesmo tempo, uma escrita por vez, e o compilador garante, sem trava em tempo de execução.
- **Entre execuções**, a entidade (D15) hoje processa **uma mensagem por vez**, inclusive consultas. Com a ideia dos domínios, os handlers de uma entidade se dividem em **leitura** (`on Recall`, que pode rodar em paralelo com outras leituras) e **escrita** (`on Remember`, exclusiva). Muitos leitores deixam de esperar na fila do dono. Proposta: o compilador infere a visão de cada handler (se ele altera `state`, é escrita).

O resto do problema 3 segue como já decidido ou proposto: snapshot na bifurcação e redutor na junção (D1, D7), validação pelo conjunto de leitura na sandbox (D13), precondições semânticas no efeito (D29).

---

## O modelo de concorrência da Calyx, resumido

| Problema | O que o programador escreve | O que o compilador faz | O que o runtime faz |
|---|---|---|---|
| **1. Descobrir** | O grafo (nós, arestas, fan-out, laços), e limites | Monta o template (um grafo parametrizado); verifica limites, custo, contexto | Desenrola o grafo aos poucos; prioriza pelo caminho crítico (D24); escolhe o modelo por política (D30) |
| **2. Executar** | Nada | Gera o código dos nós como segmentos sem pilha | N workers com roubo de trabalho (D31); E/S nunca bloqueia worker; contadores atômicos de dependência; ramos não escolhidos não rodam (D32) |
| **3. Estado** | `reads` / `edits`, redutores, invariantes, `requires` | Verifica donos e visões (D26); recusa ciclos de `ask` (D33) | Snapshot e junção; validação no momento do efeito; entidades com visões de leitura e escrita (D15) |

---

## Decisões novas ou revisadas

| # | Decisão | Proposta |
|---|---|---|
| D8 (nota) | Template | O template compilado é um **grafo de tarefas parametrizado**: o grafo realizado é desenrolado aos poucos, sem ser montado inteiro antes |
| D15 (revisada) | Entidades | Handlers de leitura rodam em paralelo; handlers de escrita são exclusivos; a visão é inferida pelo compilador (Domains) |
| D24 (revisada) | Escalonamento | Lista com prioridade pelo caminho crítico (garantia de Graham: no máximo 2× o ótimo); memória como limite opcional, terminando ramos antes de abrir outros quando perto do limite |
| D30 (nota) | Roteamento | É um escalonador heterogêneo (estilo HEFT, com custo como segundo objetivo); *hedging* entre provedores como política opcional |
| **D31** | Execução | N workers com roubo de trabalho (roubando da ponta mais antiga); E/S nunca bloqueia um worker; contadores atômicos de dependência para descobrir nós prontos |
| **D32** | Avaliação de ramos | Nós de ramos não escolhidos nunca rodam; resultados que deixaram de ser necessários são cancelados; sem execução especulativa por padrão |
| **D33** | Impasse entre entidades | O compilador recusa ciclos de `ask` (pergunta síncrona); `send` pode formar ciclos |
