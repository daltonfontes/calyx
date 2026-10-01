# Mapa da orquestração de agentes

**Fonte:** [CuiZHIQ/Awesome-LLM-Agent-Orchestration](https://github.com/CuiZHIQ/Awesome-LLM-Agent-Orchestration), uma lista curada (atualizada em 4 de setembro de 2026) de papers, benchmarks e frameworks sobre escalonamento, orquestração, roteamento, despacho de tools, coordenação multiagente e otimização de workflows.

**O que foi lido:** a lista inteira (taxonomia, roteiro de leitura, papers aceitos em 2026, seções por tema). **Os papers listados não foram lidos**; este documento usa a lista como mapa para ver onde a Calyx está e o que falta.

---

## A pilha de orquestração, segundo a lista

```text
Objetivo do usuário
  → Estimador de intenção / dificuldade
  → Planejador / decompositor
  → Roteador: de modelo, de tool, de agente, de memória
  → Conjunto de executores: LLMs, tools, APIs, navegadores, código
  → Escalonador: dependências, paralelismo, retentativas, orçamento, latência
  → Verificador / crítico
  → Memória / biblioteca de workflows
  → Sintetizador final
```

## Onde a Calyx está

| Camada | Na Calyx | Situação |
|---|---|---|
| Estimador de intenção | Um nó como outro qualquer (ex.: `classify` no W3) | ✅ expressável |
| Planejador / decompositor | Nós LLM; planos gerados como `Graph<...>` tipado (W7) | ✅ |
| **Roteador de modelo** | O modelo é fixo por nó | ❌ **lacuna** |
| Roteador de tool | O agente escolhe entre as tools permitidas (ReAct) | ✅ |
| Roteador de agente | `match` e `switch` no grafo | ✅ expressável, sem política aprendida |
| Executores | Tools e modelos, com efeitos declarados | ✅ núcleo da Calyx |
| **Escalonador** | Concorrência derivada, limites, prioridade pelo caminho crítico, retentativa por efeito, orçamento | ✅ **é o centro da Calyx** |
| Verificador | Laços de revisão, invariantes, precondições | ✅ |
| Memória / biblioteca de workflows | `entity` (W6); reuso de traces não coberto | ⚠️ parcial |
| Sintetizador | Um nó | ✅ |

**Leitura:** a Calyx cobre bem a camada que a lista chama de **escalonamento em tempo de execução** e dá estrutura às outras. Quase todos os papers recentes da lista atacam outra coisa: **políticas aprendidas** (roteadores treinados com RL, geração de topologia, memória com portões aprendidos). São camadas de **otimização sobre** a linguagem, como o compilador do DSPy ou os métodos do survey de ACG. A Calyx não deveria embutir nenhuma delas, mas precisa permitir que sejam **plugadas**.

## A lacuna: roteamento de modelo

A maior parte dos papers de 2026 da lista é sobre **roteamento**: escolher, a cada chamada, qual modelo usar (barato, forte, especialista), quanto deixar ele gerar e quando escalar. Na Calyx, hoje, cada nó usa um modelo fixo.

Proposta (**decisão nova D30**): um **roteador** como construção da linguagem, que escolhe entre modelos declarados segundo uma política:

```
router smart = route [haiku, sonnet, opus] {
  policy cheapest_that_passes(check)      // tenta do mais barato ao mais caro
  learn  from traces                      // usa o histórico de execuções
}

node answer = smart(solve(question))
```

O que a Calyx tem a oferecer aqui, e as bibliotecas não têm:

- **Orçamento conhecido em tempo de execução:** o runtime sabe quanto já foi gasto. Isso permite políticas sensíveis a orçamento, e permite **mostrar ao agente o orçamento restante**, que é a ideia do paper *Budget-Aware Tool Use* (COLM 2026).
- **Histórico estruturado:** o diário registra modelo, custo, latência e resultado de cada chamada. É exatamente o dado de que roteadores "por experiência" (como EvoRoute, ProgRouter) precisam.
- **Escolha gravada no diário:** a decisão do roteador é não-determinística; gravada, a retomada usa o mesmo modelo.

## O que vale ler a seguir

Selecionados da lista por tocarem decisões abertas da Calyx:

| Paper | Por que ler | Decisões |
|---|---|---|
| *An LLM Compiler for Parallel Function Calling* (ICML 2024) | Escalonador de DAG de chamadas de função, o mais próximo da concorrência derivada da Calyx | D3, D24 |
| *GPTSwarm: Language Agents as Optimizable Graphs* (ICML 2024) | Agentes como grafos otimizáveis: o lado "otimização sobre a linguagem" | D8, plugabilidade |
| *Budget-Aware Tool Use Enables Effective Agent Scaling* (COLM 2026) | Orçamento visível ao agente | D3, D30 |
| *Asynchronous LLM Function Calling* (2024) | Chamadas de função concorrentes geradas pelo próprio modelo | D2, D3 |
| *When Parallelism Pays Off: Cohesion-Aware Task Partitioning for Multi-Agent Coding* (2026) | Quando paralelizar agentes de código compensa | D13, D26 |
| *Characterizing LLM Agentic Workflows: A Study on N8n Ecosystem* (2026) | Mais de 6.000 workflows reais: **material para testar se a sintaxe da Calyx expressa o que as pessoas constroem de verdade** | Sintaxe |
| *Position: LLMs Can't Plan, But Can Help Planning in LLM-Modulo Frameworks* (ICML 2024) | LLM propõe, verificador decide: o mesmo princípio do SVBE e do `Graph<...>` do W7 | D29, W7 |
