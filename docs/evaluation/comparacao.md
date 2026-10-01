# Comparação: Calyx × Python × LangGraph

Primeira medição contra baselines. Os mesmos workflows, escritos em Calyx e
em Python (sequencial, asyncio escrito à mão e LangGraph 1.2.12), rodando
contra o mesmo mundo falso: modelos com latência fixa e as mesmas tools MCP.
O código, as regras para ser justo e os comandos para repetir estão em
[`bench/`](../../bench/README.md); os números crus, em `bench/results/`.

**Resumo.**

| Pergunta | Calyx | Melhor baseline | Conclusão |
|---|---|---|---|
| **Q3: recuperação com efeitos externos** (W2) | Certa nos 3 pontos de queda, sem código de recuperação | LangGraph só acerta com cuidado manual; no padrão, paga duas vezes até numa queda **depois** do pagamento | **A diferença mais forte** |
| **Q2: bugs antes de rodar** (14 bugs que a Calyx pega) | 14 de 14 | pyright + mypy: 2 de 14; LangGraph para 3 ao rodar, 2 depois do dano | Forte, mas o corpus foi escrito por quem fez o compilador |
| **Q1: paralelismo** (W1) | A 30–50 ms do limite teórico | asyncio à mão: a 80–95 ms; LangGraph: +0,8 s | Empate com asyncio. O ganho é não escrever o paralelismo, não ser mais rápido |
| **Custo do runtime** (W1 sem latência) | Linear, 0,16 ms por item | asyncio: 0,025 ms; LangGraph: 8,4 ms e crescendo | Desprezível perto de uma chamada de modelo; o LangGraph cresce mais que linearmente |

## W2: recuperação com efeitos externos (Q3)

O reembolso `pedido → decisão (modelo) → pagamento → resposta (modelo) →
e-mail`, contra a loja falsa (MCP), que guarda os pagamentos e os e-mails
num arquivo. O processo morre em três pontos e é retomado do jeito que cada
sistema oferece:

- **depois do pagamento:** o pagamento terminou e a execução o registrou; o
  processo morre antes do passo seguinte (o `save_result` da pergunta
  original);
- **pagamento em andamento:** a loja pagou, mas a resposta ainda não chegou
  quando o processo leva `kill -9`;
- **e-mail em andamento:** o mesmo, com o e-mail.

O certo é **1 pagamento, 1 e-mail e 2 chamadas de modelo**.

| Sistema | Depois do pagamento | Pagamento em andamento | E-mail em andamento |
|---|---|---|---|
| **Calyx** | ✅ 1 pag., 1 e-mail, 2 chamadas | ✅ 1, 1, 2 | ✅ 1, 1, 2 |
| LangGraph (padrão, `durability="async"`) | ❌ **2 pagamentos** | ❌ 2 pagamentos | ❌ 2 e-mails |
| LangGraph `durability="sync"` | ✅ | ❌ 2 pagamentos | ❌ 2 e-mails |
| LangGraph + cuidado manual (os dois modos) | ✅ | ✅ | ✅ |
| Python sem checkpoint | ❌ 2 pagamentos, 3 chamadas | ❌ 2 pagamentos, 3 chamadas | ❌ 2 pag., 2 e-mails, 4 chamadas |
| Python sem checkpoint + cuidado manual | efeitos ✅, 3 chamadas | efeitos ✅, 3 chamadas | efeitos ✅, 4 chamadas |

**O que explica cada linha:**

- **LangGraph no padrão paga duas vezes mesmo quando o processo morre
  _depois_ do pagamento.** Desde a 1.x, o padrão é `durability="async"`: o
  checkpoint de um passo é gravado enquanto o passo seguinte já roda. Se o
  processo morre nesse intervalo, o último checkpoint se perde e o pagamento
  é refeito. Com `durability="sync"`, esse caso fica certo.
- **Com a chamada em andamento, nenhum checkpoint por passo resolve.** O
  efeito aconteceu e a resposta não voltou. Só duas coisas evitam a
  duplicata: uma chave de idempotência que o provedor respeite (pagamento) e
  conferir antes de repetir (e-mail). No LangGraph, as duas são código que o
  programador precisa lembrar de escrever, e nada avisa quando ele esquece.
- **Na Calyx, as duas vêm do contrato da tool:**
  - `refund` declara `idempotency_key request`; a chave vai para a loja e a
    retomada repete com a mesma chave.
  - `email` é `write once` com `on_uncertain verify(email_sent(...))`; a
    retomada encontra o `begin` sem resposta no diário e pergunta antes de
    reenviar.

  O compilador recusa um `write once` sem política (`E0304`) e avisa sobre
  uma escrita sem chave (`W0601`, um aviso, não um erro).
- **Sem checkpoint, o cuidado manual protege os efeitos, mas tudo é
  refeito:** 3 a 4 chamadas de modelo em vez de 2.

**Custo de escrever:** a versão em Calyx tem 42 linhas efetivas, já com os
tipos e os contratos das tools. A versão em LangGraph tem 69 (fluxo + grafo),
das quais o "cuidado manual" são 3. Linhas de código são uma métrica fraca
aqui. O ponto é outro: as 3 linhas são **opcionais** no Python e
**obrigatórias** na Calyx.

## Q2: bugs de estado antes de rodar

16 bugs do corpus (`tests/state_bugs/`, mesmo número) reescritos em Python +
LangGraph **no melhor caso para o Python**: estado em `TypedDict`, funções
anotadas, pyright 1.1.414 e mypy 2.3.1, sem nenhum erro de tipo nas versões
sem o bug. Para cada um: aparece no pyright ou no mypy? Ao rodar? Causa dano?

| # | Bug | Calyx | pyright / mypy | Ao rodar (Python) |
|---|---|---|---|---|
| 01 | Dois ramos paralelos gravam a mesma chave | `E0501` | — | `InvalidUpdateError` (LangGraph para) |
| 02 | E-mail sem plano para queda no meio do envio | `E0304` | — | nada (latente) |
| 03 | Agente com tool de e-mail | `E0640` | — | 6 e-mails, depois `GraphRecursionError` |
| 04 | E-mail pode sair antes do pagamento | `W0602` | — | nada (latente) |
| 06 | Precondição com nome de campo errado | `E0603` | ✅ os dois | `KeyError` |
| 13 | Pagamento sem chave de idempotência | `W0601` | — | **2 pagamentos** |
| 15 | Duas escritas esperando uma pela outra | `E0506` | — | 5 pagamentos e 5 e-mails, depois `GraphRecursionError` |
| 16 | Grafo "só de leitura" que paga | `E0701` | — | 1 pagamento indevido |
| 17 | Resultado que pode falhar usado como sucesso | `E0608` | ✅ os dois | nada nesta execução |
| 23 | Itens paralelos editam o mesmo repositório | `E0644` | — | o arquivo final depende da ordem das edições |
| 33 | Ler, somar e gravar (atualização perdida) | `W0603` | — | saldo 10 em vez de 20 |
| 36 | Mensagem com nome errado para a entidade | `E0655` | — | depósito perdido em silêncio |
| 42 | Espera de aprovação sem prazo | `E0671` | — | nada (espera para sempre) |
| 52 | Condição da corrida chama um modelo | `E0682` | — | uma chamada paga a mais por ramo |
| 20 | Conferir e agir separados, sem `requires` | ninguém | — | reembolso de pedido cancelado |
| 21 | Chave de idempotência errada | ninguém | — | 2º reembolso descartado em silêncio |

- **Dos 14 bugs que a Calyx pega antes de rodar, o pyright e o mypy pegam 2**
  (06 e 17). Os dois são erros de tipo comuns, que um checador já sabe
  procurar. Nenhum bug de efeito, ordem, concorrência ou espera aparece.
- **O LangGraph para 3 ao rodar**, e em dois deles (03, 15) só depois do
  dano: 6 e-mails e 5 pagamentos duplicados antes do limite de recursão.
- **Os 2 bugs que ninguém pega** (20, 21) passam em silêncio nos dois.
  Ficam no corpus de propósito.
- **Viés de seleção:** os 14 primeiros foram escolhidos entre os que a Calyx
  pega, para ver o que o Python faz com eles. A taxa da Calyx no corpus
  inteiro é **34 de 52**, não 14 de 14. O plano do paper pede o corpus
  inteiro portado por outra pessoa e bugs tirados de issues reais.

## W1: paralelismo e escala (Q1)

N perguntas → busca + resumo de cada → relatório. No máximo 8 chamadas ao
mesmo tempo em todas as versões paralelas. Tempo de parede do processo
inteiro (com a inicialização), mediana de 3 repetições.

**Com latência (cada chamada de modelo leva 1 s).** O limite teórico é
`⌈N/8⌉ + 1` s: as rodadas de 8 resumos e depois o relatório.

| N | Limite | Calyx | asyncio à mão | LangGraph | Python sequencial | Calyx `--deterministic` |
|---|---|---|---|---|---|---|
| 5 | 2 s | 2,032 s | 2,089 s | 2,802 s | 6,070 s | 6,035 s |
| 20 | 4 s | 4,037 s | 4,082 s | 4,878 s | 21,084 s | 21,060 s |
| 50 | 8 s | 8,046 s | 8,095 s | 8,826 s | — | — |

- **A Calyx fica a 30–50 ms do limite**, sem nenhuma palavra de paralelismo
  no programa (12 linhas). O asyncio escrito à mão fica a 80–95 ms (14
  linhas, com `gather` e `Semaphore`). O LangGraph fica a ~0,8 s: quase tudo
  é a inicialização da biblioteca (33 linhas, com `Send` e um redutor).
- **Empate com o asyncio.** O ganho da Calyx não é ser mais rápida, é não
  escrever o paralelismo, e o mesmo programa roda em sequência com
  `--deterministic`.
- **O diário não custa nada visível** quando o modelo leva 1 s (2,032 s com
  e sem diário).

**Sem latência (custo do runtime por item).**

| N | Calyx | Calyx sem diário | asyncio | LangGraph |
|---|---|---|---|---|
| 10 | 0,026 s | 0,030 s | 0,070 s | 0,843 s |
| 100 | 0,041 s | 0,042 s | 0,072 s | 0,844 s |
| 1.000 | 0,167 s | 0,174 s | 0,084 s | 2,413 s |
| 10.000 | 1,617 s (0,16 ms/item) | 1,402 s | 0,253 s (0,025 ms/item) | **84,3 s** (8,4 ms/item) |

- **A Calyx é linear:** de 1.000 para 10.000 itens, 9,7× o tempo. O LangGraph
  não é: 35× o tempo para 10× os itens.
- **A inicialização da Calyx é a menor:** 26 ms, contra 70 ms do Python e
  840 ms do LangGraph.
- **Por item, a Calyx é 6× mais cara que o asyncio**, mas a comparação a
  desfavorece: cada item da Calyx faz uma chamada MCP de verdade (um processo
  Python separado, por um pipe), enquanto as versões em Python chamam a busca
  como uma função. Mesmo assim, 0,16 ms por item é ~6.000 vezes menos que uma
  chamada de modelo de 1 s.

## O que a comparação mostra e o que não mostra

**Mostra:**

1. **Recuperação com efeitos externos:** é onde a diferença é maior e mais
   fácil de defender. Nenhum checkpoint por passo, síncrono ou não, resolve a
   queda com a chamada em andamento. Isso exige um contrato do efeito, e na
   Calyx ele é obrigatório.
2. **Bugs de efeito, ordem e concorrência:** checadores de tipo não os veem,
   e o LangGraph, quando os vê, é ao rodar, às vezes depois do dano.
3. **Paralelismo:** sai sozinho e tão bem quanto à mão. Não é uma vantagem de
   velocidade.

**Não mostra:**

- **Resultados com modelos reais.** Os modelos são falsos, com latência fixa.
- **Comparação com o Temporal**, o baseline mais forte para recuperação.
- **Versões escritas por outras pessoas.** O mesmo autor escreveu as duas
  linguagens e o corpus de bugs.
- **Todos os pontos de queda:** foram 3 de muitos.

O [plano de avaliação](plano-paper.md) diz como cobrir cada um desses pontos.
