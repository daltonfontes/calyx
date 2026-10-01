# Teste da hipótese no papel

## Objetivo

Verificar se as quatro propriedades da [hipótese](01-hipotese.md) (concorrência, dependências de estado, recuperação de falhas e observabilidade) saem **automaticamente** da estrutura do grafo, usando só as decisões já tomadas em [03-decisoes.md](03-decisoes.md) (D1, D2, D3).

**Isto é um experimento mental, não uma medição.** Ele serve para achar onde a hipótese quebra antes de escrever qualquer código.

## Método

8 workflows reais, cada um escolhido para forçar um ponto diferente:

| # | Workflow | Ponto que força |
|---|---|---|
| W1 | Pesquisa profunda recursiva | Fan-out dinâmico aninhado, recursão |
| W2 | Agente de código (estilo SWE-bench) | Efeitos dentro de um laço ReAct, comandos de shell |
| W3 | Reembolso com aprovação humana | Espera longa, tempo, efeitos externos em ordem |
| W4 | Debate entre agentes | Agentes que precisam ver as respostas uns dos outros |
| W5 | Extração em lote (1000 documentos) | Escala, limites, falha parcial |
| W6 | Assistente com memória entre sessões | Estado persistente, execuções simultâneas |
| W7 | Planejar e executar (LLM gera o grafo) | Grafo dinâmico gerado em tempo de execução |
| W8 | Estratégias concorrentes ("vence a primeira") | Corrida, cancelamento, não-determinismo |

Para cada workflow: a notação (**ilustrativa, não é proposta de sintaxe**), o que sai automático, o que precisou de anotação extra e o que quebrou.

---

## W1. Pesquisa profunda recursiva

Baseado no workflow de *deep research* do AgentSPEX (profundidade, largura, `largura // 2` a cada nível).

```
grafo pesquisar(tema, profundidade: int ≤ 3, largura: int ≤ 8) -> relatório
  nó gerar_consultas  efeito: llm   lê: tema           produz: consultas: lista<texto> (máx largura)
  nó buscar[c]        efeito: read  para cada c em consultas   produz: resultados
  nó aprender[c]      efeito: llm   lê: resultados[c]  produz: aprendizados, novas_direções
  se profundidade > 1:
    subgrafo pesquisar[c](novas_direções[c], profundidade - 1, largura / 2)
  nó juntar           efeito: pure  junção: concat (na ordem das consultas)
  nó escrever         efeito: llm   lê: tema, aprendizados   produz: relatório
```

**Sai automático:**
- **Concorrência:** todos os `buscar[c]` em paralelo, e os subgrafos recursivos também. O runtime extrai uma árvore de threads.
- **Dependências:** cada nível depende só do próprio ramo.
- **Recuperação:** pelo diário; um ramo concluído não é refeito.
- **Observabilidade:** a árvore de execução é a própria visualização.

**Derivação extra (não prevista):** como a recursão e o fan-out têm limites, o compilador calcula o **número máximo de nós** (8 × 4 × 2 = 64 folhas) e, portanto, **um teto de custo antes de rodar**.

**Precisou de anotação extra:** limites de recursão (`profundidade ≤ 3`) e de fan-out (`máx largura`). É coerente com D4/D5: são limites, não mecânica de execução.

**Quebrou / em aberto:**
- **Recursão não estava prevista** em D4/D5. Ela precisa de um parâmetro que **decresce** a cada chamada, para o compilador provar que termina.
- Por causa do snapshot (D1), ramos irmãos podem pesquisar a mesma direção sem saber. É o custo aceito da D1; a solução é um nó de deduplicação entre níveis.

## W2. Agente de código (estilo SWE-bench)

```
nó entender   efeito: llm   lê: issue   produz: conversa: conversation
nó agir       efeito: (inferido)   continua: conversa
              tools: ler_arquivo [read], editar_arquivo [write], rodar_testes [?]
              laço ReAct até "pronto", no máximo 30 passos
nó gerar_patch  efeito: read   (git diff)
```

**Sai automático:**
- **Recuperação:** o diário registra cada chamada de LLM e de tool dentro do laço (D2); na retomada, o laço é reencenado até o ponto da falha.
- **Observabilidade:** custo e latência por passo do laço e por tool.

**Derivação extra (não prevista):** quando o LLM pede **várias tools no mesmo turno**, o runtime pode rodar em paralelo as que são `read` e manter em ordem as que são `write`. A concorrência sai do **tipo de efeito**, não só das arestas do grafo.

**Quebrou / em aberto:**
- **Qual o efeito de `rodar_testes`?** Um comando de shell pode fazer qualquer coisa: gravar arquivos, acessar a rede. Classificá-lo como `write once` tornaria cada execução de teste uma operação "perigosa". **Solução proposta:** um **ambiente da execução** (sandbox). O que acontece dentro da sandbox pertence à execução e é recuperável restaurando um snapshot dela (o AgentSPEX já faz backup e restauração do workspace). Só o que *sai* da sandbox (rede) conta como mundo externo.
- **A recuperação precisa restaurar o diário e a sandbox juntos**, no mesmo ponto. Senão, o laço reencenado vê arquivos num estado diferente do original.
- **Orçamento de contexto:** 30 passos × (máximo de tokens do LLM + **tamanho da saída da tool**). Sem um limite declarado para a saída das tools, o compilador não consegue calcular o pior caso. **As tools precisam declarar o tamanho máximo da saída** (ou o runtime trunca).
- **Concorrência dentro do laço:** pouca. Um laço ReAct é sequencial por natureza; a hipótese não ganha nada aqui além das tools paralelas no mesmo turno.

## W3. Reembolso com aprovação humana

```
nó classificar     efeito: llm    lê: mensagem          produz: intenção
nó buscar_pedido   efeito: read   lê: id_pedido         produz: pedido
nó buscar_cliente  efeito: read   lê: id_cliente        produz: cliente
nó decidir         efeito: llm    lê: pedido, cliente, política   produz: proposta {valor, motivo}
se proposta.valor > 100:
  nó aprovar       efeito: read (humano)   espera: até 3 dias   ao expirar: recusar
nó reembolsar      efeito: write       idempotência: id_pedido
nó notificar       efeito: write once  política: pausar
ordem: notificar depois de reembolsar
```

**Sai automático:**
- **Concorrência:** `buscar_pedido` e `buscar_cliente` rodam em paralelo sem ninguém pedir.
- **Recuperação:** a resposta humana fica no diário e a pergunta nunca é feita duas vezes; `reembolsar` repete com segurança; `notificar` nunca repete.
- **Ordem:** a aresta de ordem garante que o aviso não saia antes do reembolso.

**Quebrou / em aberto:**
- **Tempo é um efeito.** "Esperar até 3 dias" e "ao expirar" dependem do relógio. Se o runtime cair e voltar, precisa saber se o prazo já tinha vencido. **Timers precisam ir para o diário**, como no Temporal.
- **Execuções longas exigem que o runtime persista e "descarregue" a execução** enquanto espera, e a retome em outro processo. Isso vira requisito do runtime (execução durável).
- **Versionamento:** se o código do grafo mudar enquanto uma execução espera 3 dias, ela retoma com o template antigo ou com o novo? O diário precisa estar ligado à **versão do template**.

## W4. Debate entre agentes

```
rodadas r em 1..3:
  nó agente[a]  efeito: llm   para cada a em [otimista, cético, técnico]
                continua: conversa[a]
                lê: respostas da rodada anterior (de todos)   produz: resposta[a]
  barreira: junta respostas (lista por agente)
nó juiz  efeito: llm   lê: respostas finais   produz: veredito
```

**Sai automático:**
- **Concorrência:** os 3 agentes rodam em paralelo dentro de cada rodada; as rodadas são sequenciais.
- **Estado:** cada agente tem a sua `conversation`; o que entra nela (as respostas dos outros) é explícito.
- **Reprodutibilidade:** a troca de informação só acontece na barreira, então o resultado não depende de quem terminou primeiro.

**Quebrou / em aberto:**
- **"Rodadas" precisa ser uma construção da linguagem.** Na discussão da D1, dissemos "snapshot por padrão, rodadas como construção explícita", mas só o snapshot foi registrado. Este workflow mostra que as rodadas são necessárias para agentes que se enxergam, sem quebrar a D1.
- A variante do AutoGen em que **um LLM escolhe quem fala a seguir** vira um laço com `switch`. Funciona, mas é sequencial: o runtime deriva corretamente que não há paralelismo.

## W5. Extração em lote (1000 documentos)

```
limites: 20 threads, 50 requisições/s, orçamento 30 USD

nó extrair[d]     efeito: llm   para cada d em documentos   produz: campos: Contrato
nó validar[d]     efeito: pure  lê: campos[d]               produz: ok | erro
se erro: nó corrigir[d]  efeito: llm   no máximo 2 tentativas
nó consolidar     efeito: pure  junção: lista ordenada
nó gravar_planilha  efeito: write   idempotência: id do lote
```

**Sai automático:**
- **Concorrência:** controlada só pelos limites; o programador não escreve nada sobre paralelismo.
- **Recuperação:** se cair no documento 600, os 599 anteriores vêm do diário.
- **Observabilidade:** custo por documento, por tentativa de correção, por lote.

**Derivações extras (não previstas):**
- **Retentativa derivada do efeito:** erros temporários da API (limite de requisições, erro 500) podem ser repetidos automaticamente em nós `llm` e `read`, que são seguros de repetir. Nós `write once` nunca são repetidos automaticamente.
- **Teto de custo antes de rodar:** 1000 × (extração + até 2 correções) × máximo de tokens × preço. O compilador pode recusar o programa se o teto passar do orçamento declarado, ou avisar.

**Quebrou / em aberto:**
- **Falha parcial não tem semântica definida.** Se um documento falhar de vez (a API recusa, ou continua inválido depois de 2 correções), o que acontece com a junção? A execução inteira falha? O item é pulado? **Proposta:** falhas são **valores** (cada item da junção é `ok(Contrato)` ou `falha(erro)`), e o compilador obriga o programador a tratar o caso de falha.
- **Orçamento estourado no meio:** o runtime para de agendar nós novos e a execução fica **pausada**, podendo ser retomada com mais orçamento (o diário torna isso natural).

## W6. Assistente com memória entre sessões

```
nó lembrar         efeito: read    lê: memória_longa(usuário, mensagem)   produz: lembranças
nó responder       efeito: llm     continua: conversa   lê: lembranças   produz: resposta
nó extrair_fatos   efeito: llm     lê: conversa         produz: fatos
nó memorizar       efeito: write   idempotência: chave do fato
```

**Sai automático:**
- **Dependências:** `extrair_fatos` e `memorizar` não estão no caminho de `responder`.
- **Recuperação e observabilidade:** como nos demais.

**Quebrou / em aberto:**
- **Resposta antes do fim do grafo.** O usuário quer a resposta assim que `responder` termina, sem esperar `memorizar`. A linguagem precisa de um conceito de **resultado antecipado**: o grafo entrega a saída e continua rodando o resto em segundo plano.
- **Execuções simultâneas sobre o mesmo estado externo.** Se o mesmo usuário tem duas sessões abertas, as duas podem gravar fatos conflitantes na memória ao mesmo tempo. A D1 coloca o estado persistente **fora** da linguagem, então a hipótese **não cobre** concorrência *entre execuções*. É um limite honesto: ou isso fica a cargo do armazenamento externo (transações), ou a linguagem ganha um conceito de **recurso compartilhado** com regras de acesso entre execuções.

## W7. Planejar e executar (LLM gera o grafo)

```
nó planejar    efeito: llm   lê: pedido
               produz: plano: grafo<tools: [buscar, ler, resumir]; efeito máximo: read; nós ≤ 20>
nó executar    roda: plano   (verificado antes de rodar)
nó sintetizar  efeito: llm   lê: resultados   produz: resposta
```

**Sai automático:**
- O grafo gerado passa pelo **mesmo verificador** do compilador (D4, nível 3). Se passar, ganha as quatro propriedades como qualquer grafo escrito à mão.
- O **tipo do plano** carrega as restrições (tools permitidas, efeito máximo, tamanho máximo), então o LLM **não consegue** gerar um plano que envie e-mail se o tipo só permite `read`.
- **Recuperação:** o plano é saída de um nó `llm` e fica no diário; na retomada, o mesmo plano é reutilizado.

**Quebrou / em aberto:**
- **O verificador precisa estar disponível em tempo de execução**, embutido no runtime. Isso vira critério para a D10 (plataforma).
- **Em que formato o LLM gera o grafo?** No código-fonte da linguagem ou numa representação intermediária (ex.: JSON)? Uma representação intermediária é mais fácil de gerar e validar.

## W8. Estratégias concorrentes ("vence a primeira")

```
corrida:
  nó tentativa_A  (sandbox própria)  efeito: llm + sandbox
  nó tentativa_B  (sandbox própria)  efeito: llm + sandbox
  nó tentativa_C  (sandbox própria)  efeito: llm + sandbox
vence: a primeira cujos testes passam; as demais são canceladas
```

**Sai automático:**
- **Concorrência:** as três rodam em paralelo.
- **Recuperação:** se o diário registrar **qual venceu**, a retomada reutiliza o vencedor.

**Quebrou / em aberto:**
- **"Vence a primeira" é não-determinístico por natureza:** o resultado depende de tempo. Isso vai contra o motivo do snapshot (D1). O diário contém o problema: o não-determinismo vira um evento gravado, como uma chamada de LLM. A alternativa determinística é "rodar todas e escolher a melhor por critério fixo", mais cara. Provavelmente as duas precisam existir.
- **Cancelamento precisa de semântica.** O que acontece com um ramo cancelado que está no meio de um `write once`? **Proposta:** o cancelamento só acontece **entre nós**, nunca no meio de um nó com efeito externo; ramos dentro de sandbox são simplesmente descartados.
- **Timeouts são uma corrida contra um timer.** Resolver a corrida resolve também os timeouts do W3.

---

## Resultado

### Placar

| | Concorrência | Dependências | Recuperação | Observabilidade |
|---|---|---|---|---|
| W1 Pesquisa recursiva | ✅ | ✅ | ✅ | ✅ |
| W2 Agente de código | ⚠️ pouca (laço sequencial) | ✅ | ⚠️ precisa de sandbox | ✅ |
| W3 Reembolso | ✅ | ✅ | ⚠️ precisa de timers no diário | ✅ |
| W4 Debate | ⚠️ precisa de rodadas | ✅ | ✅ | ✅ |
| W5 Lote | ✅ | ✅ | ⚠️ precisa de falha parcial | ✅ |
| W6 Memória | ✅ dentro da execução / ❌ entre execuções | ✅ | ✅ | ✅ |
| W7 Planejar e executar | ✅ | ✅ | ✅ | ✅ |
| W8 Corrida | ✅ | ✅ | ⚠️ precisa de cancelamento | ✅ |

✅ sai automático com as decisões atuais · ⚠️ sai automático depois de uma decisão nova · ❌ fora do alcance da hipótese

### Veredito

**A hipótese se sustenta *dentro de uma execução*.** Nos 8 workflows, as quatro propriedades saem da estrutura do grafo sem que o programador escreva threads, checkpoints ou instrumentação. As anotações extras que apareceram são todas **limites** (recursão, fan-out, tamanho de saída das tools, prazos), nunca **mecânica de execução**. Isso é coerente com o espírito da hipótese.

**Observabilidade foi a única propriedade que nunca quebrou**, como previsto.

**Recuperação foi a que mais exigiu decisões novas** (sandbox, timers, falha parcial, cancelamento). É onde a linguagem vai precisar de mais engenharia.

**Ela não se sustenta *entre execuções*** (W6). Duas execuções gravando no mesmo estado externo estão fora do que o grafo de uma execução pode ver. A hipótese precisa dizer isso explicitamente.

### Derivações extras (não previstas)

O teste revelou propriedades que também saem do grafo e dos efeitos e que não estavam na hipótese:

1. **Teto de custo antes de rodar** (W1, W5): com laços, recursão e fan-out limitados, o compilador calcula o custo máximo e pode recusar o programa se passar do orçamento.
2. **Retentativa automática derivada do efeito** (W5): `llm` e `read` podem ser repetidos em erros temporários; `write once` nunca.
3. **Tools paralelas no mesmo turno** (W2): a concorrência também sai do tipo de efeito, não só das arestas.

### Decisões novas

| # | Decisão | Origem | Proposta inicial |
|---|---|---|---|
| D11 | Falha parcial em fan-out | W5 | Falhas são valores (`ok` / `falha`); o compilador obriga a tratar |
| D12 | Corrida e cancelamento | W8, W3 | Construção `corrida` explícita; vencedor gravado no diário; cancelamento só entre nós |
| D13 | Ambiente da execução (sandbox) | W2, W8 | Efeitos dentro da sandbox são recuperáveis por snapshot; diário e sandbox restaurados juntos |
| D14 | Tempo e execuções longas | W3 | Timers no diário; runtime durável; diário ligado à versão do template |
| D15 | Concorrência entre execuções | W6 | Fora do escopo inicial; registrar como limite da hipótese |
| D16 | Tamanho máximo da saída das tools | W2 | Tools declaram o máximo, ou o runtime trunca |
| D17 | Recursão de subgrafos | W1 | Permitida com parâmetro que decresce a cada chamada |
| D18 | Rodadas (barreira) | W4 | Construção explícita, já prevista na discussão da D1 |
| D19 | Resultado antecipado | W6 | O grafo pode entregar a saída antes de terminar os nós restantes |

Também ficaram dois critérios novos para a **D10 (plataforma)**: o verificador precisa estar **embutido no runtime** (W7), e o runtime precisa suportar **execução durável** (persistir e retomar execuções longas, W3).

## Próximos passos

1. Revisar a hipótese em [01-hipotese.md](01-hipotese.md) para dizer explicitamente que ela vale **dentro de uma execução** e incluir as derivações extras.
2. Discutir as decisões novas, começando pelas que afetam a recuperação: **D11, D12, D13 e D14**.
3. Ler as referências que cobrem exatamente essas lacunas: **Temporal** (D12, D14) e **LangGraph** (D18, D19).
