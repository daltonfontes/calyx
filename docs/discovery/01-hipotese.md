# Hipótese central

## Princípio do projeto

**A Calyx precisa rodar rápido e compilar rápido.** Toda análise do compilador é linear ou composicional (sem enumerar caminhos, sem provador de teoremas); no runtime, a prioridade é fazer menos chamadas de LLM e rodar em paralelo o que é independente. Ver [arquitetura do runtime](12-arquitetura-runtime.md).

## Pergunta

> Uma linguagem graph-native para agentes pode permitir que o runtime derive automaticamente **concorrência**, **dependências de estado**, **recuperação de falhas** e **observabilidade** a partir da estrutura do grafo?

## Hipótese

> Uma linguagem graph-native para agentes, em que cada nó declara explicitamente **o estado que lê e escreve**, **a regra de junção desse estado** e **o tipo de efeito que produz**, permite que o runtime derive automaticamente concorrência, dependências de estado, recuperação de falhas e observabilidade, sem que o programador as escreva, **desde que toda mudança dinâmica no grafo preserve essas declarações**.

A pergunta de fundo não é "dá para usar um grafo?". É: **quais restrições a linguagem precisa impor para que o grafo contenha toda a informação de que o runtime precisa?**

## Por que acreditamos nisso

Fora da IA, sistemas que expõem um grafo de dependências explícito já derivam essas propriedades:

| Propriedade | Onde já é derivada do grafo |
|---|---|
| Concorrência | Linguagens de dataflow, TensorFlow, Bazel, Spark |
| Dependências | Build systems: só recalculam o que mudou |
| Recuperação de falhas | Spark (lineage), Temporal (execução durável) |
| Observabilidade | Cada nó é um ponto natural de medição e de trace |

Nos trabalhos de agentes que lemos (ver [02-papers.md](02-papers.md)), essas propriedades ou não existem ou são escritas à mão:

- **ReAct / AutoGen:** o fluxo de controle fica implícito no texto ou na conversa. Não há concorrência, e o estado é o próprio prompt ou histórico.
- **DSPy:** o grafo só existe quando o Python executa (*define-by-run*), então não dá para analisá-lo antes.
- **AgentSPEX:** o paralelismo é manual (`parallel`, `gather`). O runtime copia o contexto inteiro para cada ramo e **não junta o estado de volta**. No código, o `parallel` com lista de passos roda **sequencialmente** ("sequential execution for now"). O checkpoint funciona por "id de passo concluído", o que pressupõe execução sequencial.
- **GraphFlow:** é a evidência positiva. Como o grafo é conhecido antes da execução, o runtime compartilha KV cache entre nós e requisições e reduz a memória em ~4×.

### Sobre a novidade

O **LangGraph** já deriva parte disso: é inspirado no modelo Pregel, roda em paralelo os nós do mesmo passo, junta estado com *reducers* e faz checkpoint por passo. Mas é uma biblioteca Python: as dependências de estado **não são verificadas**, e o grafo pode ser alterado por código arbitrário.

A contribuição da Calyx não é "usar grafo". É **garantir estaticamente**, via linguagem e compilador, o que as bibliotecas só permitem por convenção.

## O que a linguagem precisa exigir

Para cada propriedade: o que a linguagem exige, o que o runtime ganha e onde a abordagem quebra.

### 1. Concorrência

- **Exige:** cada nó declara o que **lê** e o que **escreve**.
- **O runtime deriva:** dois nós sem caminho de dependência entre eles rodam em paralelo. O programador não escreve `parallel` nem `thread`: **escreve o grafo, e o runtime extrai as threads**. O programador controla apenas limites (threads simultâneas, requisições por segundo, custo) e a ordem entre nós com efeito de escrita.
- **Quebra quando:** há estado mutável compartilhado (o `context` global do AgentSPEX, um histórico de conversa único).
- **Resposta:** estado imutável ou versionado, com **regra de junção declarada** quando dois ramos escrevem no mesmo lugar.

### 2. Dependências de estado

- **Exige:** nenhum efeito escondido. Um nó não lê globais nem depende de "o que aconteceu antes" sem declarar isso.
- **O runtime deriva:** invalidação, cache e recálculo seletivo.
- **Quebra quando:** o histórico de conversa é tratado como ambiente global.
- **Resposta:** a conversa é um valor do tipo `conversation` que **flui pelas arestas** (ver D3 em [03-decisoes.md](03-decisoes.md)).

### 3. Recuperação de falhas

Nós de agentes não são funções puras. O runtime precisa saber o **tipo de efeito** de cada nó:

| Tipo de efeito | Exemplo | Recuperação derivável |
|---|---|---|
| Puro (`pure`) | Transformar texto, parsear JSON | Reexecutar livremente |
| Não-determinístico (`llm`, `read`) | Chamada ao LLM, busca na web | Não reexecutar: gravar o resultado e reaproveitar do trace |
| Efeito externo idempotente (`write`) | Pagamento com chave, sobrescrever arquivo | Reexecutar com segurança |
| Efeito externo não idempotente (`write once`) | Enviar e-mail, postar mensagem | Nunca reexecutar; política declarada para falha durante a chamada |

- **Exige:** o tipo de efeito faz parte da declaração do nó e é **verificado pelo compilador** (um nó `pure` não pode chamar uma tool com efeito externo).
- **O runtime deriva:** ao falhar, retoma do último estado consistente e refaz **só o subgrafo afetado**, inclusive com ramos paralelos.

### 4. Observabilidade

- **Exige:** praticamente só o grafo explícito. É a propriedade mais "grátis" das quatro.
- **O runtime deriva:** trace por nó com a tupla *(estado, ação, observação, custo)* do survey de ACG; replay; visualização do grafo realizado; custo por nó e por caminho.
- **Ganho extra:** ataca o problema em aberto que o survey chama de **atribuição de crédito estrutural**, ou seja, saber quanto cada nó e cada aresta contribuíram para o resultado.

## Garantia de isolamento

Pela lente do controle de concorrência (ver [07-escalonamento-e-concorrencia.md](07-escalonamento-e-concorrencia.md)): **dentro de uma execução, a Calyx garante isolamento por snapshot e ausência de atualizações perdidas.** Invariantes que envolvem vários ramos (ex.: "o gasto total não passa do orçamento") **não** são garantidos automaticamente e exigem validação declarada na junção. Entre execuções, a garantia vem de recursos com dono (D15).

### Princípio de concorrência

**Nunca segurar trava durante a inferência; validar no momento do efeito.** Medido no SVBE (ver [10-svbe.md](10-svbe.md)): travas seguradas durante o raciocínio levam o P95 de 5 s para 29 s, enquanto a validação no momento do efeito fica praticamente no tempo do próprio raciocínio. A Calyx aplica o princípio em três níveis: estado da execução (snapshot + junção), sandbox (conjunto de leitura) e sistemas externos (precondições semânticas).

## O limite: grafos dinâmicos

Tudo acima é fácil com grafo estático. Agentes, porém, precisam de dinamismo. É a tensão que o survey de ACG chama de **expressividade × verificabilidade**.

Proposta de dinamismo controlado:

| Forma de dinamismo | Condição para ser permitida |
|---|---|
| Laços e condicionais | São estruturas do grafo, com **limite obrigatório** de iterações |
| Fan-out dinâmico ("um nó por item") | O **tipo** do nó criado é conhecido: o runtime não sabe *quantos* nós haverá, mas sabe o que leem, escrevem e fazem de efeito |
| Grafo gerado por LLM (plan generator do AgentSPEX, GraphFlow) | O grafo gerado passa pelo **mesmo verificador do compilador** antes de executar |

Regra geral: **o grafo pode mudar em tempo de execução, mas só de formas que preservem as garantias estáticas.**

## Perguntas de pesquisa (mensuráveis)

| # | Pergunta | Como medir | Comparar com |
|---|---|---|---|
| Q1 | Quanto paralelismo o runtime extrai sozinho? | Latência de ponta a ponta; linhas de código de orquestração | `parallel` manual do AgentSPEX; LangGraph |
| Q2 | Quantos bugs de estado o compilador pega antes de rodar? | Suíte de workflows com bugs conhecidos (ramos que se sobrescrevem, estado perdido, efeito em nó puro) | Mesmos workflows em AgentSPEX e LangGraph |
| Q3 | Quanto trabalho é refeito após uma falha? | Tokens e chamadas de tool refeitos após falha injetada em ponto aleatório | Recomeçar do zero; checkpoint por passo |
| Q4 | O trace derivado explica o custo sem instrumentação manual? | Cobertura do trace; atribuição de custo por nó | Logs do AgentSPEX; LangSmith/LangGraph |

## Fora de escopo nesta fase

- Sintaxe concreta da linguagem.
- Escolha entre C# e C para o compilador e o runtime.
- Otimização de prompts no estilo DSPy e geração de grafo por GNN no estilo GraphFlow. São extensões possíveis, que dependem de a hipótese central se sustentar primeiro.
