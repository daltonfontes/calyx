# Trabalhos relacionados: o que cada um garante

Levantamento feito em 2026-10-02 para a seção 8 do [rascunho](rascunho.md).

**Limitação:** arxiv.org, dl.acm.org, link.springer.com, researchgate.net e
api.semanticscholar.org estavam bloqueados pela rede do ambiente. O que está
abaixo vem dos resumos devolvidos pela busca na web, não da leitura dos
artigos. **Antes de submeter, cada linha precisa ser conferida no texto do
artigo**, em especial a coluna "Efeitos externos".

## A pergunta

A tese da Calyx é que o contrato de um efeito (repetível com que chave, o que
fazer quando o resultado é incerto, em que ordem) pertence à declaração da
tool e deve ser exigido **pelo compilador**, antes de rodar. Para cada
trabalho: o que garante, quando (antes de rodar ou ao rodar), e se trata de
efeitos externos repetidos, perdidos ou fora de ordem.

## Mais próximos

| Trabalho | O que faz | Quando | Efeitos externos | Diferença para a Calyx |
|---|---|---|---|---|
| **Where Does Exactly-Once Live?** (LIMBO), arXiv 2609.29095, 2026 | Benchmark de efeitos duplicados: 6 serviços, 12 modos de falha (commit atrasado, reentrega, lote parcial), 25.930 episódios, 9 modelos, 3 harnesses. Oferecer chave de idempotência em toda escrita baixa a duplicação de 28% para 4%; com commit atrasado e reentrega, o modelo pouco importa: o que decide é o contrato da tool | Ao rodar (medição) | **Sim, é o tema central** | Mede o problema e conclui que a garantia mora no contrato da tool. É a evidência empírica da tese da Calyx; a Calyx propõe tornar esse contrato obrigatório e conferido. **Possível baseline externo** (código aberto: `jaxblack/limbo-bench`) |
| **Verified Tool Calls…** arXiv 2608.02645, 2026 | Um wrapper de tool com verificação de pós-condição, *verify-before-retry* e chave de idempotência; taxonomia de 4 modos de falha não atômica | Ao rodar (biblioteca) | **Sim** | O mesmo mecanismo do `on_uncertain verify(...)` e da `idempotency_key` da Calyx, como biblioteca opcional. A Calyx exige a declaração e recusa a tool `write once` sem política (`E0304`) |
| **Agentproof**, arXiv 2603.20356, 2026 | Extrai um grafo abstrato de LangGraph, CrewAI, AutoGen e Google ADK; 6 verificações estruturais (alcançabilidade, becos sem saída, tools sensíveis precedidas de portões) e políticas temporais numa DSL compilada para autômatos | **Antes de rodar** e ao rodar | Não pelo resumo: verifica estrutura e ordem de nós, não repetição de efeitos | Verificação estática de grafos de agentes já existe, sobre frameworks existentes. A Calyx confere efeitos declarados (chave, política de incerteza, escritas sem ordem), não só a topologia |
| **λ_A**, arXiv 2604.11767, 2026 | Cálculo lambda tipado para composição de agentes (chamadas a oráculo, ponto fixo limitado para o ReAct, escolha probabilística, ambientes mutáveis). Segurança de tipos e terminação provadas em Coq; um lint derivado acha 94,1% de 835 configurações reais incompletas | **Antes de rodar** | Modela chamadas de tool como efeito IO, sem distinguir repetíveis de irreversíveis (pelo resumo) | Fundamento formal próximo do que falta à Calyx (seção 4 do rascunho). Não trata repetição, incerteza nem recuperação |
| **When Agents Do Not Stop** (IAL-Scan), arXiv 2607.01641, 2026 | Análise estática de laços infinitos de agentes em projetos reais: 68 falhas confirmadas em 47 projetos de 6.549, precisão de 91,9% | **Antes de rodar** | Cita efeitos externos repetidos como consequência | A Calyx exige limite em todo laço e agente e para o agente preso (`on stuck`); o IAL-Scan acha o problema em código Python existente |

## Linguagens e cálculos para programas com LLM

| Trabalho | O que faz | Relação |
|---|---|---|
| **Composable Effect Handling for LLM-integrated Scripts** (Di Wang, LMPL @ ICFP/SPLASH 2025) | Efeitos algébricos para separar a lógica das operações com efeito (chamadas ao LLM, E/S, concorrência); 10× num Tree-of-Thoughts | Efeitos para modularidade e paralelismo, não para recuperação nem idempotência |
| **Pangolin** (Tan, Wei, Sen, Zaharia; LMPL 2025) | Linguagem com chamadas a LLM como efeitos algébricos e mônada de seleção para escolher entre caminhos | Idem: composição e escolha de resultados |
| **The LLMbda Calculus** (Garby, Gordon, Sands), arXiv 2602.20064, 2026 | Cálculo com conversas e controle de fluxo de informação; não interferência provada; o interpretador verificado é o próprio harness | Segurança de informação (injeção de prompt), não estado nem efeitos repetidos |
| **Effect-Transparent Governance…**, arXiv 2605.01030, 2026 | Formalização em Rocq (Interaction Trees) de um operador que media todos os efeitos de um workflow | Governança dos efeitos, mecanizada; não trata recuperação |
| **AgentFlow** (política de fluxo), arXiv 2608.22868, 2026 | Linguagem de políticas sobre por onde os dados passam (taint), com monitor em tempo de execução e verificador SMT | Segurança de fluxo de dados |
| **AgentFlow** (grafos de dependência), arXiv 2607.01640, 2026 | Análise estática de programas de agentes em 5 frameworks; acha 238 riscos de prompt-para-tool | Análise de programas existentes, foco em segurança |
| **AgentSPEX** (Wang et al., 2026), já no levantamento do projeto | Workflows em YAML com checkpoints | Sem tipos de verdade nem efeitos declarados |
| **ReAct, AutoGen, DSPy** | Já no levantamento do projeto | Ver `docs/discovery/02-papers.md` |

## Execução durável e o contrato do efeito

| Sistema | O que garante | Relação |
|---|---|---|
| **Temporal** | Histórico de eventos e reexecução determinística; *activities* devem ser idempotentes | Medido na W2 e na W3 |
| **Restate** | Diário com `ctx.run`; deduplicação de chamadas entre serviços dele; para APIs externas, a idempotência continua sendo do programador (segundo a documentação e textos de terceiros) | Mesmo modelo de diário da Calyx; o contrato do efeito externo fica fora |
| **AWS Durable Execution SDK** | Cada passo é *at-least-once* (para operações idempotentes ou com chave) ou *at-most-once* (para efeitos externos sem chave), escolhido pelo programador | A escolha existe, mas é por chamada e opcional; na Calyx é uma declaração da tool conferida pelo compilador |
| **Golem** | Blocos atômicos | **[conferir]** |
| **Azure Durable Functions / Netherite** | Workflows serverless duráveis | **[conferir]** |

## Padrões que os servidores de tools já declaram

**Anotações de tools do MCP:** `readOnlyHint`, `destructiveHint`,
`idempotentHint`, `openWorldHint`. Uma tool sem anotação é tratada como
destrutiva e não idempotente. São **dicas** para o cliente (pedir
confirmação, decidir se pode repetir), não contratos conferidos.

Consequência para a Calyx: as declarações `effect read | write | write once`
são o mesmo vocabulário, só que obrigatórias. **Dá para conferir a declaração
de uma tool contra as anotações do servidor MCP** (por exemplo, avisar quando
uma tool declarada `read` vem com `readOnlyHint: false`). Isso reduz a
principal fraqueza da Calyx: hoje, se o programador declara errado o efeito,
nada confere.

## Veredito sobre a novidade

1. **Os mecanismos de runtime não são novos**: diário por chamada (Temporal,
   Restate), chave de idempotência e *verify-before-retry* (Verified Tool
   Calls, 2026). O rascunho não deve reivindicá-los.
2. **A tese tem evidência externa**: o LIMBO conclui, com 25.930 episódios,
   que a garantia de efeito único depende do contrato da tool, não do modelo.
   É o melhor apoio que o paper pode ter, e de outros autores.
3. **O que parece novo** (a confirmar lendo os artigos): uma linguagem em que
   esse contrato é **obrigatório e conferido pelo compilador**, junto com o
   paralelismo derivado das dependências. Os trabalhos estáticos próximos
   (Agentproof, λ_A, IAL-Scan) conferem estrutura, tipos ou terminação, não o
   contrato dos efeitos externos.
4. **Ameaça**: o λ_A já dá uma base formal tipada para agentes. O paper
   precisa da sua própria formalização das regras de efeito, ou se posicionar
   como estendendo o λ_A com efeitos externos e recuperação.

## Próximos passos que isso sugere

- **Rodar a Calyx no LIMBO.** É um benchmark externo, feito por outros, com
  um livro-razão dos efeitos de verdade: ataca a maior ameaça do paper (o
  autor escreveu os baselines e o corpus). Precisa de acesso ao GitHub do
  projeto.
- **Conferir as declarações contra as anotações MCP** (`idempotentHint`,
  `readOnlyHint`).
- Ler os artigos de verdade antes de submeter (precisa liberar arxiv.org na
  rede do ambiente, ou ler fora dele).
