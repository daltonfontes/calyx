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
| **M6** | `write`, `write once`, `requires`, sandbox, entidades. Em três partes: **M6a**, escritas externas seguras; **M6b**, sandbox; **M6c**, entidades | **Q2:** quantos bugs de estado o compilador pega |
| **Depois** | Geração de C nativo (como otimização, D35), várias máquinas | Desempenho e cobertura da especificação |

## Estado

| Marco | Situação |
|---|---|
| M0 | ✅ Concluído: workspace Rust (`calyx-syntax`, `calyx-check`, `calyx-ir`, `calyx-cli`), lexer completo com diagnósticos estruturados, verificador ligado ao runtime em C como biblioteca estática, testes de referência, CI |
| M1 | ✅ Concluído: parser com recuperação de erros; verificação de nomes, tipos, variáveis dos prompts, contratos das tools, limites, estrutura do grafo (ciclos, `return`) e efeitos (inferência e limite declarado); geração da representação intermediária (`calyx check --ir`); construções de marcos futuros reportadas com o marco em que chegam |
| M2 | ✅ Concluído: `calyx run`. A IR passou a levar as expressões, os modelos, as tools e os prompts (com o JSON Schema da resposta), e sai em JSON (`calyx check --ir-json`). Interpretador em C (valores imutáveis numa arena, fan-out na ordem da lista, novas tentativas por efeito, rastro). Camada de E/S em Rust: modelos pela API no formato da OpenAI (Gemini, NVIDIA, OpenAI e outros), tools por MCP via stdio, `calyx.toml`, modelos falsos para testes. Tudo numa biblioteca estática só |
| M3 | ✅ Concluído: diário por execução (`.calyx/runs/<id>/`), uma entrada por chamada com chave estável e hash do pedido, conteúdos grandes por hash, `begin` para `write once`, hash do programa (D23). `calyx resume`, `calyx replay`, `calyx runs`. Quedas simuladas nos testes (`CALYX_CRASH_AFTER`) |
| M4 | ✅ Concluído: cada passo e cada item de `for each` é uma tarefa; workers com fila de prioridade e roubo de trabalho; chamadas em threads de E/S (nunca bloqueiam um worker), até `limits threads`; prioridade pelo caminho crítico calculado pelo compilador; limites `rate` e `budget` (preços no `calyx.toml`); espera pedida pelo provedor respeitada; `--deterministic` |
| M5 | ✅ Concluído: `agent` (ciclo ReAct com chamada de tools nativa do provedor, tools em paralelo, `stuck`, `final_answer`), `loop` com `done`/`next`/`on limit`, `match` com cobertura de todas as variantes, `if`, operadores, construção de registros e variantes, `try` com `Result[T]`. Falhas locais (capturáveis) e respostas conferidas contra o tipo do prompt |
| Distribuição (D35) | ✅ Concluído: binários estáticos (musl) para Linux x86_64 e ARM, e binários para macOS, publicados por tag (`.github/workflows/release.yml`), com teste do binário e de um programa gerado com ele em cada alvo; `install.sh` com conferência de SHA-256; `calyx build` gera um executável autocontido (o próprio `calyx` com o programa e o `calyx.toml` anexados) |
| M6a | ✅ Concluído: escritas externas seguras. `write` com `idempotency_key` (a chave vai para a tool, e a escrita pode ser repetida em erros temporários); `write once` com `on_uncertain pause`, `accept_loss` e `verify(tool(...))`, aplicadas tanto quando a resposta se perde (timeout, servidor caiu) quanto na retomada; `calyx resume --uncertain done\|retry\|failed` para a decisão de uma pessoa; precondições `requires` conferidas pela tool sobre o estado atual (`checks`), com falha local `PreconditionFailed` que o `try` captura; `after` e aviso de escritas sem ordem; agentes não usam tools `write once`. Exemplo: `examples/refund.clyx` com a loja falsa `examples/tools/fake_store.py` |
| M6b | ✅ Concluído: sandboxes. Parâmetros `Sandbox` (a execução trabalha numa cópia do diretório), tools com `reads Sandbox` / `edits Sandbox`, empréstimos `reads repo` / `edits repo` nas chamadas e nas tools de agentes; a ordem entre passos sai dos empréstimos; erros para edições em paralelo e para a sandbox usada como valor. No runtime, travas (edições uma por vez), snapshots por conteúdo, chamada que falha desfeita antes de repetir, sandbox restaurada do diário na retomada. Exemplo: `examples/fix.clyx`, um agente de código que corrigiu um bug com o Gemini. `fork`/`share` ficam para depois |
| M6c | ✅ Concluído: entidades. `entity` com `state` e handlers puros que respondem ou mudam o estado; `ask` e `send`; estado em `.calyx/entities/`, uma mudança por vez por chave também entre processos (`flock`), cada mensagem aplicada uma vez (ids gravados junto com o estado), ordem entre mensagens à mesma entidade pela ordem do texto, aviso de atualização perdida (`W0603`). Exemplo: `examples/memory.clyx`, memória entre conversas com o Gemini. `receive` e `respond` ficam para depois |
| M7 | ✅ Concluído: camada pura (D27). `def` com valores novos, `if`/`elif`/`else` e `return`, sem efeitos e sem recursão; `true`/`false`; listas por compreensão (`[x.a for x in xs if ...]`); `in`; funções embutidas (`len`, `take`, `sum`, `join`, `lower`, `upper`, `trim`). Usáveis em grafos, handlers e outros `def`s |
| M8 | ✅ Concluído: `receive` com espera durável (D21). Tipos `message`; a execução para no estado `waiting` (código 4) e grava o prazo uma vez; `calyx deliver` confere a mensagem contra o tipo e a entrega; `calyx resume` e `calyx tick` (para cron, sem servidor) continuam; `on timeout` quando o prazo vence. Exemplo: `examples/approval.clyx` |
| M9 | ✅ Concluído: `rounds` e `race` (D18, D12). `rounds N, carry x = ...` com passos no corpo (`turn = for each r in roles: ...`) e barreira no fim de cada rodada; corpos de `loop` também aceitam passos. `race first where cond:` com ramos que rodam ao mesmo tempo; o primeiro que passa vence, vai para o diário (a retomada e o `replay` não disputam de novo) e os outros são cancelados entre passos (subgrafos param, chamadas que ainda não começaram não são feitas); `on none` obrigatório; aviso para escritas nos ramos. Exemplos: `examples/debate.clyx` e `examples/race.clyx`, rodados com o Gemini |
| M10 | ✅ Concluído: roteador de modelos (D30). `router r = route [barato, caro]:` com `policy cheapest_that_passes(check)`; chamado como um modelo; um modelo por vez até uma resposta passar na verificação (um `def`); a escolha vai para o diário; sem resposta que passe, a chamada falha e `try` captura. Erros de configuração (chave de API ausente) agora param a execução em vez de virar uma falha capturável. Exemplo: `examples/router.clyx`, triagem de chamados com o Gemini |
| M11 | Próximo (a definir) |

## Medidas

**Comparação com Python e LangGraph:** ver [`docs/evaluation/comparacao.md`](evaluation/comparacao.md) (recuperação, bugs antes de rodar, paralelismo e escala) e o [plano de avaliação para o paper](evaluation/plano-paper.md).

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

### M6 (Q2): quantos bugs de estado o compilador pega

**Com o aviso `W0605`, a suíte tem 53 bugs:** o compilador pega 35, 2 não podem acontecer por construção, o runtime pega 10 e 6 escapam; **47 de 53 nunca causam dano**. O novo veio do estudo de bugs reais ([`docs/evaluation/bugs-reais.md`](evaluation/bugs-reais.md)): um pagamento `write once` num laço de novas tentativas sai de novo a cada volta (CrewAI 5802).

**Com `rounds` e `race` (M9), a suíte tinha 52 bugs:** o compilador pega 34, 2 não podem acontecer por construção, o runtime pega 10 e 6 escapam; **46 de 52 nunca causam dano**. Os 7 novos: ramo de corrida que cobra o cliente (`W0604`), dois ramos editando o mesmo repositório (`E0645`), corrida sem `on none` (`E0683`) e condição que chama um modelo (`E0682`), pelo compilador; rodada que vê respostas pela metade (por construção: o `next` só existe com todas); corrida decidida de novo na retomada e ramo perdedor que continua gastando (runtime).

**Com o `receive` (M8), a suíte tinha 45 bugs:** o compilador pega 30, 1 não pode acontecer por construção, o runtime pega 8 e 6 escapam; **39 de 45 nunca causam dano**. Os 4 novos: espera sem prazo e espera de um tipo que não é `message` (compilador); resposta entregue duas vezes e prazo que sobrevive a um reinício (runtime).

**Com as entidades (M6c), a suíte tinha 41 bugs:** o compilador pega 28 (68%), 1 não pode acontecer por construção, o runtime pega 6 e 6 escapam. **35 de 41 nunca causam dano.**

| Quem pega | Bugs de entidade (9) |
|---|---|
| Compilador (5) | atualização perdida (ler, somar, gravar); handler que chama modelo; `ask` a quem não responde; mensagem com nome errado; campo de estado errado |
| Por construção (1) | consultar logo depois de gravar, na mesma execução (a ordem sai do texto) |
| Runtime (2) | várias execuções do mesmo usuário ao mesmo tempo (`flock`: 50 de 50 aplicadas); retomada que manda de novo (aplicada uma vez) |
| Ninguém (1) | o mesmo depósito registrado por duas execuções diferentes (clique duplo): são mensagens diferentes |

**Memória com o Gemini** (`examples/memory.clyx`): a primeira conversa ("moro em Recife e tenho uma gata chamada Pipoca") gravou dois fatos; a segunda, numa execução nova, respondeu "Sua gata se chama Pipoca e você mora em Recife!"; outro usuário não viu nada disso.

**Com as sandboxes (M6b), a suíte tinha 32 bugs:** o compilador pegava 23 (72%), o runtime 4, ninguém 5. Os 10 de sandbox:

| Quem pega | Bugs de sandbox |
|---|---|
| Compilador (6) | itens de um `for each` editando o mesmo repositório; tool que edita recebendo empréstimo de leitura; testes e edição ao mesmo tempo; repositório guardado num passo; tool que edita declarada como leitura; agente com tool de edição sem a sandbox |
| Runtime (2) | tool que cai depois de escrever parte da edição (desfeita antes de repetir); queda entre edições (sandbox restaurada do diário) |
| Ninguém (2) | tool que escreve fora da sandbox; tool de leitura que escreve na sandbox |

Os dois que escapam pedem isolamento do sistema operacional (a tool só enxergar a cópia) ou conferir, depois de cada leitura, que nada mudou. As duas coisas ficam para depois.

**O agente de código com o Gemini** (`examples/fix.clyx`, `average([2, 4, 6])` devolvia 6): em 6 voltas (4,9 s) listou os arquivos, leu o código e o teste, corrigiu a divisão, rodou os testes (passaram) e respondeu; o `diff` depois do agente mostra só a linha corrigida, e o diretório original não mudou.

#### M6a: escritas externas

Uma suíte de 22 workflows pequenos, cada um com um bug de estado conhecido envolvendo escritas externas (`tests/state_bugs/`). Cada programa diz na segunda linha quem pega o bug, e um teste (`compiler/calyx-check/tests/state_bugs.rs`) confere. Para os que o compilador não pega, o teste exige que ele não diga nada, então a conta é honesta.

| Quem pega | Bugs | Exemplos |
|---|---|---|
| **Compilador, antes de rodar** | **17 (77%)** | e-mail sem política para resultado incerto; agente com tool de e-mail; aviso que pode sair antes do pagamento; precondição numa tool que não sabe verificá-la, com campo errado, com tipos errados ou com uma chamada; `accept_loss` que inventaria um resultado; verificação que escreve; pagamento sem chave de idempotência; `after` com nome errado ou circular; grafo de leitura que paga; falha usada como sucesso; dois ramos que gravam o mesmo valor |
| Runtime, quando o bug aconteceria | 2 | queda no meio do envio de um e-mail (nunca reenviado sem decisão); pedido que mudou entre a decisão e o reembolso (a tool recusa, `PreconditionFailed`) |
| Ninguém | 3 | conferir com uma leitura e agir depois, sem `requires`; chave de idempotência mal escolhida (o pedido em vez da solicitação); itens de um `for each` gravando o mesmo arquivo |

- **Resposta à Q2, para escritas externas: 17 de 22 bugs saem antes de rodar, e 19 de 22 nunca causam dano.** Dois dos 17 são avisos (`W0601`, `W0602`), não erros: o programa roda, mas o problema fica dito.
- **O que escapa é semântico:** a linguagem garante que a precondição é conferida no momento certo, mas não obriga a escrevê-la, nem sabe se a chave escolhida identifica a operação certa. Dá para fechar parte disso depois: avisar quando uma escrita com `checks` não tem `requires`; avisar quando itens de um `for each` escrevem com a mesma chave.
- **Comparação:** em bibliotecas como LangGraph (Python), nenhum desses bugs é verificado antes de rodar, porque não há análise do programa inteiro. Não medimos isso; é uma consequência de serem bibliotecas.
- **Com um modelo de verdade:** `examples/refund.clyx` com o Gemini propôs 300 de reembolso para um pedido de 300; a loja pagou e o e-mail saiu depois (`notice after paid`). As falhas incertas (resposta perdida, servidor que cai antes ou só na primeira vez) são simuladas pela loja falsa e cobertas por testes de cada política, com retomada e replay.

### M5: o ReAct como ciclo funciona

`examples/agent.clyx` com `gemini-3.5-flash-lite` e a busca falsa (MCP): um agente que pesquisa, seguido de um laço de revisão (`loop` + `match` em `Approved | Rejected(feedback)`).

| | Resultado |
|---|---|
| Agente | 6 voltas com chamada de tool nativa do Gemini, depois `final_answer` pelo limite de voltas (a busca falsa nunca traz o que ele procura) |
| Assinatura de raciocínio do Gemini | Ida e volta em todas as voltas, sem erro: a mensagem do modelo volta exatamente como veio |
| Laço de revisão | `Rejected` com comentário → resposta melhorada → `Approved` → `done` |
| Total | 10 chamadas de modelo, 6 de tool |

O que o M5 ensinou:

- **Modelos não respeitam o esquema que recebem.** Na primeira execução, o esquema de `Review` juntava os campos de todas as variantes como opcionais, e o Gemini respondeu `{"kind": "Rejected"}` **sem** o `feedback`. O laço seguiu com um comentário vazio, e o modelo respondeu "você esqueceu de colar o comentário". Duas correções:
  - o esquema agora tem uma alternativa por variante, cada uma exigindo os próprios campos;
  - **toda resposta é conferida contra o tipo antes de ser usada**; fora do tipo, conta como erro e o modelo é chamado de novo. É isso que torna "a resposta chega no tipo declarado" uma garantia, não uma esperança.
- **Escrever os testes achou dois erros do verificador**: o literal `0` não servia de valor inicial de um laço com `next i + 1`; e os campos de um `case` eram ligados pelo nome, o que tornava impossível aninhar dois `match` sobre `Result` (os dois ligavam `error`). Agora os campos são ligados pela posição, como no Python.
- **Uma falha precisa ser local antes de ser global.** Para o `try` funcionar, o erro de uma chamada deixou de parar a execução na hora: ele fica com a tarefa, e só para tudo se nada o capturar. Do mesmo jeito, uma tool que falha dentro de um agente vira uma observação para o modelo, e o agente continua.
- **Latência do plano gratuito:** nessa execução, cada chamada levou de 3 a 35 s (antes, 1 a 2 s). O agente levou 3 min, quase todo esperando o provedor. Para agentes, o limite de ritmo do provedor pesa mais que tudo o resto.

### M4 (Q1): quanto paralelismo sai sozinho

`examples/research.clyx`, **sem mudar uma linha**, com `gemini-3.5-flash-lite` e a busca falsa (MCP). Duas execuções em cada modo, com pausas para respeitar a cota do plano gratuito:

| | Sequencial (`--deterministic`) | Paralelo (padrão) |
|---|---|---|
| Tempo total | 7,18 s e 8,36 s | 4,84 s e 4,63 s |
| Fan-out (5 buscas + 5 resumos) | 4,13 s e 4,76 s | 1,64 s e 1,62 s |
| Chamadas ao mesmo tempo | 1 | 5 |

Com modelos falsos de 1 s por chamada (sem variação de rede): sequencial 5,30 s, paralelo 3,03 s, e o caminho crítico do grafo é de 3 chamadas (plano → resumo → relatório).

- **Resposta à Q1: todo o paralelismo que o grafo permite sai sozinho.** As 5 perguntas rodaram juntas sem nenhuma palavra de paralelismo no programa, e com latência fixa o tempo fica a 30 ms do caminho crítico.
- **O ganho total é limitado pela forma do grafo, não pelo runtime:** 1,6× no total (7,8 s → 4,7 s em média), 2,5 a 2,9× no trecho em paralelo. O plano e o relatório são sequenciais por natureza (cada um depende do anterior), e o fan-out leva o tempo do resumo **mais lento**, não a média.
- **O que a medição ensinou:**
  - **Cota antes de velocidade.** No plano gratuito do Gemini (15 requisições por minuto), a 3ª execução seguida já recebeu 429. O provedor dizia quanto esperar ("retry in 21 s"), mas o runtime tentava de novo em 2, 4 e 8 s e desistia. Agora ele espera o que o provedor pede (até 60 s), e `limits rate 15/min` evita o problema de antemão.
  - **"Determinístico" precisava ser exato.** Com uma thread só, a ordem das chamadas ainda podia variar se a thread de E/S pegasse uma chamada antes de outra mais prioritária entrar na fila. No modo determinístico, a próxima chamada agora só é escolhida quando nenhum passo pode rodar, e a ordem se repete sempre (há teste para isso).
  - **A prioridade pelo caminho crítico só pesa quando as vagas são poucas.** Com `threads 8` e 5 chamadas, tudo cabe e a ordem não importa; com `threads 1`, a cadeia mais longa sai primeiro (há teste para isso).

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
- **Limite atual:** chamadas que estavam em andamento são refeitas; com paralelismo (M4) podem ser várias ao mesmo tempo. Escritas `write once` interrompidas não são refeitas sem critério: a política `on_uncertain` da tool decide (M6a).

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
