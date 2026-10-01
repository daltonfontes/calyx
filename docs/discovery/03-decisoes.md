# Decisões de design

Decisões que a [hipótese](01-hipotese.md) levanta. Cada uma traz as opções, o que a literatura faz e uma recomendação. As marcadas com ✅ foram decididas; as demais seguem como pauta de discussão.

| # | Decisão | Recomendação preliminar |
|---|---|---|
| D1 | Modelo de estado | ✅ **Decidido:** dataflow + estado nomeado com junção obrigatória; visibilidade por snapshot |
| D2 | Tipos de efeito | ✅ **Decidido:** `pure`, `llm`, `read`, `write`, `write once` (com política obrigatória); arestas de ordem entre escritas |
| D3 | Threads e conversa | ✅ **Decidido:** threads = execução paralela derivada pelo runtime; `conversation` = valor; compactação explícita com verificação de orçamento |
| D4 | Quanto dinamismo permitir | Dinamismo limitado e verificável |
| D5 | Ciclos no grafo | Laço com valores carregados e limite obrigatório; ciclo só no template, execução desenrolada (DAG); ver [ReAct como ciclo](06-react-como-ciclo.md) |
| D6 | Unidade de recuperação | Diário de nós concluídos (event sourcing) |
| D7 | Junção de ramos paralelos | Lista ordenada por padrão; redutores declarados |
| D8 | Template / realized graph / trace | Os três são conceitos de primeira classe |
| D9 | Superfície da linguagem | Linguagem textual, com visualização derivada |
| D10 | Plataforma | ✅ **Direção decidida:** filosofia do Bend. O compilador emite C (um arquivo por programa, com o runtime); runtime em C com o modelo de atores da BEAM reimplementado; o mesmo binário roda de uma thread a várias máquinas; `calyx check` em até 1 segundo. **Compilador em Rust**, com o verificador compilado também como biblioteca estática ligada ao runtime em C (um verificador só). Ver [arquitetura do runtime](12-arquitetura-runtime.md) |
| D11 | Falha parcial em fan-out | Falhas são valores (`ok` / `falha`); o compilador obriga a tratar; tools declaram erros não retentáveis |
| D12 | Corrida e cancelamento | Construção `corrida` explícita; vencedor no diário; cancelamento cooperativo entre nós; `llm`/`read` abandonáveis, `write` protegidos; nós de compensação |
| D13 | Ambiente da execução (sandbox) | Efeitos na sandbox recuperáveis por snapshot, restaurado junto com o diário; isolamento entre ramos por validação do conjunto de leitura a cada escrita (STORM), preferida a cópia + merge |
| D14 | Tempo e execuções longas | Timers no diário; runtime durável e orientado a eventos; diário ligado à versão do template |
| D15 | Concorrência entre execuções | Chave de negócio (no máximo uma execução aberta por chave); recurso compartilhado tem uma execução dona, e as outras enviam mensagens a ela |
| D16 | Tamanho máximo da saída das tools | Tools declaram o máximo, ou o runtime trunca |
| D17 | Recursão de subgrafos | Permitida com parâmetro que decresce a cada chamada |
| D18 | Rodadas (barreira) | Construção explícita para ramos que trocam informação |
| D19 | Resultado antecipado | O grafo entrega a saída antes de terminar os nós restantes |
| D20 | Tamanho do diário | Subgrafos com diário próprio; conteúdos grandes fora do diário, referenciados por hash |
| D21 | Mensagens para uma execução em andamento | Mensagens tipadas e consultas de estado; mensagens recebidas vão para o diário |
| D22 | Timeouts | Timeout por tentativa obrigatório em nós com efeito externo; padrão por tipo de efeito |
| D23 | Versionamento de templates | Execuções fixadas na versão; migração quando o compilador provar compatibilidade |
| D24 | Escalonamento | Lista com prioridade pelo caminho crítico; durações estimadas pelo compilador e refinadas pelo histórico de traces |
| D25 | Invariantes entre ramos | Recursos: resolvido por afinidade (D26); valores: validação declarada na junção, violação produz valor `conflito` |
| D26 | Recursos afins | Sandbox, orçamento e capacidades `write once` têm um dono por vez; dividir entre ramos é explícito e verificado (ver [Bend](08-bend.md)) |
| D27 | Camadas da linguagem | Camada de grafo (`node`, efeitos, diário) + camada pura pequena (`let`, `fn`), recalculável; ver [sintaxe](09-sintaxe.md) |
| D28 | Chamada ao modelo | Forma única `modelo(prompt, continue: conversa?)`, saída tipada pelo prompt |
| D29 | Precondições no efeito | `requires { ... }` na chamada de um efeito, na camada pura; a tool valida sobre o estado atual na mesma transação; falha vira valor (ver [SVBE](10-svbe.md)) |
| D30 | Roteamento de modelo | Construção `router` que escolhe entre modelos por política, usando orçamento e histórico do diário; escolha gravada no diário (ver [mapa da orquestração](11-mapa-orquestracao.md)) |

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
| Limitado por E/S, não por computação | O tempo está na espera de LLMs e tools; desempenho bruto pesa menos que E/S assíncrona, durabilidade e custo de escrever o compilador (Bend) |

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
