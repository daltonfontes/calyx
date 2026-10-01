# Arquitetura do runtime

**Status:** direção decidida, detalhes em aberto.

## Decisão

**Compilador e runtime nativos, com o modelo de atores da BEAM reimplementado**: processos leves, supervisão e mensagens. A linguagem de implementação (C# ou C) continua em aberto.

## Princípio do projeto

> **A Calyx precisa rodar rápido e compilar rápido.**

Consequências diretas:

- **Toda análise do compilador é linear ou composicional.** Cada nó é analisado uma vez, e o resultado de um subgrafo é resumido e reaproveitado. Nada de enumerar caminhos, nada de provador de teoremas (SMT). Isso vale também para o verificador embutido no runtime, que checa grafos gerados por LLM (W7).
- **Compilação incremental:** arquivo por arquivo, recompilando só o que mudou.
- **No runtime, a maior alavanca é fazer menos chamadas de LLM e rodar em paralelo o que é independente** (D3, D6, D24, D29). O custo do runtime por nó (milissegundos) importa menos, mas precisa ser baixo: diário com escrita só no fim do arquivo, gravado em lotes, conteúdos grandes fora dele (D20).
- **Início imediato:** `calyx run` precisa responder em milissegundos, o que um binário nativo permite e a BEAM não.

## Por que reimplementar o modelo da BEAM, e não usar a BEAM

| | BEAM | Nativo com o modelo de atores |
|---|---|---|
| Milhares de execuções esperando ao mesmo tempo | ✅ | ✅ (com E/S assíncrona) |
| Supervisão, mensagens, isolamento | ✅ | ✅ (reimplementado) |
| Tempo para iniciar | Centenas de ms | Milissegundos |
| Compilador e verificador (CPU) | Lentos | Rápidos |
| Embutir em outras aplicações | Difícil | Possível |

## Uma simplificação importante: escalonamento cooperativo basta

A BEAM interrompe processos à força (preempção por contagem de reduções), porque um processo pode calcular por muito tempo. Na Calyx isso não acontece:

- um nó com efeito passa o tempo **esperando E/S** (LLM, tool), não calculando;
- a camada pura (D27) é pequena e com limites, então nunca roda por muito tempo.

Por isso, **escalonamento cooperativo** (cada processo devolve o controle quando espera E/S) é suficiente. É bem mais simples de implementar que a preempção da BEAM.

## Uma segunda simplificação: o processo de uma execução é um dado, não uma pilha

O runtime **interpreta** o grafo compilado (a representação intermediária). Então o estado de uma execução é uma estrutura de dados: nós prontos, nós em andamento, valores já produzidos, posição no diário. Não há pilha de chamadas para guardar.

Consequências:
- um "processo leve" é só essa estrutura mais uma caixa de mensagens; custa poucos bytes;
- **suspender** uma execução (esperar dias, D14) é gravar a estrutura e liberá-la da memória;
- **retomar** é reconstruí-la a partir do diário, que é exatamente o mecanismo de recuperação (D6). Suspender, retomar e se recuperar de uma queda viram a mesma operação.

É a mesma ideia do BendRT, que não usa a pilha de chamadas do C e trata as tarefas como dados.

## Os atores

| Ator | Quantos | Papel |
|---|---|---|
| **Execução** | Um por execução de um grafo | Dono do estado da execução; decide quais nós prontos começam (respeitando limites e prioridade, D3 e D24); recebe os resultados dos nós como mensagens |
| **Chamada** | Um por chamada de efeito em andamento | Faz uma chamada de LLM ou de tool, com timeout (D22) e retentativa conforme o efeito (D2); devolve o resultado à execução |
| **Entidade** | Um por chave (D15) | Dono de um recurso compartilhado (ex.: memória de um usuário); processa mensagens uma por vez (D21) |
| **Diário** | Um (ou um por partição) | Grava eventos só no fim do arquivo, em lotes; é o único que escreve no armazenamento |
| **Supervisor** | Uma árvore | Quando uma execução ou entidade falha, recria a partir do diário |

```text
                 Supervisor
                /          \
        Execução A        Entidade (usuário 42)
        /   |    \               ▲
  Chamada Chamada Chamada        │ mensagens
   (LLM)  (tool)   (LLM)    Execução B
        \   |    /
          Diário  ◄── todos os eventos passam por aqui
```

- **Cancelamento (D12):** a execução manda uma mensagem de cancelamento às chamadas. Chamadas `llm` e `read` param; chamadas `write` terminam antes de responder.
- **Mensagens externas (D21):** chegam na caixa da execução ou da entidade, são gravadas no diário e só então processadas.

## C# ou C

Com essa arquitetura, a comparação fica concreta:

| | C# (com compilação nativa AOT) | C |
|---|---|---|
| Processos leves | `async`/`await` já gera máquinas de estado; canais (`System.Threading.Channels`) servem de caixas de mensagem | Estrutura de dados + laço de eventos próprio (ex.: libuv), escritos à mão |
| E/S assíncrona, HTTP, TLS, JSON, streaming de LLM | Na biblioteca padrão | Bibliotecas externas (libcurl, um parser de JSON), integradas à mão |
| Tempo para iniciar | Dezenas de ms com AOT | Poucos ms |
| Segurança de memória | Gerenciada (coletor de lixo, adequado para carga de E/S) | Manual: risco de falhas de memória num runtime que fica dias no ar |
| Velocidade para escrever o compilador | Alta | Baixa |
| Embutir em outras linguagens | Possível (AOT exporta funções com interface C) | Natural |
| Tamanho do binário | Médio | Mínimo |

Uma terceira opção que cabe nos mesmos critérios, caso valha considerar: **Rust** (nativo, sem coletor de lixo, com segurança de memória e um ecossistema assíncrono maduro), ao custo de uma curva de aprendizado maior.
