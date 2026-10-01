# Arquitetura do runtime

**Status:** direção decidida. Código nativo (o compilador emite C); verificação em até 1 segundo; compilador em Rust; runtime em C com modelo de atores.

## Decisão

**Compilador e runtime nativos, com o modelo de atores da BEAM reimplementado**: processos leves, supervisão e mensagens. Compilador em Rust, runtime em C (ver "Linguagem de implementação" abaixo).

## Princípios do projeto

> **A Calyx compila para código nativo, roda rápido e verifica um programa em até 1 segundo, para que um agente de IA possa verificar a cada mudança.**

Consequências diretas:

- **Toda análise do compilador é linear ou composicional.** Cada nó é analisado uma vez, e o resultado de um subgrafo é resumido e reaproveitado. Nada de enumerar caminhos, nada de provador de teoremas (SMT). Isso vale também para o verificador embutido no runtime, que checa grafos gerados por LLM (W7).
- **Compilação incremental:** arquivo por arquivo, recompilando só o que mudou.
- **No runtime, a maior alavanca é fazer menos chamadas de LLM e rodar em paralelo o que é independente** (D3, D6, D24, D29). O custo do runtime por nó (milissegundos) importa menos, mas precisa ser baixo: diário com escrita só no fim do arquivo, gravado em lotes, conteúdos grandes fora dele (D20).
- **Início imediato:** `calyx run` precisa responder em milissegundos, o que um binário nativo permite e a BEAM não.

### 1. Compilar para código nativo, emitindo C

O compilador da Calyx emite **um arquivo C por programa**, contendo o runtime, o grafo compilado e o código dos efeitos (clientes de LLM, tools). Um compilador C gera o binário.

- Cada nó do grafo vira um **segmento** de uma máquina de estados, **sem usar a pilha de chamadas do C**. O estado de uma execução continua sendo **dado** (nós prontos, nós em andamento, valores, posição no diário), então suspender, retomar e se recuperar continuam sendo a mesma operação.
- **Afinidade no lugar do coletor de lixo** (D26): valores com um dono são liberados pelo código compilado; só o que o compilador detectar como compartilhado (ex.: a mesma `conversation` em vários ramos) recebe contador de referências.
- **Dois caminhos de execução:** o código nativo, para os grafos escritos pelo programador, e um **interpretador pequeno**, dentro do runtime, para grafos gerados por LLM em tempo de execução (W7), depois de verificados.
- **Versionamento (D23):** cada versão do template é um binário. Execuções fixadas numa versão rodam no binário dela.

### 2. O mesmo binário, de um processo a muitos

| Modo | Uso |
|---|---|
| **Uma thread, determinístico** | Testes e replay: a mesma execução sempre na mesma ordem |
| **Várias threads, um processo** | `calyx run` no computador do desenvolvedor |
| **Vários processos ou máquinas** | Produção, com o diário compartilhado |

O **mesmo binário**, escolhendo o modo na hora de rodar. **Todos os modos produzem o mesmo resultado** (garantido pelas junções em ordem fixa, D7).

### 3. Verificar em até 1 segundo, para que um agente de IA verifique a cada mudança

- `calyx check` verifica tipos, efeitos, recursos afins, variantes obrigatórias, terminação, orçamento de custo e orçamento de contexto. **Meta: até 1 segundo** num projeto de tamanho médio.
- É por isso que **toda análise é linear ou composicional** (sem enumerar caminhos, sem provador de teoremas): é o que torna a meta possível.
- `calyx check` é separado de `calyx build`: verificar não exige gerar nem compilar C.
- **Mensagens de erro feitas para agentes:** estruturadas, com esperado, observado e local, para um agente de IA corrigir sozinho.
- **Mais tarde, provas opcionais:** *verificar* uma prova é rápido; *encontrar* a prova é que é caro, e um agente de IA pode escrevê-la. A Calyx pode, no futuro, aceitar propriedades provadas sobre grafos (invariantes, precondições), verificadas em tempo linear.

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

## Linguagem de implementação

**Decidido:**
- **Runtime em C**, porque vai junto, como modelo, no arquivo C gerado.
- **Compilador em Rust.**

**Motivo decisivo:** o verificador (tipos, efeitos, recursos, limites) é usado em dois lugares: no `calyx check` / `calyx build` e **dentro do runtime**, para verificar grafos gerados por LLM antes de rodá-los (W7). Em Rust, o mesmo código do verificador é compilado também como **biblioteca estática com interface C**, sem coletor de lixo e sem runtime extra, e ligado ao runtime em C. Resultado: **um verificador só**, e o que o `check` aceita é exatamente o que o runtime aceita.

Outros motivos: bibliotecas maduras para compilação incremental (ajudam a meta de 1 segundo e um futuro plugin de editor); tipos com variantes e `match` completo; um único binário estático; e um compilador que pega muitos erros antes de rodar, útil quando boa parte do código é escrita ou revisada por agentes de IA.

Custo aceito: curva de aprendizado maior que C#, e duas linguagens no projeto (Rust e C).

Alternativa considerada: C#, que exigiria embutir o .NET em todo binário ou manter um segundo verificador em C para os planos gerados por LLM.

### Comparação anterior (C# ou C)


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
