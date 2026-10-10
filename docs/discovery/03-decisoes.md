# Decisões de design

Decisões que a [hipótese](01-hipotese.md) levanta. Cada uma traz as opções, o que a literatura faz e uma recomendação. Todas as decisões estão fechadas (✅). A D34 surgiu ao montar o [plano de implementação](../roadmap.md), e a D35 ao preparar a distribuição. A especificação consolidada está em [`docs/spec/calyx.md`](../spec/calyx.md).

| # | Decisão | Recomendação preliminar |
|---|---|---|
| D1 | Modelo de estado | ✅ **Decidido:** dataflow + estado nomeado com junção obrigatória; visibilidade por snapshot |
| D2 | Tipos de efeito | ✅ **Decidido:** `pure`, `llm`, `read`, `write`, `write once` (com política obrigatória); arestas de ordem entre escritas |
| D3 | Threads e conversa | ✅ **Decidido:** threads = execução paralela derivada pelo runtime; `conversation` = valor; compactação explícita com verificação de orçamento |
| D4 | Quanto dinamismo permitir | ✅ **Decidido:** até grafo gerado por LLM, desde que verificado antes de rodar (níveis 0 a 3); edição arbitrária durante a execução fica de fora |
| D5 | Ciclos no grafo | ✅ **Decidido:** laço com valores carregados e limite obrigatório; ciclo só no código, execução desenrolada; pior caso calculado multiplicando pelo limite; agente com variantes obrigatórias `turn_limit` e `stuck`; repetição detectada pelo runtime; compactação por tamanho com limite calculado pelo compilador. Ver [ReAct como ciclo](06-react-como-ciclo.md) |
| D6 | Unidade de recuperação | ✅ **Decidido:** diário de eventos com uma entrada por chamada de LLM ou tool; escrito só no fim do arquivo, em lotes; formato próprio em arquivo local (uma máquina); backend PostgreSQL atrás da mesma interface, para várias máquinas (feito: diário, blobs, lock por execução e `calyx worker`; entidades, `receive` e sandboxes ainda locais) |
| D7 | Junção de ramos paralelos | ✅ **Decidido:** resultados na ordem da entrada; redutor obrigatório quando ramos escrevem no mesmo estado |
| D8 | Template / realized graph / trace | ✅ **Decidido:** os três são conceitos da linguagem: o código, o grafo desenrolado de cada execução e o diário |
| D9 | Superfície da linguagem | ✅ **Decidido:** linguagem textual que **parece Python e se comporta como linguagem funcional** (blocos por indentação, `#`, `x = ...` para cada passo; valores imutáveis e ordem dada pelas dependências), com visualização do grafo gerada a partir do código. Substitui a primeira direção, com chaves no estilo Rust/TypeScript. Ver [sintaxe](09-sintaxe.md) |
| D10 | Plataforma | ✅ **Direção decidida:** o compilador emite C (um arquivo por programa, com o runtime); runtime em C com o modelo de atores da BEAM reimplementado; o mesmo binário roda de uma thread a várias máquinas; `calyx check` em até 1 segundo. **Compilador em Rust**, com o verificador compilado também como biblioteca estática ligada ao runtime em C (um verificador só). Ver [arquitetura do runtime](12-arquitetura-runtime.md) |
| D11 | Falha parcial em fan-out | ✅ **Decidido:** falhas são valores (`Ok` / `Failed`); o compilador obriga a tratar; tools declaram erros não retentáveis |
| D12 | Corrida e cancelamento | ✅ **Decidido:** `race` explícito; vencedor gravado no diário; cancelamento cooperativo entre nós (`llm`/`read` abandonados, `write` termina antes de valer). Compensação (saga): `compensate g(...)` na tool; o runtime desfaz as escritas do ramo que perdeu, uma vez, também depois de uma queda (implementada depois do M10) |
| D13 | Ambiente da execução (sandbox) | ✅ **Decidido:** efeitos na sandbox recuperáveis por snapshot, restaurado junto com o diário; dois modos explícitos: `fork` (cópias isoladas; na junção fica uma ou se juntam por merge) e `share` (repositório compartilhado; escrita aceita só se o que o ramo leu não mudou, como no STORM) |
| D14 | Tempo e execuções longas | ✅ **Decidido:** timers no diário; suspender = gravar o estado e liberar a memória; retomar = reconstruir a partir do diário; diário ligado à versão do template |
| D15 | Concorrência entre execuções | ✅ **Decidido:** `entity ... key`, no máximo uma execução aberta por chave, dona do recurso; outras execuções mandam mensagens; handlers de leitura rodam em paralelo, de escrita são exclusivos (visão inferida pelo compilador) |
| D16 | Tamanho máximo da saída das tools | ✅ **Decidido:** toda tool declara `max_output`; o runtime trunca o excedente; obrigatório para tools usadas por agentes |
| D17 | Recursão de subgrafos | ✅ **Decidido:** permitida com `decreases parametro`, verificado só pela sintaxe |
| D18 | Rodadas (barreira) | ✅ **Decidido:** `rounds a..b carry x: T = inicial { ... next valor }`, com barreira no fim de cada rodada; mesma forma do `loop` |
| D19 | Resultado antecipado | ✅ **Decidido:** `respond` entrega a resposta e o resto do grafo continua em segundo plano; grafo com `respond` não tem `return` de valor; no máximo um `respond` por caminho |
| D20 | Tamanho do diário | ✅ **Decidido:** conteúdos grandes (prompts, respostas) fora do diário, referenciados por hash; subgrafos grandes podem ter diário próprio |
| D21 | Mensagens para uma execução em andamento | ✅ **Decidido:** tipos `message`, `receive ... timeout ... else`, `ask` (síncrono) e `send` (assíncrono); toda mensagem recebida vai para o diário |
| D22 | Timeouts | ✅ **Decidido:** obrigatórios em nós com efeito externo; padrão por tentativa: `llm` 5 min, `read` 30 s, `write`/`write once` 60 s, `sandbox` 10 min; cada tool pode sobrescrever |
| D23 | Versionamento de templates | ✅ **Decidido:** cada versão é um binário; execuções terminam na versão em que começaram; migram só se o compilador provar que os grafos são compatíveis |
| D24 | Escalonamento | ✅ **Decidido:** lista com prioridade pelo caminho crítico (garantia de Graham: até 2× o ótimo); durações estimadas pelo compilador e refinadas pelo histórico; memória como limite opcional |
| D25 | Invariantes entre ramos | ✅ **Decidido:** recursos resolvidos por afinidade (D26); valores com regra declarada na junção (`ensures { ... }`), violação produz valor de conflito |
| D26 | Recursos afins | ✅ **Decidido:** sandbox, orçamento e capacidades `write once` têm um dono por vez; dividir entre ramos é explícito e verificado; sintaxe `reads` / `edits` (sem `&` / `&mut`). Ver [Bend](08-bend.md) |
| D27 | Camadas da linguagem | ✅ **Decidido:** camada de grafo (`node`, efeitos, diário) + camada pura pequena (`let`, `fn`), recalculável. Ver [sintaxe](09-sintaxe.md) |
| D28 | Chamada ao modelo | ✅ **Decidido:** forma única `modelo(prompt)`, ou `modelo(prompt, continue: conversa)` devolvendo `.value` e `.conversation`; `continue: new` começa uma conversa |
| D29 | Precondições no efeito | ✅ **Decidido:** `requires { ... }` na chamada de um efeito, na camada pura; escritas pelo programador ou propostas pelo LLM como saída tipada (só operadores permitidos); a tool valida sobre o estado atual na mesma transação; falha vira valor (ver [SVBE](10-svbe.md)) |
| D30 | Roteamento de modelo | ✅ **Decidido:** construção `router` na primeira versão, só com a política `cheapest_that_passes(verificação)`; escolha gravada no diário; `fastest_within` e políticas aprendidas ficam para depois, como extensões (ver [mapa da orquestração](11-mapa-orquestracao.md)) |
| D31 | Execução paralela | ✅ **Decidido:** N workers com roubo de trabalho (da ponta mais antiga); E/S nunca bloqueia um worker; contadores atômicos de dependência (ver [concorrência](13-concorrencia.md)) |
| D32 | Avaliação de ramos | ✅ **Decidido:** nós de ramos não escolhidos nunca rodam; resultados que deixaram de ser necessários são cancelados; sem execução especulativa por padrão |
| D33 | Impasse entre entidades | ✅ **Decidido:** o compilador recusa ciclos de `ask`; `send` pode formar ciclos |
| D34 | Implementação das tools | ✅ **Decidido:** tools rodam como servidores **MCP** (Model Context Protocol), em qualquer linguagem; o código Calyx só declara o contrato (efeito, `max_output`, timeout, idempotência, política); o runtime em C fala o protocolo |
| D35 | Distribuição | ✅ **Decidido:** quem usa a Calyx instala **um binário só**, autocontido, sem Rust nem compilador C. Binários prontos para Linux (estáticos, musl) e macOS, x86_64 e ARM, publicados em cada versão, com script de instalação. `calyx build` deixa de gerar C: copia o próprio binário e anexa o programa (fonte + `calyx.toml`), e o resultado roda onde não há Calyx. Rust e C ficam só para quem desenvolve a Calyx. Gerar C nativo continua possível depois, como otimização, não como requisito |

As decisões D11 a D19 surgiram no [teste no papel](04-teste-no-papel.md), onde estão descritas com os workflows que as motivaram. As propostas de D11, D12, D14 e D15 foram revisadas, e D20 a D23 surgiram, na leitura do [Temporal](05-temporal.md).

---

## D1. Modelo de estado

A decisão mais importante: sem ela, o runtime não consegue derivar concorrência.

| Opção | Quem faz assim | Concorrência derivável? | Problema |
|---|---|---|---|
| (a) Dicionário global mutável | AgentSPEX | Não | Qualquer nó escreve em tudo; os ramos paralelos precisam de cópia e perdem estado |
| (b) Canais de estado com *reducers* | LangGraph | Parcial | Leituras e escritas não são declaradas nem verificadas |
| (c) Dataflow puro: valores imutáveis nas arestas | Linguagens de dataflow, TensorFlow | Sim | Estado acumulado (memória, conversa) fica verboso |
| (d) Híbrido: dataflow + estado nomeado com junção obrigatória | — | Sim | Mais conceitos na linguagem |

**Recomendação: (d).** Por padrão, um nó recebe valores pelas arestas e produz valores. Quando precisa de estado compartilhado, o estado é nomeado, o nó declara se lê ou escreve, e **todo estado que pode ser escrito por ramos concorrentes tem uma regra de junção obrigatória**. O compilador recusa o programa se faltar a regra.

**Decidido: visibilidade por snapshot.** Ramos paralelos **não** veem as escritas uns dos outros enquanto rodam. Cada ramo vê o estado do momento da bifurcação, e as escritas só se juntam (via redutor) no ponto de junção. Não existe estado compartilhado "ao vivo" (quadro-negro), porque ele tornaria o resultado dependente da ordem de término e impediria o runtime de raciocinar sobre o grafo.

Escopos de estado:

| Escopo | Vive em | Regra |
|---|---|---|
| Valor | Arestas | Imutável, flui de um nó para outro |
| Estado nomeado | Uma execução do grafo | Snapshot na bifurcação + redutor obrigatório na junção |
| Persistente | Mundo externo | Acesso só via efeitos `read`/`write` (D2) |

## D2. Tipos de efeito

Base da recuperação de falhas.

**Decidido.**

| Efeito | Significado | Na recuperação |
|---|---|---|
| `pure` | Função determinística | Recalcula livremente |
| `llm` | Não-determinístico, sem efeito externo | Reaproveita o resultado gravado no diário |
| `read` | Lê do mundo externo (busca, arquivo, API GET, input humano) | Reaproveita o resultado gravado no diário |
| `write` | Altera o mundo de forma **idempotente** (pagamento com chave, sobrescrever arquivo) | Repete automaticamente |
| `write once` | Altera o mundo de forma **não idempotente** (enviar e-mail, postar mensagem) | Nunca repete; se a falha cair *durante* a chamada, aplica a política declarada |

Regras:

- **Tools declaram o próprio efeito**, na fronteira com o mundo externo. O **compilador infere o efeito dos nós**: o efeito de um nó é o maior efeito de tudo o que ele chama. O programador pode **restringir** ("este nó é no máximo `read`"), e o compilador verifica.
- **O diário registra cada chamada de LLM e de tool**, não só cada nó. Na retomada, um nó com laço interno (ex.: ReAct) é reencenado: as chamadas já feitas vêm do diário, e só a que falhou roda de novo. Sem isso, um nó que enviou um e-mail e caiu depois enviaria o e-mail de novo.
- **Todo nó `write once` declara uma política obrigatória** para o caso de falha *durante* a chamada (quando não dá para saber se o efeito aconteceu):
  - **verificar:** chama uma função que checa se o efeito aconteceu (ex.: buscar na pasta de enviados);
  - **pausar:** para a execução e pede decisão humana;
  - **aceitar perda:** segue sem repetir.

  O compilador recusa um nó `write once` sem política.

**Por que não proibir efeitos não idempotentes:** muitas integrações reais (e-mail, chat, APIs legadas, shell) não têm idempotência. Proibir afastaria a linguagem do mundo real e empurraria o controle de duplicidade para o programador, que o faria de forma ad hoc. Com o diário, o runtime já garante "no máximo uma vez" para qualquer escrita; a política obrigatória cobre a única janela restante.

Inspiração: sistemas de efeitos (Koka) e as garantias de atividades do Temporal.

### Ordem entre efeitos

**Decidido.** A concorrência é derivada das arestas de dados, mas dois nós com efeito externo podem precisar de ordem sem trocar dados (ex.: "salvar o relatório" antes de "enviar e-mail avisando que está salvo").

- Nós `pure`, `llm` e `read` sem dependência entre si rodam em paralelo livremente.
- Para nós `write` e `write once`, o programador declara **arestas de ordem** ("B depois de A"), que não carregam dados.
- O compilador **avisa** quando dois nós com efeito de escrita não têm ordem definida entre si.

## D3. Threads e conversa

**Decidido.**

### Threads = execução paralela, derivada pelo runtime

"Thread" na Calyx significa **execução paralela**. As threads **não são criadas pelo programador**: o runtime as deriva da estrutura do grafo.

| | Grafo | Threads |
|---|---|---|
| Quem escreve | O programador | Ninguém: o runtime deriva do grafo |
| O que o programador controla | Nós, arestas de dados, arestas de ordem, fan-out | **Limites**: máximo de threads simultâneas, requisições por segundo, orçamento de custo |
| Onde aparecem | No código | No trace e na visualização: cada caminho paralelo é uma thread visível |

Princípio: **"você escreve o grafo, o runtime extrai as threads."** Os limites são obrigatórios na prática, porque APIs de LLM têm limite de requisições e custam dinheiro. O programador limita o paralelismo, mas não o cria.

Um comando para criar threads foi descartado: ele contradiz a hipótese e repete o problema do AgentSPEX (paralelismo escrito à mão, que o runtime não consegue analisar).

### `conversation` = valor que percorre o grafo

O histórico de conversa com o LLM é um valor do tipo **`conversation`**, que flui pelas arestas como qualquer outro valor (D1).

- Um nó LLM que recebe uma `conversation` continua a conversa; um que não recebe começa com contexto limpo. Isso substitui a distinção `step` × `task` do AgentSPEX.
- Como o valor é imutável, ramos paralelos que recebem a mesma `conversation` trabalham sobre cópias (bifurcação). Juntá-las exige regra (D7).
- **Otimização derivada:** conversas bifurcadas compartilham o mesmo prefixo. O runtime sabe disso pelo grafo e pode usar o cache de prompt por prefixo das APIs de LLM sem anotação do programador (a versão "API fechada" do que o GraphFlow faz com KV cache).

Nomes descartados: `context` (conflita com "janela de contexto" e "contexto de execução"), `transcript` (sugere registro só de leitura).

Vocabulário da linguagem: *o programa é um **graph**, o runtime extrai as **threads**, e a **conversation** é um valor que percorre o grafo.*

### Compactação da conversa: explícita

Uma `conversation` cresce até estourar a janela de contexto do modelo.

- A compactação é feita por um **nó explícito** no grafo.
- **Verificação de orçamento:** cada nó LLM tem máximo de tokens de saída, e cada laço tem limite de iterações (D5). Com isso, o compilador calcula o **pior caso** do tamanho da `conversation` em cada ponto e dá **erro de compilação** se algum caminho puder estourar a janela do modelo, apontando onde falta compactação.
- **Opção:** o programador pode ligar a **inserção automática pelo compilador**, que adiciona o nó de compactação no grafo compilado, num ponto fixo e visível.
- Compactação automática **pelo runtime** foi descartada: seria uma chamada de LLM escondida (custo, não-determinismo, fora do grafo), impediria a verificação estática e quebraria o compartilhamento de cache por prefixo.

## D4. Quanto dinamismo permitir

A tensão expressividade × verificabilidade do survey de ACG.

| Nível | Exemplo | Verificável? |
|---|---|---|
| 0. Grafo estático | Pipeline fixo | Totalmente |
| 1. Condicionais e laços limitados | Laço de revisão com até N tentativas | Sim |
| 2. Fan-out tipado | Um nó de pesquisa por sub-pergunta gerada pelo LLM | Sim: não se sabe *quantos* nós, mas se sabe o tipo deles |
| 3. Subgrafo gerado e verificado antes de executar | Plan generator do AgentSPEX; GraphFlow | Sim, se passar pelo verificador |
| 4. Edição arbitrária durante a execução | DyFlow, EvoFlow | Não |

**Recomendação: níveis 0 a 3.** O nível 4 fica fora, pelo menos no início. Regra: *o grafo pode mudar em tempo de execução, mas só de formas que preservem as garantias estáticas.*

## D5. Ciclos no grafo

| Opção | Quem faz assim | Consequência |
|---|---|---|
| Só DAG | GraphFlow | Análise simples, mas não expressa laços de revisão |
| Ciclos com limite obrigatório | AgentSPEX (`max_iterations`) | Expressa revisão e retentativa; a análise trata cada iteração como uma "camada" |
| Ciclos livres | AutoGen, LangGraph | Risco de laço infinito e custo sem limite |

**Recomendação:** ciclos permitidos, com **limite obrigatório** verificado pelo compilador. Um limite de custo (tokens, dinheiro, tempo) também deve ser suportado.

## D6. Unidade de recuperação

| Opção | Quem faz assim | Problema |
|---|---|---|
| Checkpoint por "passo concluído" | AgentSPEX | Pressupõe execução sequencial |
| Checkpoint por super-passo | LangGraph (Pregel) | Refaz o super-passo inteiro |
| Diário de nós concluídos (event sourcing) | Temporal | — |

**Recomendação:** um **diário** em que cada entrada é *(nó, iteração, hash das entradas, saída, custo)*. Na retomada, todo nó cuja entrada já está no diário é pulado e tem a saída reaproveitada. Funciona naturalmente com ramos paralelos e é o mesmo dado que o trace de observabilidade (D8): recuperação e observabilidade saem da mesma estrutura.

## D7. Junção de ramos paralelos

| Caso | Regra padrão proposta |
|---|---|
| Fan-out sobre uma lista | Resultados em lista, **na ordem da entrada** (não na ordem de término), para a execução ser reproduzível |
| Dois ramos escrevem no mesmo estado nomeado | Obrigatório declarar o redutor: `concat`, `last`, `max`, `vote` ou função `pure` definida pelo usuário |
| Dois ramos estendem a mesma `conversation` | Obrigatório declarar: concatenar, resumir via LLM, ou manter separados |

**Em aberto:** redutores via LLM ("junte essas duas respostas") são nós `llm`, não `pure`. Permitir?

## D8. Template, realized graph e trace

**Recomendação:** adotar os três artefatos do survey de ACG como conceitos de primeira classe:

- **Template:** o código-fonte compilado.
- **Realized graph:** o que o runtime materializou nesta execução (após condicionais, fan-outs, subgrafos gerados). Pode ser inspecionado e visualizado.
- **Trace:** o diário da D6. Serve para retomada, replay, depuração, custo por nó e, no futuro, otimização estilo DSPy.

## D9. Superfície da linguagem

| Opção | Evidência |
|---|---|
| YAML | AgentSPEX: legível para coisas simples, mas usuários preferiram LangGraph para workflows complexos; sem tipos |
| Biblioteca em linguagem existente | LangGraph, DSPy: o grafo não é analisável antes de rodar |
| Linguagem textual própria | Permite tipos, verificação e compilador |
| Visual | Bom para inspeção; ruim como única forma de autoria |

**Recomendação:** linguagem **textual** própria, com **visualização do grafo derivada** do código (o editor sincronizado do AgentSPEX foi bem avaliado).

**Decisão final:** textual, com sintaxe que **parece Python e se comporta como linguagem funcional**. A primeira versão usava chaves (estilo Rust/TypeScript); foi trocada para ficar simples para qualquer pessoa usar. Detalhes e motivos em [09-sintaxe.md](09-sintaxe.md#revisão-parecer-python-se-comportar-como-linguagem-funcional).

## D10. Plataforma: C# ou C

Adiada por decisão do projeto. Critérios a considerar:

| Critério | Por que importa |
|---|---|
| Primitivas de concorrência do runtime | O runtime é um escalonador de grafo com muita espera de I/O (LLM, tools) |
| Ecossistema HTTP, JSON e clientes de LLM | Quase todos os nós conversam com APIs externas |
| Custo de escrever compilador e verificador | Parser, sistema de tipos, verificação de efeitos |
| Performance e controle de memória | Relevante se o runtime for gerenciar cache e estado em escala (lição do GraphFlow) |
| Embutir o runtime em outras aplicações | C é mais fácil de embutir; C# depende do .NET |
| Verificador embutido no runtime | Grafos gerados por LLM precisam ser verificados em tempo de execução (W7) |
| Execução durável | O runtime precisa persistir e retomar execuções que esperam dias (W3) |
| Runtime orientado a eventos | Execuções esperando não podem ocupar threads; vivem só no armazenamento (Temporal) |
| Limitado por E/S, não por computação | O tempo está na espera de LLMs e tools; desempenho bruto pesa menos que E/S assíncrona, durabilidade e custo de escrever o compilador |

---

## Exemplo ilustrativo

**Não é proposta de sintaxe.** Mostra só *que informação* cada nó precisaria declarar para a hipótese funcionar.

```
limites: no máximo 8 threads simultâneas, orçamento de 2 USD

nó planejar      efeito: llm    lê: pergunta           produz: subperguntas: lista<texto>
nó pesquisar[q]  efeito: read   para cada q em subperguntas   produz: achado: texto
nó escrever      efeito: llm    lê: pergunta, achados (junção: lista ordenada)
                                continua: conversa: conversation    produz: rascunho
nó revisar       efeito: llm    lê: rascunho           produz: aprovado: bool
laço escrever → revisar  até aprovado, no máximo 3 vezes
nó salvar        efeito: write       idempotência: hash(rascunho)   lê: rascunho
nó avisar        efeito: write once  política: verificar(pasta de enviados)
ordem: avisar depois de salvar
```

A partir só disso, o runtime e o compilador saberiam:

- **Concorrência:** os `pesquisar[q]` rodam em paralelo, até 8 threads por vez; `escrever` espera todos.
- **Dependências:** mudar `pergunta` invalida tudo; mudar um achado invalida só `escrever` em diante.
- **Recuperação:** se cair durante `revisar`, reaproveita `planejar`, os `pesquisar` e `escrever` do diário; `salvar` pode repetir com segurança; `avisar` nunca repete e, se cair no meio, verifica a pasta de enviados.
- **Ordem:** `avisar` não troca dados com `salvar`, mas a aresta de ordem impede que rodem em paralelo.
- **Orçamento de contexto:** com o máximo de tokens de `escrever` e o limite de 3 iterações, o compilador calcula o pior caso da `conversation` e recusa o programa se ela puder estourar a janela do modelo.
- **Observabilidade:** custo e latência por nó, por iteração do laço e por ramo do fan-out.

---

## Próximos passos sugeridos

1. ~~Discutir e fechar D1, D2 e D3.~~ ✅ Feito.
2. **Ler mais referências** que cobrem as lacunas desta rodada:
   - LangGraph e o modelo Pregel (o concorrente mais próximo em concorrência e estado).
   - LLMCompiler (Kim et al., ICML 2024): paralelismo derivado de DAG de chamadas de função.
   - Temporal / execução durável (base da D6).
   - Sistemas de efeitos (Koka, efeitos algébricos), como base da D2.
3. ✅ Feito, ver [04-teste-no-papel.md](04-teste-no-papel.md). **Validar a hipótese no papel:** escrever 5–10 workflows reais (dos benchmarks do AgentSPEX, por exemplo) na notação ilustrativa e checar se as quatro propriedades saem sem anotação extra.
4. **Só então:** sintaxe concreta e escolha entre C# e C.
