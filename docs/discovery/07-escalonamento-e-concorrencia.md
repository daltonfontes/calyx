# Escalonamento e controle de concorrência

Quatro trabalhos que tocam duas perguntas que a Calyx ainda não tinha respondido:

1. **Escalonamento:** quando há mais nós prontos do que threads disponíveis (limite da D3), **qual roda primeiro**?
2. **Controle de concorrência:** quando agentes paralelos mexem no mesmo estado, **que garantias** a Calyx dá, e onde elas acabam?

| Trabalho | Tipo | Base da leitura |
|---|---|---|
| Yang et al., *Position: Multi-Agent Systems Should Prioritize Concurrency Control* (ICML 2026) | Position paper | PDF completo |
| Zhang et al., *Plan-over-Graph: Towards Parallelable LLM Agent Schedule* (2025) | Paper + código | Código-fonte e resumo (arXiv bloqueado) |
| Ahmad, Kwok & Wu, *Analysis, Evaluation, and Comparison of Algorithms for Scheduling Task Graphs on Parallel Processors* (ISPAN 1996) | Survey clássico | Resumo + conhecimento prévio |
| da Silva & Gabriel, *A Comprehensive Review of Evolutionary Algorithms for Multiprocessor DAG Scheduling* (Computation, 2020) | Survey | Resumo + conhecimento prévio |

Um quinto trabalho, *State Consistency in Concurrent LLM Agent Workflows* (SSRN), não pôde ser lido: o domínio está bloqueado no ambiente, o link era temporário e as buscas pelo título não o encontraram.

---

## 1. Position: Multi-Agent Systems Should Prioritize Concurrency Control (ICML 2026)

1. **Problema:** sistemas com vários agentes ficam **menos** confiáveis quando se adicionam agentes (taxas de falha de 41% a 86,7% em benchmarks populares, segundo os trabalhos citados). Os autores defendem que boa parte dessas falhas, atribuídas a "coordenação" ou "comunicação", são na verdade **anomalias clássicas de concorrência**.
2. **Agente:** um estado local (janela de contexto, raciocínio), que só ele altera, e ações sobre um **ambiente compartilhado** (arquivos, memórias, caixas de mensagem, mundo de um jogo). Ações são **leituras** (sem efeito no ambiente) ou **escritas** (com efeito).
3. **Workflow:** não propõem um. Analisam sistemas existentes (MAGIS, CAID, CodeR, SagaLLM, MegaAgent) e mostram que cada um reinventa, sem dar o nome, um mecanismo de concorrência: isolamento otimista, escalonamento por dependência, recuperação transacional.
4. **Grafo:** não é o foco.
5. **Execução:** o problema central é uma **assimetria de tempo**: a inferência do LLM leva de segundos a minutos, enquanto uma tool leva milissegundos. Enquanto um agente "pensa", outros mudam o ambiente várias vezes, e a decisão dele se baseia num estado velho.
6. **Estado:** as quatro anomalias que eles mapeiam:
   - **Leitura velha (*stale read*):** o agente A lê `utils.py` e começa a pensar; o agente B renomeia uma função; A escreve código que importa o nome antigo.
   - **Atualização perdida (*lost update*):** A e B leem o mesmo arquivo, cada um muda uma parte, e a última escrita apaga a outra.
   - **Correção atrasada (*stale correction*):** B corrige um plano, mas C já começou a executar o plano antigo.
   - **Dessincronia entre ação e mensagem:** uma mensagem descreve um estado do mundo que outro agente já mudou.
7. **Concorrência:** propõem trazer as técnicas de bancos de dados: níveis de isolamento, travas (*locks*), controle otimista com validação, múltiplas versões (MVCC), granularidade de transação. E mostram por que cada uma precisa ser **adaptada**: uma trava segurada durante minutos de inferência bloqueia todo mundo; uma validação otimista que falha joga fora minutos de inferência.
8. **Sem solução:** como definir "versões" para ambientes arbitrários (não só bancos de dados); onde ficam as fronteiras das transações (explícitas, com o LLM tendo que entender `BEGIN`/`COMMIT`, ou implícitas, inferidas pelo sistema); como fazer rollback quando há efeitos irreversíveis (enviar e-mail); como interromper uma inferência no meio. A chamada final é explícita: **pesquisadores de linguagens de programação e de bancos de dados deveriam adaptar essas técnicas para agentes**.

**Para a Calyx:** é a defesa mais direta da hipótese até agora, vinda de fora. Cada recomendação deles tem um correspondente já decidido ou proposto na Calyx (ver a seção "O que muda na Calyx" abaixo).

## 2. Plan-over-Graph: Towards Parallelable LLM Agent Schedule (2025)

1. **Problema:** LLMs planejam bem tarefas sequenciais, mas mal **planos paralelos**.
2. **Agente:** um LLM planejador + um escalonador + executores.
3. **Workflow:** o LLM recebe a tarefa em texto, extrai um **grafo de tarefas abstrato** (regras do tipo "de A e B produz-se C, em tempo t, a custo c") e devolve um **plano em JSON**: subtarefas com dependências.
4. **Grafo:** **dinâmico, gerado pelo LLM** antes da execução (como o W7 do teste no papel). O grafo de regras tem **alternativas**: o mesmo alvo pode ser produzido por caminhos diferentes, e o LLM precisa escolher o mais rápido. É um grafo E/OU, não só de dependências.
5. **Execução** (no código):
   - O plano é **validado**: cada subtarefa precisa corresponder a uma regra existente e a **ordenação topológica** rejeita planos com ciclo. Se o plano for inválido, o LLM tenta de novo.
   - O escalonador roda **em paralelo, como processos separados**, todas as subtarefas cujas dependências terminaram, e libera as seguintes à medida que cada uma acaba (contagem de dependências).
6. **Estado:** um ambiente com os "materiais" disponíveis; cada subtarefa concluída é registrada (`commit`), conferindo se as entradas estavam disponíveis.
7. **Concorrência:** derivada das dependências do plano, sem limite de processos e sem prioridade entre tarefas prontas.
8. **Sem solução:** a contribuição principal é **treinar o LLM** (pipeline de grafos sintéticos + treinamento em duas etapas) para escolher bons planos paralelos, o que mostra que **LLMs não fazem isso bem sozinhos**. O escalonamento em si é ingênuo (sem limite de recursos, sem prioridade).

**Para a Calyx:** confirma a divisão de trabalho da D3, agora também para grafos gerados: **o LLM decide *o quê* (o grafo); o runtime decide *quando* (o escalonamento)**. Pedir ao LLM que otimize o paralelismo é gastar inferência num problema que tem algoritmo determinístico, barato e melhor.

## 3. Analysis, Evaluation, and Comparison of Algorithms for Scheduling Task Graphs on Parallel Processors (1996)

1. **Problema:** dado um programa paralelo como um **grafo acíclico com pesos** (tempo de cada tarefa, custo de comunicação de cada aresta), alocar as tarefas a processadores minimizando o **tempo total** (*makespan*). O problema é NP-difícil no caso geral.
2. **Agente:** não se aplica.
3. **Workflow:** o grafo de tarefas (*task graph* ou *macro-dataflow graph*).
4. **Grafo:** **estático**, conhecido antes, com tempos conhecidos.
5. **Execução:** analisam 21 algoritmos, em quatro famílias:
   - **BNP** (número limitado de processores): escalonam diretamente em P processadores;
   - **UNC** (número ilimitado de grupos): agrupam tarefas e depois mapeiam;
   - **TDB** (com duplicação de tarefas): repetem uma tarefa em vários processadores para economizar comunicação;
   - **APN** (redes arbitrárias de processadores): consideram a topologia da rede.
6. **Estado:** não se aplica.
7. **Concorrência:** a ideia que atravessa quase todos os algoritmos é o **escalonamento por lista** (*list scheduling*): mantém-se uma lista de tarefas prontas, ordenada por uma **prioridade**, e cada processador livre pega a de maior prioridade. As prioridades mais usadas são baseadas no **caminho crítico**: o *b-level* de uma tarefa é o caminho mais longo dela até o fim do grafo; tarefas no caminho crítico vão primeiro.
8. **Sem solução:** todos supõem **tempos conhecidos** e grafo estático; nenhum trata incerteza de duração.

**Para a Calyx:** o limite de threads da D3 transforma o runtime num escalonador **BNP**. Quando há mais nós prontos que threads, a resposta clássica e barata é **lista com prioridade por caminho crítico**.

## 4. A Comprehensive Review of Evolutionary Algorithms for Multiprocessor DAG Scheduling (2020)

1. **Problema:** o mesmo da seção 3, atacado com **metaheurísticas evolutivas** (algoritmos genéticos e afins).
2. **Agente:** não se aplica.
3. **Workflow:** grafo acíclico de tarefas.
4. **Grafo:** estático.
5. **Execução:** a busca evolutiva codifica um escalonamento como um "cromossomo" (ordem e alocação das tarefas) e evolui uma população, avaliando o *makespan* e, em alguns trabalhos, custo, balanceamento de carga e confiabilidade.
6. **Estado:** não se aplica.
7. **Concorrência:** otimização **offline**, antes da execução.
8. **Sem solução:** custo computacional alto da própria busca; dependência de tempos conhecidos.

**Para a Calyx:** relevância baixa no runtime. Os grafos de agentes são pequenos (dezenas a milhares de nós), as durações são incertas e as decisões precisam ser tomadas na hora. Uma busca evolutiva custaria mais do que economiza. Pode ter uso **offline**, para planejar um lote grande e repetitivo (como o W5), mas não como mecanismo central.

---

## O que muda na Calyx

### 1. Escalonamento: uma decisão nova (D24)

O limite de threads (D3) cria um problema que não tínhamos tratado: **qual nó pronto roda primeiro?**

**Proposta:** escalonamento por lista, com **prioridade pelo caminho crítico**, onde a "duração" de cada nó vem de:

- **estimativas do compilador** quando não há histórico (o máximo de tokens de um nó `llm` dá uma ideia da duração);
- **o histórico de execuções anteriores** (o trace, D8) quando ele existe.

Isso liga observabilidade e concorrência: **o trace de ontem melhora o escalonamento de hoje**, sem o programador fazer nada.

Também precisa ser considerado um segundo objetivo além do tempo: o **custo** (orçamento da D3). Com orçamento apertado, pode ser melhor rodar primeiro o que tem mais chance de tornar outros nós desnecessários.

**O que fica fora:** escalonamento ótimo (NP-difícil) e metaheurísticas no runtime. LLM escolhendo escalonamento (Plan-over-Graph) também fica fora: o LLM produz o grafo; o runtime escalona.

### 2. A Calyx, lida pela lente do controle de concorrência

O position paper dá um vocabulário preciso para dizer **o que a Calyx garante**:

| Recomendação do paper | Na Calyx |
|---|---|
| Separar estado local do agente e ambiente compartilhado | `conversation` é um valor de cada ramo (D3); estado nomeado e ambiente têm regras próprias (D1, D13) |
| Isolamento entre agentes | **Snapshot na bifurcação** (D1): é, literalmente, *snapshot isolation* |
| Evitar atualização perdida | **Redutor obrigatório** na junção (D1, D7): duas escritas no mesmo estado nunca se sobrescrevem em silêncio |
| Fronteiras de transação: explícitas (LLM entende `BEGIN`/`COMMIT`) ou implícitas | **Implícitas, mas pela estrutura do grafo**: a bifurcação abre, a junção fecha. Nem o LLM precisa entender transações, nem o sistema precisa adivinhar fronteiras |
| Efeitos irreversíveis limitam o rollback | `write once` com política obrigatória (D2); compensação (D12) |
| Escalonamento por dependência | Concorrência derivada do grafo (D3) |
| Feedback semântico de conflito para o agente | Conflito como valor (D11) que entra na `conversation` como observação, como os erros de tool no ReAct |
| Infraestrutura com versões | Diário (D6) + snapshot da sandbox (D13) |

### 3. O limite honesto: *snapshot isolation* não é *serializable*

Bancos de dados conhecem bem a fraqueza do *snapshot isolation*: a **anomalia de escrita enviesada** (*write skew*).

Exemplo na Calyx: dois ramos leem o mesmo estado (`orçamento restante = 100`). Cada um decide gastar 70, achando que cabe, e escreve o gasto. O redutor soma as escritas corretamente (nenhuma se perde), mas o resultado (140) viola uma regra que nenhum dos dois ramos violou sozinho.

O redutor resolve **atualizações perdidas**, mas não **invariantes entre ramos**. Opções:

- **Validação na junção:** o programador declara invariantes (`gasto ≤ orçamento`), verificados depois do redutor; se violados, a junção produz um valor `conflito` (D11) que o grafo trata.
- **Recursos com dono** (D15): um recurso crítico, como o orçamento, pertence a um único nó ou execução, e os outros pedem a ele.

A hipótese precisa dizer isso: **dentro de uma execução, a Calyx garante isolamento por snapshot e ausência de atualizações perdidas; invariantes entre ramos exigem validação declarada.**

### 4. A sandbox segue as mesmas regras do estado (D13 refinada)

O exemplo do paper (dois agentes de código no mesmo repositório) é o W2 do teste no papel com dois agentes. A solução que o paper cita como mais eficaz (CAID: um *worktree* isolado por agente, validação na junção) é exatamente a D1 aplicada ao ambiente:

- **Bifurcação:** cada ramo recebe a **sua cópia da sandbox** (cópia barata, como um *worktree* do Git ou um snapshot de sistema de arquivos).
- **Junção:** as sandboxes dos ramos se juntam por um **redutor declarado** (ex.: *merge* do Git), seguido de **validação** (ex.: rodar os testes).
- **Conflito:** um *merge* que falha ou testes que quebram produzem um valor `conflito` (D11), que pode voltar para um agente como observação, com detalhes do que conflitou (o "feedback semântico" do paper).

Assim, **estado em memória e ambiente seguem o mesmo modelo**: snapshot na bifurcação, redutor e validação na junção. Nenhum conceito novo.

**Contraponto (STORM):** a busca pelo paper do SSRN trouxe um trabalho que desafia esta proposta. O *STORM* ([*Multi-agent Collaboration with State Management*](https://arxiv.org/abs/2605.20563), 2026; lido só pelo resumo) **não** dá uma cópia isolada para cada agente. Ele intermedia cada leitura e escrita de arquivo e aceita uma escrita apenas se **os arquivos que aquele agente leu** não mudaram desde a leitura (*consistência local de estado*). Se mudaram, a escrita é recusada e o agente recebe o conteúdo novo para tentar de novo. O resultado reportado: **+18,7 pontos sobre a abordagem de um *worktree* do Git por agente** no benchmark Commit0-Lite.

A ideia é controle otimista com **conjunto de leitura** (*read set*) por escrita, em vez de isolamento por ramo com *merge* no fim. Os conflitos aparecem cedo, enquanto o agente ainda está trabalhando, e não só na junção.

**Para a Calyx, isso é viável sem anotação:** o diário já registra cada chamada `read` de cada nó, então o runtime **conhece o conjunto de leitura** de cada ramo automaticamente. As duas estratégias ficam em aberto para a D13:
- **cópia por ramo + merge na junção** (simples, conflitos tardios);
- **validação por conjunto de leitura a cada escrita** (conflitos cedo; resultado melhor no STORM).

### 5. Assimetria de tempo: por que snapshot e não travas

O paper explica por que travas são ruins para agentes: uma trava segurada durante minutos de inferência bloqueia todos os outros. O modelo da Calyx (snapshot + junção) é **otimista**: ninguém espera ninguém durante a inferência. O custo é que, num conflito, o trabalho de um ramo pode ser perdido. A mitigação é estrutural: **conflitos só são possíveis onde o grafo junta ramos que escreveram no mesmo lugar**, e o compilador sabe exatamente onde isso acontece, então pode **avisar** nos pontos de risco.

---

## Decisões novas ou revisadas

| # | Decisão | Proposta |
|---|---|---|
| D24 | Escalonamento | Lista com prioridade pelo caminho crítico; durações estimadas pelo compilador e refinadas pelo histórico de traces; orçamento como segundo objetivo |
| D25 | Invariantes entre ramos | Validação declarada na junção; violação produz valor `conflito` (D11) |
| D13 (revisada) | Sandbox | Em aberto entre: cópia por ramo + redutor (merge) e validação na junção; ou validação por conjunto de leitura a cada escrita (STORM), com o conjunto de leitura extraído do diário |
| Hipótese (revisada) | Garantia de isolamento | Dentro de uma execução: *snapshot isolation* sem atualizações perdidas; invariantes entre ramos exigem validação declarada |
