# Plano de implementação

## Estratégia: provar o mais arriscado primeiro

A parte mais barata de mudar é a sintaxe. A parte que ainda não foi provada, e pode derrubar o projeto, é o **modelo de execução**: o paralelismo sai mesmo sozinho do grafo? A retomada pelo diário funciona com chamadas reais? O `check` cabe em 1 segundo?

Por isso a implementação começa por uma **fatia vertical**: um caminho completo, de ponta a ponta, para um subconjunto pequeno da linguagem.

```text
arquivo .clyx → parser → check → representação intermediária → runtime → resultado
```

O runtime começa pelo **interpretador** de grafos (já previsto na arquitetura para grafos gerados por LLM). Gerar C nativo por programa vem depois, quando o modelo de execução estiver provado.

Os exemplos de `examples/` entram aos poucos: em cada marco, os programas de que ele precisa viram **testes**.

## Marcos

Cada marco termina com algo que roda e com uma medida ligada a uma pergunta de pesquisa da [hipótese](discovery/01-hipotese.md).

| Marco | Entrega | Mede |
|---|---|---|
| **M0** | Estrutura do repositório: compilador em Rust (sintaxe, verificação, representação intermediária, CLI), runtime em C, verificador ligado ao runtime como biblioteca estática, testes com programas `.clyx` de referência, CI | O verificador em Rust é chamado de dentro do runtime em C |
| **M1** | `calyx check` para o subconjunto inicial: `model`, `prompt`, `tool` de leitura, `type` (registros), `graph` com `node`, fan-out e `return`. Tipos, variáveis dos prompts, inferência de efeitos, erros estruturados | Tempo do `check` (meta: até 1 s) |
| **M2** | Runtime em C interpretando o grafo numa thread, com chamadas reais de LLM e tools via MCP | O `examples/research.clyx` roda de ponta a ponta |
| **M3** | Diário, retomada após queda, `calyx replay` | **Q3:** quanto trabalho é refeito depois de uma falha |
| **M4** | Workers com roubo de trabalho, limites, prioridade pelo caminho crítico | **Q1:** quanto paralelismo sai sozinho |
| **M5** | `loop`, `agent`, `match` com variantes, `try` | O ReAct como ciclo funciona |
| **M6** | `write`, `write once`, `requires`, sandbox, entidades | **Q2:** quantos bugs de estado o compilador pega |
| **Depois** | Geração de C nativo, várias máquinas, roteador, `rounds`, `race` | Desempenho e cobertura da especificação |

## Estado

| Marco | Situação |
|---|---|
| M0 | ✅ Concluído: workspace Rust (`calyx-syntax`, `calyx-check`, `calyx-ir`, `calyx-cli`), lexer completo com diagnósticos estruturados, verificador ligado ao runtime em C como biblioteca estática, testes de referência, CI |
| M1 | ✅ Concluído: parser com recuperação de erros; verificação de nomes, tipos, variáveis dos prompts, contratos das tools, limites, estrutura do grafo (ciclos, `return`) e efeitos (inferência e limite declarado); geração da representação intermediária (`calyx check --ir`); construções de marcos futuros reportadas com o marco em que chegam |
| M2 | ✅ Concluído: `calyx run`. A IR passou a levar as expressões, os modelos, as tools e os prompts (com o JSON Schema da resposta), e sai em JSON (`calyx check --ir-json`). Interpretador em C (valores imutáveis numa arena, fan-out na ordem da lista, novas tentativas por efeito, rastro). Camada de E/S em Rust: modelos pela API no formato da OpenAI (Gemini, NVIDIA, OpenAI e outros), tools por MCP via stdio, `calyx.toml`, modelos falsos para testes. Tudo numa biblioteca estática só |
| M3 | ✅ Concluído: diário por execução (`.calyx/runs/<id>/`), uma entrada por chamada com chave estável e hash do pedido, conteúdos grandes por hash, `begin` para `write once`, hash do programa (D23). `calyx resume`, `calyx replay`, `calyx runs`. Quedas simuladas nos testes (`CALYX_CRASH_AFTER`) |
| M4 | Próximo |
| M5–M6 | Não iniciados |

## Medidas

### M1: tempo do `calyx check`

Programas sintéticos (grafos de 21 nós com fan-out), binário de release, máquina de 4 núcleos:

| Tamanho | Tempo | Instruções executadas |
|---|---|---|
| 1,3 mil linhas (50 grafos) | ~3 ms | — |
| 12,5 mil linhas (500 grafos) | 37–39 ms | 342 milhões |
| 125 mil linhas (5000 grafos) | 460–1170 ms (variável entre execuções) | 3,34 bilhões |

- **Meta atingida** para projetos de tamanho médio: dezenas de milissegundos, muito abaixo de 1 s.
- **O algoritmo é linear:** 10× mais código executa 9,75× mais instruções (medido com o callgrind). A variação do tempo de relógio no arquivo grande vem de alocação de memória no ambiente, não do algoritmo.
- **Otimizações possíveis, ainda não necessárias:** cerca de 20% das instruções são alocação (`malloc`/`free`) e 5% são o hash padrão de `HashMap`. Trocar o hash por um mais rápido e reduzir cópias de texto deve reduzir o tempo do arquivo grande.

Para repetir: `cargo run --release -p calyx-check --example phases -- arquivo.clyx`.

### M3 (Q3): quanto trabalho é refeito depois de uma falha

`examples/research.clyx` com `gemini-3.5-flash-lite`, tema "baterias de sódio". O processo foi encerrado abruptamente (como um `kill -9`) logo depois da 5ª chamada, e retomado com `calyx resume`:

| | Antes da queda | Na retomada |
|---|---|---|
| Chamadas feitas | 3 de modelo, 2 de tool (3,0 s) | 4 de modelo, 3 de tool (4,7 s) |
| Tirado do diário | — | 5 (todas as concluídas) |
| **Chamadas refeitas** | — | **0** |

- **Resposta à Q3: nada do que terminou é refeito.** O que se perde numa queda é só a chamada em andamento naquele instante. Recomeçar do zero teria pago de novo 3 chamadas de modelo (652 tokens de entrada, 316 de saída) e uns 3 s.
- **O resultado é o mesmo de uma execução sem queda:** os testes comparam a saída de uma execução retomada com a de uma que nunca caiu.
- **Custo do diário:** cerca de 1 ms por execução (25 ms com diário, 24 ms sem, com modelos falsos). O `fsync` em lote é o que mantém esse custo baixo.
- **Limite atual:** chamadas que estavam em andamento são refeitas; com paralelismo (M4) podem ser várias ao mesmo tempo. Escritas `write once` interrompidas não são refeitas: a execução para (M6 traz as políticas `on_uncertain`).

### M2: primeira execução de ponta a ponta

`examples/research.clyx` com `gemini-3.5-flash-lite` e a busca falsa (MCP), tema "energia solar no Brasil":

| Medida | Valor |
|---|---|
| Chamadas | 7 de modelo, 5 de tool, 0 novas tentativas |
| Tokens | 2241 de entrada, 1560 de saída |
| Tempo total | 8,7 s, quase todo esperando o modelo (as 5 perguntas rodam **uma depois da outra**: o paralelismo é o M4) |
| Custo do runtime | ~30 ms por execução com modelos falsos, a maior parte para iniciar o servidor MCP em Python |

O que a primeira execução real ensinou:

- **Erros temporários são comuns:** no primeiro teste, um modelo do Gemini respondeu 503 (demanda alta). A política de novas tentativas (1 s, 2 s, 4 s; o dobro para limite de requisições) não é detalhe: sem ela o programa falha à toa.
- **Identificadores de modelo envelhecem rápido:** dois modelos usados poucos meses antes já não existiam para contas novas (404). Fixar a versão no programa (D23) é certo, mas o erro precisa dizer claramente qual modelo sumiu.
- **A conversa não é só texto:** o Gemini devolve uma assinatura do raciocínio (`thought_signature`) que precisa voltar nas chamadas seguintes de uma conversa. O diário (M3) e o `continue=` (M5) precisam guardar a mensagem inteira.
- **O tipo do prompt vira JSON Schema:** `Plan` (registro com `List[Text] max 5`) chegou do modelo já no formato certo, sem texto de instrução extra no prompt.

### M1: erros encontrados pelo próprio verificador

Ao escrever o `examples/research.clyx`, o verificador pegou um erro real do autor: dentro de um fan-out, a lista inteira de resultados era passada onde o prompt esperava o resultado de uma pergunta (`E0608`).
