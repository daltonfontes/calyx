# Temporal

**O que é:** uma plataforma de *execução durável* (durable execution). O programador escreve um processo longo como código comum, e a plataforma garante que ele sobrevive a quedas de processo, de máquina e de rede, sem escrever lógica de recuperação.

**Por que ler:** o [teste no papel](04-teste-no-papel.md) mostrou que a recuperação de falhas é onde a Calyx mais precisa de decisões novas (D11 a D15). O Temporal resolve exatamente tempo, cancelamento, retentativa e execuções longas, só que fora do contexto de agentes.

**Fonte:** documentação oficial, seção *Encyclopedia* do repositório [temporalio/documentation](https://github.com/temporalio/documentation) (versão atual, 2026). Não é um paper; foi lida a documentação conceitual, não o código do servidor.

---

## As 8 perguntas

1. **Problema:** processos longos e distribuídos (pagamentos, provisionamento, pipelines) falham no meio. Normalmente cada time escreve a própria recuperação, com filas, tabelas de estado e retentativas. O Temporal torna a recuperação uma propriedade da plataforma.

2. **Agente:** não existe conceito de agente. Um agente seria um **Workflow** (a orquestração) que chama **Activities** (as chamadas de LLM e as tools). A própria documentação orienta: *"para operações não-determinísticas como chamadas de API, invocações de LLM/IA e consultas a banco, coloque-as em Activities"*.

3. **Workflow:** **código imperativo** numa linguagem comum (Go, Java, Python, TypeScript, .NET, Ruby, Rust, PHP), usando um SDK. Cada chamada ao SDK (executar activity, iniciar timer, iniciar workflow filho) gera um **Command**.

4. **Grafo:** **não há grafo explícito.** A estrutura emerge da execução do código, como no *define-by-run* do DSPy. Mas há uma restrição forte: **a sequência de Commands tem que ser a mesma toda vez que o código é reexecutado** com a mesma entrada (determinismo).

5. **Execução:** três partes separadas.
   - O **Server** grava o **histórico de eventos** (Event History) de cada execução num banco de dados e coloca tarefas em filas (*task queues*).
   - Os **Workers** (processos do usuário) pegam as tarefas, executam o código e devolvem Commands e resultados.
   - O **Client** inicia execuções, envia mensagens e lê resultados.

   Para continuar uma execução, o Worker **reexecuta o código do zero** (*replay*), conferindo cada Command contra o histórico. Os resultados de activities já concluídas vêm do histórico, sem reexecutar. Quando o código chega a um ponto novo, ele roda de verdade.

6. **Estado:** o estado do workflow são as **variáveis do próprio código**, reconstruídas pelo replay do histórico (*event sourcing*). Activities longas podem salvar progresso parcial no *heartbeat*. O histórico tem limite: **51.200 eventos ou 50 MB** por execução. Além disso, é preciso *continue-as-new* (encerrar e começar uma execução nova passando o estado adiante).

7. **Concorrência:**
   - **Dentro de uma execução:** o programador inicia várias activities e espera os *futures*. O paralelismo é **escrito à mão**. O código do workflow em si roda numa thread lógica, determinística.
   - **Entre execuções:** **no máximo uma execução aberta por Workflow ID**. Um ID com significado de negócio (número do pedido, ID do cliente) serializa todas as operações sobre aquele recurso.
   - **Escala:** milhões de timers simultâneos sem consumir recursos do Worker; limite de 2.000 operações pendentes por execução.

8. **Sem solução** (incluindo o que a própria documentação admite):
   - **O determinismo é responsabilidade do programador.** Usar o relógio local, um número aleatório ou reordenar chamadas quebra o replay, e o erro (*non-deterministic error*) só aparece **em tempo de execução**.
   - **Versionamento é difícil.** O *patching* tem o que a documentação chama de "comportamentos potencialmente inesperados". A alternativa (*Worker Versioning*) exige manter Workers antigos rodando até as execuções antigas terminarem.
   - **Idempotência é só recomendação.** Activities são executadas **pelo menos uma vez** e repetidas por padrão. Nas palavras da documentação: *"a falta de idempotência pode afetar a correção da sua aplicação, mas não causa erro na plataforma"*.
   - **O limite de histórico exige particionamento manual** (workflows filhos, continue-as-new).
   - **Cancelamento só chega a activities que enviam heartbeat.** Uma activity sem heartbeat roda até o fim mesmo cancelada.

---

## O que o Temporal confirma na Calyx

| Ideia da Calyx | Correspondente no Temporal | Situação |
|---|---|---|
| Diário de execução (D6) | Event History | ✅ Confirmado: é a base de tudo no Temporal |
| Separar orquestração de efeitos (D2) | Workflow (determinístico) × Activity (efeitos) | ✅ Confirmado, com uma diferença importante (abaixo) |
| Resultado de LLM reaproveitado do diário | Activity concluída não é reexecutada no replay | ✅ Confirmado |
| Retentativa derivada do efeito (teste no papel, W5) | Activities têm retentativa por padrão; workflows não | ✅ Confirmado, com uma diferença importante (abaixo) |
| Timers no diário (D14) | Timers duráveis, persistidos no servidor | ✅ Confirmado |
| Execução durável (D14) | O modelo inteiro | ✅ Confirmado |

## Onde a Calyx vai além do Temporal

Estas diferenças são argumentos concretos para a Calyx ser uma **linguagem** e não uma biblioteca.

### 1. Determinismo garantido pela construção, não pela disciplina

No Temporal, o código do workflow é Python, Go, Java… nada impede o programador de chamar o relógio ou um número aleatório, e o erro só aparece no replay, em produção.

Na Calyx, **o grafo é a própria estrutura de Commands**. A orquestração não tem como chamar o relógio ou gerar um aleatório, porque isso só existe como efeito (`read`) gravado no diário. **A classe inteira de *non-deterministic errors* deixa de existir**, e não por convenção: a linguagem não permite escrever o programa errado.

### 2. Efeitos com tipo, não uma divisão binária

O Temporal só distingue "workflow" de "activity", e trata toda activity igual: repete por padrão e "recomenda" idempotência. Um envio de e-mail sem chave pode sair duas vezes sem nenhum aviso.

A Calyx distingue `llm`, `read`, `write` e `write once`, e o compilador **impede** a retentativa automática de `write once` e exige a política de falha. É exatamente o ponto em que a documentação do Temporal admite deixar a correção por conta do programador.

### 3. Concorrência derivada, não escrita

No Temporal, o paralelismo é escrito à mão (iniciar activities e esperar futures), como no AgentSPEX. Na Calyx, ele sai do grafo.

### 4. Versionamento verificável pelo compilador

No Temporal, saber se um código novo é compatível com uma execução em andamento exige *patching* manual ou testes de replay.

Na Calyx, as duas versões do template são **grafos**. O compilador pode comparar os dois e responder: *os nós que esta execução já concluiu existem, iguais, na versão nova?* Se sim, a execução pode migrar; se não, ela termina na versão antiga. **A compatibilidade de versão vira uma verificação estática**, outra propriedade derivada do grafo que não estava na hipótese.

### 5. Coisas específicas de agentes

O Temporal não sabe nada de LLM: nem de `conversation`, nem de orçamento de contexto, nem de custo em tokens, nem de cache por prefixo. A Calyx trata tudo isso como parte da linguagem.

**Posicionamento resultante:** *a Calyx é a semântica de execução durável do Temporal, compilada a partir de um grafo e com tipos de efeito, aplicada a agentes.*

---

## Impacto nas decisões D11 a D15

### D11. Falha parcial

- **Temporal:** quando uma activity esgota as retentativas, o erro é **lançado como exceção** no código do workflow, que decide o que fazer. Quem implementa a activity pode marcar erros como **não retentáveis** (ex.: entrada inválida): eles falham na hora, sem repetir.
- **Para a Calyx:** mantém a proposta de **falhas como valores** (`ok` / `falha`), que funciona melhor que exceção num fan-out de 1000 itens, porque o compilador obriga a tratar. Adota a ideia de **erros não retentáveis declarados pela tool**: a tool diz quais erros são permanentes, e o runtime não perde tempo repetindo.

### D12. Corrida e cancelamento

- **Temporal:** o cancelamento é **cooperativo**: o workflow recebe o pedido e pode limpar recursos e compensar o que já fez. Existe também o **término forçado**, sem limpeza. Activities só recebem o cancelamento via heartbeat. A limpeza roda num escopo separado, que não é cancelado. Workflows filhos têm uma **política de fechamento do pai**: terminar, pedir cancelamento ou abandonar.
- **Para a Calyx:**
  - Cancelamento cooperativo, **entre nós** (como proposto).
  - Um nó `llm` ou `read` em andamento pode ser **abandonado** (o resultado é descartado).
  - Um nó `write` ou `write once` em andamento é **protegido**: termina antes de o cancelamento valer.
  - Um subgrafo pode declarar **nós de compensação** que rodam se ele for cancelado depois de ter feito escritas (padrão *saga*).
  - Subgrafos têm política de fechamento, como os workflows filhos.

### D13. Sandbox

- **Temporal:** não tem equivalente direto. O mais próximo é o progresso salvo no heartbeat.
- **Para a Calyx:** a proposta segue de pé. O Temporal mostra o custo de não ter sandbox: tudo o que uma activity faz conta como efeito externo.

### D14. Tempo e execuções longas

- **Temporal:**
  - Timers persistidos no servidor; um Worker espera milhões deles sem gastar recursos.
  - **Quatro tipos de timeout por activity:** tempo máximo na fila, por tentativa, total com retentativas, e entre heartbeats. O servidor **não detecta** sozinho que um Worker caiu: depende do timeout por tentativa.
  - Versionamento: execuções **fixadas** (*pinned*: terminam na versão em que começaram) ou com **atualização automática** (*auto-upgrade*: migram, desde que o código seja compatível).
- **Para a Calyx:**
  - Timers no diário, como proposto.
  - **Todo nó com efeito externo precisa de timeout por tentativa**, porque é a única forma de detectar um executor que morreu. O compilador pode exigir ou aplicar um padrão por tipo de efeito.
  - Execuções **fixadas por padrão**, com migração permitida quando o compilador provar compatibilidade (ver "versionamento verificável" acima).
  - O runtime precisa ser **orientado a eventos**: uma execução que espera não ocupa thread nem memória, fica só no banco. Isso vale como critério para a D10.

### D15. Concorrência entre execuções

- **Temporal:** **no máximo uma execução aberta por ID**. O exemplo da documentação: um workflow por host, com o nome do host como ID, *"para garantir que todas as operações no host sejam serializadas"*. Execuções em andamento recebem **mensagens**: *signals* (escrita assíncrona), *updates* (escrita síncrona com resposta) e *queries* (leitura do estado).
- **Para a Calyx:** a D15 **não precisa ficar fora de escopo**. Existe um mecanismo conhecido:
  - Execuções podem ter uma **chave de negócio** (ex.: ID do usuário), com no máximo uma execução aberta por chave.
  - Um recurso compartilhado (a memória do usuário, no W6) é dono de **uma única execução de longa duração** (uma "entidade"), e as outras execuções enviam mensagens para ela em vez de gravar diretamente.

  Assim, a concorrência entre execuções vira **troca de mensagens com uma execução dona do recurso**, e cada execução, individualmente, continua coberta pela hipótese.

---

## Decisões novas

| # | Decisão | Origem | Proposta inicial |
|---|---|---|---|
| D20 | Tamanho do diário | Limite de 51.200 eventos / 50 MB do Temporal; W5 gera milhares de chamadas | Subgrafos podem ter diário próprio (como workflows filhos); conteúdos grandes (prompts, respostas) ficam fora do diário, referenciados por hash |
| D21 | Mensagens para uma execução em andamento | Signals, updates e queries do Temporal; W3 (aprovação), W6 (memória) | Uma execução pode receber mensagens tipadas e responder a consultas de estado; mensagens recebidas vão para o diário |
| D22 | Timeouts | Os 4 timeouts do Temporal | Timeout por tentativa obrigatório em nós com efeito externo; padrão por tipo de efeito |
| D23 | Versionamento de templates | Patching e Worker Versioning | Execuções fixadas na versão por padrão; migração permitida quando o compilador provar compatibilidade entre os grafos |

**Observação sobre D21:** as *queries* do Temporal (ler o estado de uma execução em andamento sem alterá-la) são uma forma de **observabilidade**. Na Calyx, como o estado é explícito no grafo, consultar o estado de qualquer execução em andamento pode sair automaticamente.

## Critérios novos para a D10 (plataforma)

- **Runtime orientado a eventos:** execuções esperando não podem ocupar threads; precisam viver só no armazenamento.
- **Armazenamento do diário:** o Temporal usa um servidor com banco de dados. A Calyx pode começar com um runtime embutido e um diário local (ex.: SQLite), e só depois pensar em execução distribuída.
