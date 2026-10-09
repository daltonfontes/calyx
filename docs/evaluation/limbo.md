# A Calyx no LIMBO

Rodada em 2026-10-09, LIMBO no commit `2db09fb`. Kit, programas e como
reproduzir: [`bench/limbo/`](../../bench/limbo/). Dados:
`bench/results/limbo_native.jsonl`, `limbo_keys_everywhere.jsonl` e o resumo
`limbo.json`.

## Por que este experimento

Nos outros experimentos, o autor da Calyx escreveu os baselines, o mundo
falso e o corpus de bugs. O [LIMBO](https://github.com/jaxblack/limbo-bench)
("Where Does Exactly-Once Live?", 2026) é de outros autores. Ele traz:
- seis serviços simulados;
- doze tarefas;
- um injetor de falhas no serviço;
- um livro-razão do que cada serviço de fato fez;
- um avaliador próprio;
- números publicados para nove modelos e onze políticas de recuperação.

Aqui as doze tarefas viram programas Calyx e rodam nesse ambiente sem
nenhuma mudança no código do LIMBO.

## Como foi feito

- **Grade.** É a mesma do experimento E2 do LIMBO: as 12 tarefas, cada
  escrita focal e 8 modos de falha, na instância 0. Os episódios são gerados
  pela própria função `runner.expand` do LIMBO. São 217 episódios, e em 205
  deles a falha disparou: o mesmo `n` por condição do artigo.
- **Ambiente.** As classes do LIMBO montam cada episódio: `SandboxSession`
  com a política `vanilla`, ou seja, nenhum cuidado do harness dele. As
  tools passam por um adaptador MCP nosso (`bench/limbo/adapter.py`), que
  chama o mesmo endpoint local usado pelo proxy MCP do próprio LIMBO.
- **Declarações das tools.** Foram escritas lendo só a documentação que o
  agente vê (`programs/tools_native.clyx`):
  - **Escritas com chave:** a cobrança e o post no mastodon.
  - **Escritas idempotentes por natureza:** upsert, mudança de estado do
    ticket e reembolso.
  - **`write once` com `verify`:** as demais. O `verify` procura a escrita
    com as leituras públicas e espera o atraso que a documentação anuncia
    (weibo, 3 min; pasta de enviados, 2 min).
  - **`write once` com `pause`:** duas escritas que não há como conferir.
    O post no x não tem listagem. O lote de linhas pode ficar pela metade, e
    o `verify` só responde sim ou não.
- **A pausa.** Quando a execução para, o executor faz o papel da pessoa.
  Ele pergunta ao operador do LIMBO (`escalate_to_human`, 15 min simulados,
  como para os agentes) e retoma com `--uncertain done`, `retry` ou `failed`.
- **O fim.** O executor chama o `finish` do LIMBO com `completed` só se a
  execução terminou bem, e o avaliador do LIMBO dá a nota.
- **Segundo contrato.** No `keys_everywhere`, a documentação diz que toda
  escrita aceita chave. Os programas das tarefas são os mesmos; só mudam as
  declarações (`programs/tools_keys.clyx`), e toda escrita vira `write` com
  chave.

**Não há modelo.** Cada tarefa é um programa fixo, com os textos da
instância como parâmetros. O que se mede é a camada de execução: o que
acontece com cada escrita quando o serviço falha. Os agentes do artigo
também precisam entender a instrução, e é por isso que a comparação abaixo é
de recuperação, não de agente contra agente.

## Resultados

Episódios com a falha disparada (205 por linha). Os números do LIMBO vêm de
`paper/generated/numbers.json` do repositório deles (E2, `vanilla`, `guard`
e o oráculo de resultado).

| Contrato `native` | EOS | Duplicata | TS | Chamadas | Pessoa (min) |
|---|---|---|---|---|---|
| **Calyx** | **76,1%** | **23,4%** | **99,5%** | 7,8 | 0,8 |
| gpt-6-sol, vanilla | 79,0% | 20,5% | 99,5% | 8,5¹ | 0,7¹ |
| claude-opus-5.5, vanilla | 76,6% | 23,4% | 100% | | |
| gemini-3.8-flash, vanilla | 74,2% | 25,9% | 100% | | |
| melhor `guard` (harness com contratos) | 77,1% | 22,9% | 100% | 8,4¹ | 0,4¹ |
| oráculo de resultado (teto do cliente) | 87,8% | 12,2% | 100% | | |

¹ Média dos três modelos na política (o artigo só publica essa média).

Taxa de duplicata por modo de falha (e2k do LIMBO, 3 modelos agregados):

| Modo | Calyx `native` | LIMBO `native` vanilla | LIMBO `native` guard | Calyx `keys` | LIMBO `keys` vanilla |
|---|---|---|---|---|---|
| timeout depois do commit | 0% | 0% | 0% | 0% | 0% |
| 500 depois do commit | 0% | 8% | 0% | 0% | 0% |
| commit atrasado (90 s) | 68% | 58% | 69% | 0% | 2% |
| lote pela metade | 0% (1 ep.) | 33% (3 ep.) | 0% | 0% | 0% |
| reentrega no transporte | 74% | 74% | 74% | 0% | 0% |

No contrato `keys_everywhere`, a Calyx teve 100% de EOS, 0% de duplicata e
100% de TS nos 205 episódios, com 7,2 chamadas por episódio e nenhuma pausa.

## Leitura

1. **No contrato nativo, a Calyx empata com os melhores modelos e não os
   supera.** O EOS de 76% fica dentro da faixa dos três modelos do E2
   (74–79%) e do `guard`. Todas as duplicatas vêm de dois modos que nenhum
   cliente resolve sem chave:
   - **Reentrega:** o transporte entrega o mesmo pedido duas vezes. São 74%
     em todos, inclusive no oráculo.
   - **Commit atrasado:** a escrita ainda está em trânsito quando o `verify`
     olha. Ele não a vê e manda repetir. Com a lista atrasando de novo (o
     e-mail atrasa 90 s para chegar e mais 2 min para aparecer), nem esperar
     o atraso documentado resolve. O `guard` do LIMBO tem o mesmo problema
     (69%).

   Nos modos que uma releitura resolve (timeout e 500 depois do commit), a
   Calyx não duplicou nenhuma vez.
2. **Com chave em toda escrita, a duplicação some, e por construção.** As
   declarações fazem toda escrita mandar a sua chave, e o runtime repete
   sozinho os erros temporários. Os modelos do artigo chegam a 0–2%, mandando a
   chave em 99–100% das escritas. A diferença não
   está no número: está em que, na Calyx, a chave é parte da declaração que
   o compilador confere, e não um comportamento do modelo a cada chamada.
3. **O resultado confirma a conclusão do LIMBO, agora de fora.** Uma
   linguagem que só torna o contrato explícito e obrigatório chega ao mesmo
   lugar dos melhores modelos com o contrato nativo, e a zero com o contrato
   bom. A garantia mora no contrato da tool. O que a Calyx acrescenta é
   tornar esse contrato escrito, conferido e igual em toda execução, sem
   tokens: o executor não gasta nenhum, contra cerca de 14 mil por episódio
   dos agentes do artigo.
4. **O custo de ser cuidadoso aparece onde não há como conferir.** As duas
   escritas com `pause` (post no x e lote) pararam para a pessoa em 11
   episódios. Em média são 0,8 min de pessoa por episódio, contra 0,4–0,7
   min nos agentes. Esperar a pessoa salvou o commit atrasado no x: durante
   os 15 min o post apareceu, e o operador respondeu "aconteceu". No lote
   pela metade, a única falha de TS, a Calyx parou sem duplicar, mas não
   tinha como terminar. A resposta à pausa é `done`, `retry` ou `failed`, e
   nenhuma diz "faça só o que falta".

## O que o LIMBO achou na Calyx

Duas lacunas apareceram ao escrever os programas, e foram corrigidas antes
dos números acima:

- **O `verify` só respondia sim ou não.** Uma tool que devolve algo (o id do
  ticket, da mensagem, do deploy) não podia usar `verify`. Achar a escrita
  não dava a resposta que o resto do programa usa, e o compilador exigia
  `pause` (`E0634`). Agora a leitura do `verify` pode devolver `List[T]`, o
  que a chamada fez, achado de novo: vazia, a chamada é repetida; senão, o
  primeiro item vira a resposta, gravada no diário. Em 40 dos 205 episódios
  foi esse caminho que evitou a duplicata. Antes da mudança, esses 40
  episódios teriam parado para uma pessoa.
- **Um servidor de tools na frente de outro serviço não tinha como dizer
  "o serviço não respondeu".** O erro virava `ToolError`, e uma `write once`
  falhava em vez de aplicar o `on_uncertain`. Agora um erro com texto
  começando por `Timeout:`, `Unavailable:`, `RateLimit:` ou `Network:` é
  tratado como esse erro temporário.

Ficaram duas lacunas, relatadas e não corrigidas:

- o lote pela metade (falta um "faça só o que falta");
- o `--uncertain done` de uma tool que devolve algo: a pessoa não tem como
  informar a resposta. Por isso o post no x devolve `Unit` e o e-mail do
  `cross_post` não traz o id dele. O avaliador do LIMBO não confere esse id.

## Declarações erradas: o que as anotações MCP pegam

A fraqueza das declarações da Calyx é a própria declaração: se o
programador declara errado o efeito de uma tool, o compilador acredita.
Os servidores MCP podem descrever cada tool com anotações (`readOnlyHint`,
`idempotentHint`, `destructiveHint`), e o LIMBO as publica. Agora a Calyx
confere as duas coisas:
- `calyx check --tools` confere sem rodar;
- o runtime confere na primeira chamada de cada tool.

O estudo está em `bench/limbo/declarations.py`, e os dados em
`bench/results/limbo_declarations.json`.

**Como foi medido.**
- Cada uma das 21 tools dos programas foi declarada, uma vez cada, das
  quatro formas possíveis: `read`, `write` sem chave, `write` com chave e
  `write once`.
- Cada declaração foi conferida contra um ambiente do LIMBO.
- Cada declaração foi classificada pelos contratos internos do LIMBO, que o
  agente nunca vê:
  - **perigosa:** o runtime pode repetir uma escrita que não é idempotente.
    É o caso de uma escrita assim declarada `read`, `write` sem chave, ou
    com uma chave que o serviço ignora;
  - **segura:** a declaração está certa ou só é mais cuidadosa que o
    necessário.

| Declaração perigosa | Avisadas |
|---|---|
| escrita não idempotente declarada `read` | 10 de 10 (`W0701`) |
| escrita não idempotente declarada `write` sem chave | 10 de 10 (`W0702`) |
| `write` com uma chave que o serviço ignora | **0 de 8** |

Das 56 declarações seguras, 3 receberam aviso: são escritas idempotentes
(reembolso, mudança de estado e upsert) declaradas `read`. Repetir essas
escritas não faz mal, mas o aviso está certo: elas mudam coisas.

**Leitura.** As anotações pegam os dois erros mais comuns: tratar uma
escrita como leitura, e esquecer que uma escrita sem chave vai ser
repetida. Não pegam o erro que mais importou no LIMBO: confiar numa chave
que o serviço não respeita. Isso não é um defeito da conferência. As
anotações MCP não têm vocabulário para chaves de idempotência. Por exemplo,
`social_publish` aceita chave no mastodon e a ignora nas outras
plataformas, e nada nas anotações diz isso. Uma anotação nova no MCP, algo
como "aceita chave de idempotência", fecharia a lacuna. Até lá, essa parte
do contrato continua só na declaração.

## Ameaças

- **Sem modelo.** Os programas não interpretam a instrução, e a Calyx não
  tem o trabalho de um agente de achar os passos. A comparação justa é com
  as políticas de recuperação do LIMBO, que também são fixas; o `guard` é a
  mais próxima. Ela também vale para quem escreve um workflow fixo, que é o
  caso de uso da Calyx.
- **Programas e adaptador escritos pelo autor da Calyx.** O que o adaptador
  faz está listado no README do kit. As leituras do `verify` usam só tools
  públicas do LIMBO e os atrasos que a documentação dele anuncia, nunca o
  livro-razão.
- **Uma instância por tarefa**, como no E2 do artigo. A rodada é
  determinística (`--deterministic`; o mundo do LIMBO tem semente).
- **Os números do LIMBO são os publicados**, não rodados aqui: rodar os
  modelos pediria as chaves de cada provedor.
