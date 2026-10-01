# Plano de avaliação para o paper

Este plano parte da comparação já feita ([`comparacao.md`](comparacao.md)) e
diz o que falta medir, contra o quê, e o que cada resultado provaria ou
derrubaria.

## 1. O objeto da pesquisa

**Um compilador que deriva concorrência e segurança de efeitos a partir de
um grafo com efeitos tipados.** A linguagem é a forma de dar ao compilador a
informação de que ele precisa. O runtime durável é o veículo para provar a
tese, não a contribuição: execução durável já existe (Temporal), checkpoint
por passo também (LangGraph).

O que nenhum dos dois faz, e é a aposta da Calyx, é **recusar o programa
antes de rodar** quando ele pode duplicar um efeito, perder uma atualização
ou deixar a ordem de duas escritas ao acaso. Também é a Calyx que torna a
recuperação correta **por padrão**, sem código manual.

### Tese (uma frase)

> Quando cada passo de um workflow de agentes declara o tipo do seu efeito,
> o compilador consegue derivar o paralelismo e recusar, antes de rodar, a
> maior parte dos bugs de estado. O runtime consegue retomar uma execução
> interrompida sem refazer trabalho nem repetir efeitos, sem código de
> recuperação escrito pelo programador.

### Afirmações e o que as derrubaria

| # | Afirmação | Medida | Derrubada se |
|---|---|---|---|
| A1 | O compilador pega antes de rodar a maioria dos bugs de estado que Python + LangGraph + checadores de tipo deixam passar | Corpus Q2 nas duas linguagens, escrito por pessoas diferentes | A diferença some quando o Python é escrito com tipos e checadores no modo estrito |
| A2 | A recuperação é correta sem código manual: nenhum efeito duplicado, nenhuma chamada de modelo refeita | Matriz de quedas (todos os pontos) × sistemas | Algum ponto de queda duplica um efeito ou refaz uma chamada na Calyx |
| A3 | Todo o paralelismo que o grafo permite sai sozinho, sem palavras de paralelismo no programa | Tempo × caminho crítico, em workloads reais | O tempo fica longe do caminho crítico, ou abaixo do asyncio escrito à mão |
| A4 | O custo do runtime é desprezível perto do custo das chamadas de modelo | Custo por item sem latência, 10 a 10⁵ itens | O custo por item cresce mais que linearmente, ou passa de ~1 ms por item |
| A5 | O diário explica tempo e custo de cada passo sem instrumentação (Q4) | Cobertura do rastro; custo por nó | Algum custo não aparece atribuído a um passo |

## 2. Workloads

Cada um em Calyx e nos baselines, contra o mesmo mundo falso (modelos com
latência fixa, servidores MCP falsos), e os principais também com um modelo
real.

| # | Workload | Exercita | Situação |
|---|---|---|---|
| W1 | Pesquisa com fan-out (N perguntas → busca + resumo → relatório) | Paralelismo derivado, escala | ✅ feito |
| W2 | Reembolso com pagamento e e-mail | Efeitos externos, recuperação | ✅ feito (3 pontos de queda) |
| W3 | Aprovação humana com prazo (`receive`) | Espera durável, prazo que sobrevive a reinício | A fazer; baseline: `interrupt()` do LangGraph, signals do Temporal |
| W4 | Debate em rodadas (`rounds`) | Barreira, paralelismo dentro da rodada | A fazer |
| W5 | Agente de código numa sandbox (`examples/fix.clyx`) | Empréstimos, snapshots, queda no meio de uma edição | A fazer |
| W6 | Corrida entre estratégias e roteador de modelos | Cancelamento, custo | A fazer |
| W7 | Memória entre execuções (entidades) com execuções simultâneas | Atualização perdida, exactly-once por mensagem | A fazer |

## 3. Baselines

| Baseline | Por quê |
|---|---|
| Python sequencial | Piso: o que se escreve sem pensar em paralelismo |
| Python + asyncio escrito à mão | Teto de paralelismo com esforço manual |
| LangGraph (padrão e `durability="sync"`, com e sem cuidado manual) | O framework de grafos de agentes mais usado; o mais próximo da proposta |
| **Temporal (SDK Python)** | **Falta.** É o baseline mais forte para recuperação: execução durável com activities. Sem ele, um revisor dirá que a W2 compara contra o adversário errado |
| AgentSPEX | Citado na hipótese; usar se o código estiver disponível |

## 4. Experimentos

**E1, paralelismo (A3).** W1, W4 e W6 com latência fixa (1 s) e com o
Gemini real, cinco repetições, mediana e intervalo. Reportar o tempo contra o
caminho crítico calculado pelo compilador.

**E2, escala e custo do runtime (A4).** W1 sem latência, de 10 a 10⁵ itens.
Medir também o custo do diário (com e sem `fsync`) e da reavaliação das
expressões, que hoje cresce com o número de chamadas de um passo.

**E3, matriz de quedas (A2).** Para cada workload com efeitos (W2, W3, W5,
W7), matar o processo:
- depois de **cada** entrada do diário (`CALYX_CRASH_AFTER=k`, para todo `k`);
- durante **cada** chamada com efeito (o marcador "em andamento" da tool).

Contar efeitos duplicados, efeitos perdidos, chamadas de modelo refeitas e
casos que pedem decisão humana. No LangGraph e no Temporal, os mesmos pontos.

**E4, corpus de bugs (A1).** Os 52 bugs de `tests/state_bugs/` portados para
Python + LangGraph **por outra pessoa**, com tipos e com pyright e mypy no
modo estrito. Para cada bug, classificar onde ele aparece: antes de rodar,
ao rodar (com ou sem dano) ou em lugar nenhum. Também crescer o corpus com
bugs reais tirados de issues públicas do LangGraph, AutoGen e CrewAI, para
não depender de bugs escritos pelo autor da linguagem.

**E5, custo de escrever (secundário).** Linhas de código efetivas e linhas
de código "de cuidado" (idempotência, conferências, retomada), por workload.
Se possível, um estudo pequeno com programadores: tempo até uma versão
correta, contra LangGraph.

**E6, rastro (A5).** Para cada execução, conferir que todo token e todo
segundo aparecem atribuídos a um passo no diário.

## 5. Ameaças à validade

- **Mesmo autor nas duas linguagens.** É a maior. Mitigação: baselines
  escritos ou revisados por outras pessoas, e bugs tirados de issues reais.
- **Corpus escrito por quem fez o compilador.** O corpus mede o que o
  compilador sabe procurar. Mitigação: o corpus externo do E4.
- **Modelos falsos.** A latência fixa isola o runtime, mas esconde a
  variância dos provedores. Mitigação: repetir E1 com o modelo real.
- **Um computador só, sem rede real.** Mitigação: reportar a máquina e
  repetir num segundo ambiente.
- **Versões.** LangGraph muda rápido (o padrão `durability="async"` é
  recente). Fixar as versões e reportar os dois modos.

## 6. Ordem de trabalho

1. **Temporal na W2** e a **matriz de quedas completa** (E3). É o resultado
   mais forte e o mais exposto a crítica.
2. **E4 com um portador externo** dos 52 bugs.
3. W3 e W7, que exercitam o que nenhum baseline faz por padrão.
4. E2 até 10⁵ itens, e a otimização da reavaliação se ela aparecer.
5. W4 a W6 e o E1 com modelo real.
6. Texto: introdução com o caso do pagamento, que é o exemplo mais claro do
   problema.
