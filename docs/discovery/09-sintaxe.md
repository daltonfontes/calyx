# Sintaxe: primeiro teste com programas reais

**Status:** direção aprovada (superfície familiar, estilo Rust/TypeScript/Swift, sem a complexidade do Rust: sem `&`, *lifetimes*, `;`, `impl`, `async`). Detalhes em evolução.

## Escolhas de base

| Escolha | Decisão | Motivo |
|---|---|---|
| Palavras-chave | Inglês | LLMs vão escrever muito código Calyx (grafos gerados, assistentes de código) e escrevem melhor com palavras-chave em inglês. Nomes, prompts e comentários podem estar em qualquer língua |
| Blocos | Chaves `{ }`, com formatador oficial (`calyx fmt`) | Prompts longos de várias linhas e código editado por LLM tornam a indentação significativa frágil |
| Extensão | `.clyx` | Curta e lembra o nome |
| Prompts | Declarações próprias (`prompt nome(...) -> Tipo { """...""" }`), separadas do grafo | Lição do AgentSPEX: separar prompts da lógica foi o ponto mais elogiado. O grafo fica curto e mostra só a estrutura; o compilador verifica as variáveis e o tipo da saída |

## Os programas

| Arquivo | O que testa |
|---|---|
| [`examples/teste.clyx`](../../examples/teste.clyx) | Exemplo base: tools com efeito, prompts, fan-out com agente, laço de revisão, ordem entre efeitos |
| [`w1_pesquisa_recursiva.clyx`](../../examples/workflows/w1_pesquisa_recursiva.clyx) | Recursão com `decreases`, fan-out aninhado |
| [`w2_agente_de_codigo.clyx`](../../examples/workflows/w2_agente_de_codigo.clyx) | Sandbox como recurso afim, `effect sandbox`, compactação |
| [`w3_reembolso.clyx`](../../examples/workflows/w3_reembolso.clyx) | `receive ... timeout`, mensagens, `write once` com `pause` |
| [`w4_debate.clyx`](../../examples/workflows/w4_debate.clyx) | `rounds` com barreira, uma `Conversation` por agente |
| [`w5_lote.clyx`](../../examples/workflows/w5_lote.clyx) | `try` e falha como valor, funções puras (`fn`), limites de taxa |
| [`w6_memoria.clyx`](../../examples/workflows/w6_memoria.clyx) | `entity` com chave, `ask` / `send`, `respond` antecipado |
| [`w7_planejar_executar.clyx`](../../examples/workflows/w7_planejar_executar.clyx) | Plano gerado como `Graph<...>` tipado, `run` com verificação |
| [`w8_corrida.clyx`](../../examples/workflows/w8_corrida.clyx) | `race ... first where`, `fork` de sandbox |

## Resultado

**Os 8 workflows foram escritos sem nenhuma thread, checkpoint ou instrumentação escrita à mão**, o que é a hipótese vista do lado de quem escreve. Mas escrever os programas revelou pontos que o teste no papel (que usava uma notação solta) não mostrava.

### O que funcionou bem

- **Tools com bloco de propriedades** (`effect`, `max_output`, `retry_on`, `timeout`, `idempotency_key`, `on_uncertain`): todas as decisões sobre efeitos cabem num lugar só e são fáceis de ler.
- **Prompts separados**: os grafos ficaram curtos. O W5 tem 1000 documentos e o grafo cabe em 20 linhas.
- **Fan-out** `node x[i in lista] = ...`: legível e sem palavra para paralelismo.
- **`loop ... done / next ... else`**: expressou revisão (teste), correção (W5) e replanejamento (W7) com a mesma forma.
- **`try` e `Ok` / `Failed`** (W5, W7): falha como valor ficou natural, e `.ok()` / `.failed()` resolveram o lote.
- **`entity` com `ask` / `send`** (W6): a diferença entre consulta síncrona e escrita assíncrona (como *updates* e *signals* do Temporal) ficou clara.
- **`Graph<tools, max_effect, max_nodes>`** (W7): restringir o que o LLM pode gerar pelo tipo do plano foi uma das partes mais expressivas.

### Descoberta: recursos afins geram ordem de graça

No W2, o nó `diff(&repo)` precisava rodar depois do agente, mas não usa a saída dele. Esperava precisar de `after`. **Não precisou**: os dois usam a sandbox, e o empréstimo `&mut` do agente impede que outro nó a use ao mesmo tempo. **A afinidade (D26) deriva ordem automaticamente.** As arestas de ordem (`after`) ficam necessárias só para efeitos em recursos que a Calyx não controla (e-mail, pagamento).

### O que ficou estranho (marcado com ⚠️ nos arquivos)

| Onde | Problema | Direção possível |
|---|---|---|
| W1 | Fan-out dentro de fan-out gera lista de listas; precisou de `flatten` e de um `for` dentro de um nó | Uma forma única de fan-out que aceite aninhamento e achate o resultado |
| W2, W8 | ~~`&` e `&mut` (empréstimos estilo Rust) são pesados para o público-alvo~~ | ✅ Resolvido: `reads repo` / `edits repo` (D26) |
| W4 | `rounds` com dois `carry` e `next` com atribuição ficou verboso | Um tipo de registro para o estado da rodada |
| W4, W6 | ~~Chamar o modelo tinha três formas~~ | ✅ Resolvido: `claude(p)` ou `claude(p, continue: c)` (D28) |
| W6 | `respond` (entrega antecipada) e `return` convivem sem regra clara | Grafo com `respond` não tem `return` de valor; o resto do grafo roda em segundo plano |
| W3 | Nós dentro de ramos de `match`, com `return` no meio | Definir: ramos de um `match` são subgrafos exclusivos; `return` num ramo encerra o grafo |
| W8 | Não fica claro que `race` consome as três sandboxes e devolve só a do vencedor | Tornar explícito na assinatura de `race` (consome os recursos dos ramos) |

### Duas perguntas de fundo que apareceram

**1. `node` × `let` × `fn`.** Os programas usaram três formas sem regra clara:
- `node`: um passo que vai para o diário (tem efeito ou custa caro: LLM, tool);
- `let`: um valor calculado, sem efeito, que não precisa ir para o diário;
- `fn`: uma função pura reutilizável (como `check` no W5).

Proposta (**D27**): a linguagem tem **duas camadas**. A camada de **grafo** (`node`, efeitos, diário) e uma camada **pura** pequena (`let`, `fn`, expressões), sem efeitos, que o compilador pode recalcular à vontade. Lembra a separação do Bend entre o que roda e o que é dado copiável, e a do Temporal entre workflow e activity.

**2. Como chamar o modelo** (**D28**): unificar em `modelo(prompt, continue: conversa?)`, sempre devolvendo um valor tipado pelo prompt e, quando pedido, a conversa atualizada.

## Próximo passo

Resolver os pontos ⚠️ e D27/D28, reescrever os exemplos e só então escrever uma gramática formal.
