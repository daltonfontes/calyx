# Leitura dos papers

Cada trabalho foi lido a partir de 8 perguntas:

1. Qual problema o trabalho resolve?
2. Como representa um agente?
3. Como representa o workflow?
4. O grafo é estático ou dinâmico?
5. Como acontece a execução?
6. Como funciona o estado?
7. Como funciona a concorrência?
8. O que ainda ficou sem solução?

**Profundidade da leitura.** O acesso a arxiv.org estava bloqueado no ambiente usado.

| Trabalho | Base da leitura |
|---|---|
| AgentSPEX | Código-fonte do runtime + especificação da linguagem + resultados via busca |
| GraphFlow | PDF completo |
| DSPy | Parte do código-fonte (concorrência, estado) + resumos + conhecimento prévio |
| Survey de ACG | Repositório do survey + trechos encontrados em busca |
| ReAct, AutoGen | Conhecimento prévio |

Os três trabalhos mais próximos da Calyx são **AgentSPEX**, **DSPy** e o **survey de ACG**.

---

## 1. ReAct: Synergizing Reasoning and Acting in Language Models (Yao et al., 2022)

1. **Problema:** raciocínio puro (CoT) alucina porque não consulta o mundo; ação pura não planeja. O ReAct intercala os dois.
2. **Agente:** um LLM com um prompt few-shot no formato *Thought / Action / Observation*.
3. **Workflow:** não existe workflow explícito. O fluxo é o texto gerado pelo LLM.
4. **Grafo:** não há grafo. É uma cadeia linear decidida pelo LLM em tempo real: totalmente dinâmica e implícita.
5. **Execução:** laço em que o LLM gera um pensamento ou uma ação, o ambiente executa, a observação volta para o prompt, até `Finish[resposta]`.
6. **Estado:** o próprio contexto do prompt, com a trajetória crescendo.
7. **Concorrência:** nenhuma.
8. **Sem solução:** o contexto estoura em trajetórias longas; o agente entra em laços repetitivos; uma busca ruim descarrila o raciocínio; o controle fica escondido no LLM.

**Para a Calyx:** é o "átomo" que roda *dentro* de um nó. É também o contraexemplo do controle implícito.

## 2. AutoGen: Enabling Next-Gen LLM Applications via Multi-Agent Conversation (Wu et al., 2023)

1. **Problema:** construir aplicações com vários agentes (LLM, humano, tools) cooperando sem reescrever a orquestração.
2. **Agente:** `ConversableAgent`, uma entidade que envia e recebe mensagens e tem *reply functions* (LLM, código, tools, humano).
3. **Workflow:** a própria conversa (*conversation programming*).
4. **Grafo:** dinâmico e emergente. No `GroupChat`, um gerente escolhe quem fala a cada rodada.
5. **Execução:** `initiate_chat` e respostas automáticas até uma condição de término.
6. **Estado:** cada agente guarda o próprio histórico por interlocutor. Não há estado global estruturado.
7. **Concorrência:** basicamente sequencial (turnos). Existem variantes `async`, mas o modelo é de turnos.
8. **Sem solução:** o fluxo é difícil de prever e depurar; a topologia ideal é desconhecida; a execução de código é insegura; custo e laços dependem de limites manuais.

**Para a Calyx:** representa o sentido de "thread" como **conversa entre agentes**.

## 3. DSPy: Compiling Declarative Language Model Calls into Self-Improving Pipelines (Khattab et al., 2023)

1. **Problema:** pipelines de LLM feitos com prompts escritos à mão, que são frágeis e não se otimizam.
2. **Agente:** um programa de **módulos** (`Predict`, `ChainOfThought`, `ReAct`…) ligados a **signatures** tipadas (`"question -> answer"`).
3. **Workflow:** *text transformation graphs*, grafos computacionais imperativos escritos no `forward()` em Python.
4. **Grafo:** *define-by-run* (como PyTorch). A estrutura é escrita à mão e fixa: o compilador otimiza os nós, não a topologia.
5. **Execução:** **compilação** (roda sobre exemplos, grava o trace, filtra pela métrica, guarda as demonstrações boas) e depois **inferência** com os prompts otimizados.
6. **Estado:** parâmetros aprendidos por módulo (instruções e demonstrações), variáveis Python entre módulos e configuração global em `contextvars`.
7. **Concorrência:** threads (`ThreadPoolExecutor`, `dspy.Parallel`) com configuração por thread. Usada principalmente para avaliar e compilar em lote.
8. **Sem solução:** depende de métrica e dados; não otimiza a estrutura; o grafo não é analisável antes de rodar.

**Para a Calyx:** a **signature** é o tipo de um nó. "Compilar = otimizar com dados" é uma fase possível do compilador, para o futuro.

## 4. AgentSPEX: An Agent SPecification and EXecution Language (Wang et al., 2026)

Leitura feita principalmente no código-fonte ([ScaleML/AgentSPEX](https://github.com/ScaleML/AgentSPEX)).

1. **Problema:** o ReAct deixa controle e estado implícitos; LangGraph, DSPy e CrewAI acoplam a lógica ao Python. O AgentSPEX quer workflows legíveis e separados do código.
2. **Agente:** workflow YAML + harness (LLM, tools via MCP, sandbox Docker, checkpoints, logs). Submódulos podem virar tools com assinatura tipada.
3. **Workflow:** lista de passos em YAML (`step`, `task`, `if`, `switch`, `for_each`, `while`, `parallel`, `gather`, `call`, `return`, `set_variable`, `input`, `increment`). É uma **árvore de controle estruturado**, não um grafo de nós e arestas; o grafo só existe na visualização (Mermaid).
4. **Grafo:** estático quando escrito à mão. Há também um modo em que um LLM barato gera o YAML antes de executar (`agentic_loop/plan_generator.py`).
5. **Execução:** interpretador que percorre a árvore (`execution/interpreter.py`) e despacha pelo nome da chave YAML. Cada passo tem um id hierárquico (`1.2.p3`) usado em logs e retomada.
6. **Estado:** um único dicionário `context` mutável (`save_as`, `{{var}}`, `prev_output`). Os `step` compartilham `conversation_history`; os `task` não. Valores podem ser avaliados com `eval()` do Python. Memória longa em arquivo (`memory(...)`). Checkpoint = passos concluídos + JSON do estado; trace em JSONL com replay.
7. **Concorrência:**
   - `gather` e `parallel: {module, parameters_list}` são paralelos de verdade (`ThreadPoolExecutor`). Cada thread recebe `deepcopy` do contexto, e os resultados voltam num dicionário (`save_results_as`). É fork/join sem estado compartilhado.
   - `parallel` com lista de passos roda **sequencialmente** (comentário no código: "sequential execution for now"). As variáveis definidas em cada ramo **se perdem**; só volta o texto concatenado.
8. **Sem solução:** sem tipos de verdade (strings + `eval`); `parallel` não paralelo; nenhuma junção de estado entre ramos; árvore em vez de grafo (sem nó com várias entradas, sem ciclos arbitrários); o YAML não escala (no estudo com 23 usuários, a maioria preferiu LangGraph para workflows complexos); runtime em Python.

**Resultados reportados:** superou CoT e ReAct em 7 benchmarks (ex.: SciBench 90,61% vs 87,79%; ChemBench 83,30% vs 77,80%). Foi preferido em legibilidade e para começar do zero.

**Para a Calyx:** é o trabalho mais próximo e o mais útil como contraexemplo. Mostra exatamente onde falta semântica de estado e de concorrência. Vale copiar: limite obrigatório em laços, permissões de tools que só restringem, submódulo como tool, checkpoint e replay nativos.

## 5. From Static Templates to Dynamic Runtime Graphs: A Survey of Workflow Optimization for LLM Agents (Yue et al., 2026)

1. **Problema:** falta de vocabulário comum para workflows de agentes e para o que exatamente está sendo otimizado.
2. **Agente:** o próprio **Agentic Computation Graph (ACG)**. Os nós são chamadas de LLM, retrieval, tools, código, memória e verificação.
3. **Workflow:** o **template**: nós, arestas dirigidas, parâmetros dos nós (prompt, schema de tool, modelo), política de escalonamento e roteamento, e conjunto de **edições permitidas**.
4. **Grafo:** é o eixo central. A estrutura pode ser decidida estaticamente (antes do deploy), gerada ou escolhida antes da execução, ou editada durante a execução.
5. **Execução:** template → **realized graph** (o grafo usado naquela execução) → **execution trace**.
6. **Estado:** registrado no trace como tuplas *(estado, ação, observação, custo)*.
7. **Concorrência:** não é o foco; aparece embutida na política de escalonamento do template.
8. **Sem solução (declarado pelos autores):**
   - **Atribuição de crédito estrutural:** não se sabe se uma melhora veio de uma aresta nova, de um verificador novo, de um prompt diferente ou de mais computação.
   - **Expressividade × verificabilidade:** workflows com laços, criação dinâmica de agentes e condicionais ricas são poderosos, mas difíceis de validar estaticamente; representações intermediárias restritas ganham reprodutibilidade, mas podem excluir as soluções mais poderosas.

**Para a Calyx:** é a base teórica. Template, realized graph e trace podem ser conceitos de primeira classe da linguagem. A tensão expressividade × verificabilidade é a decisão de design central.

## 6. GraphFlow: A Graph-Based Workflow Management for Efficient LLM-Agent Serving (Li et al., ICML 2026)

1. **Problema:** sistemas com workflows escolhem templates inteiros por similaridade superficial, o que generaliza mal, e mantêm KV cache por workflow, duplicando memória.
2. **Agente:** um LLM executor (Qwen-2.5-7B, Llama-3.1-8B, Gemma-2-9B) guiado por um workflow gerado pelo sistema.
3. **Workflow:** subgrafo conectado e acíclico do **wGraph**, um DAG global com as operações atômicas de todos os workflows conhecidos.
4. **Grafo:** o wGraph é estático (montado offline). O workflow é dinâmico por requisição: GNN + MLP pontuam arestas, e a seleção é gulosa até formar um subgrafo válido. Sem ciclos.
5. **Execução:** **offline**: monta o wGraph, treina a GNN, pré-calcula o KV base de cada nó. **Online**: gera o subgrafo, reconstrói os KV e executa.
6. **Estado:** KV cache por nó = **KV base** (calculado isolado) + **resíduo esparso** dependente do caminho anterior. Mais de 70% das entradas quase não mudam com o prefixo. Caminhos raros não guardam resíduo e são recalculados (*path pruning*).
7. **Concorrência:** entre requisições. KV base compartilhado: < 0,5 GB com 50 requisições simultâneas, contra > 2,4 GB com cache por requisição.
8. **Sem solução** (o paper não tem seção de limitações; esta é a nossa avaliação): só DAG; precisa de dados para treinar a GNN; testado só com modelos de 7–9B; exige controle do servidor de inferência (não funciona com APIs fechadas); pequena perda de qualidade (52,6% vs 53,8% no MATH).

**Resultados:** +4,95 pontos percentuais em média sobre os baselines; ~4× menos memória de KV; P90 de 12,25 s vs 14,06 s do AFlow (Qwen-2.5-7B).

**Para a Calyx:** é a evidência de que conhecer o grafo antes de executar permite otimizações de sistema. Reforça a escolha de uma plataforma de sistemas (C ou C#).

---

## Visão comparada

| | Agente é… | Workflow é… | Grafo | Estado | Concorrência |
|---|---|---|---|---|---|
| **ReAct** | prompt | implícito no texto | nenhum (dinâmico) | o próprio prompt | nenhuma |
| **AutoGen** | objeto que conversa | a conversa | dinâmico, emergente | histórico por agente | turnos |
| **DSPy** | programa de módulos | código Python | define-by-run, estrutura fixa | parâmetros aprendidos + variáveis | threads (em lote) |
| **AgentSPEX** | YAML + harness | árvore de passos | estático (ou gerado por LLM) | um dicionário mutável | fork/join com cópia; `parallel` sequencial |
| **ACG (survey)** | o próprio grafo | template | estático → dinâmico | tuplas no trace | política de escalonamento |
| **GraphFlow** | LLM + subgrafo | subgrafo de um DAG global | DAG fixo, subgrafo por requisição | KV base + resíduo por caminho | KV compartilhado entre requisições |

## Conclusões transversais

1. **Ninguém trata estado e concorrência com seriedade.** É o espaço vazio que a Calyx ocupa (ver [01-hipotese.md](01-hipotese.md)).
2. **"Thread" tem três sentidos na literatura:** execução paralela, conversa (histórico) e caminho no grafo (prefixo que determina o estado, no GraphFlow).
3. **Expressividade × verificabilidade** é a decisão central de design (ver [03-decisoes.md](03-decisoes.md)).
4. **Conhecer o grafo antes de executar** permite otimizações que bibliotecas Python não conseguem fazer.

## Referências

- Yao et al. *ReAct: Synergizing Reasoning and Acting in Language Models.* [arXiv:2210.03629](https://arxiv.org/abs/2210.03629)
- Wu et al. *AutoGen: Enabling Next-Gen LLM Applications via Multi-Agent Conversation.* [arXiv:2308.08155](https://arxiv.org/abs/2308.08155)
- Khattab et al. *DSPy: Compiling Declarative Language Model Calls into Self-Improving Pipelines.* [arXiv:2310.03714](https://arxiv.org/abs/2310.03714), código: [stanfordnlp/dspy](https://github.com/stanfordnlp/dspy)
- Wang et al. *AgentSPEX: An Agent SPecification and EXecution Language.* [arXiv:2604.13346](https://arxiv.org/abs/2604.13346), código: [ScaleML/AgentSPEX](https://github.com/ScaleML/AgentSPEX), site: [agentspex.ai](https://agentspex.ai/)
- Yue et al. *From Static Templates to Dynamic Runtime Graphs: A Survey of Workflow Optimization for LLM Agents.* [arXiv:2603.22386](https://arxiv.org/abs/2603.22386), repositório: [IBM/awesome-agentic-workflow-optimization](https://github.com/IBM/awesome-agentic-workflow-optimization)
- Li et al. *GraphFlow: A Graph-Based Workflow Management for Efficient LLM-Agent Serving.* ICML 2026, [PMLR v306](https://proceedings.mlr.press/v306/li26ig.html)
