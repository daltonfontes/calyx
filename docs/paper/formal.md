# As regras de efeito da Calyx, formalizadas

Este documento dá a semântica das chamadas externas da Calyx e o que ela
garante. Ele traz:
- as regras estáticas e dinâmicas;
- três teoremas, com as hipóteses de que dependem;
- os esboços de prova;
- uma verificação exaustiva limitada (`bench/formal/model.py`), que confirma
  os teoremas em programas pequenos e mostra um contraexemplo para cada
  hipótese retirada.

**O que este documento não é.** As provas são de papel, não mecanizadas. O
modelo executável foi escrito à mão a partir de `runtime/src/exec.c` e
`runtime/src/journal.c`, e não extraído deles. Ficam de fora os agentes,
as entidades e as corridas. Seção 6.

## 1. O núcleo

Um programa é uma sequência de passos `x_i = c_i`, em que cada `c_i` é uma
chamada:

```
c ::= m(e…)                      modelo: a resposta pode mudar a cada chamada real
    | t(e…)                      tool, declarada com um efeito:
δ ::= read
    | write key p                escrita com chave: o valor do parâmetro p
    | write                      escrita sem chave (W0601: a tool deve ser idempotente)
    | write once π               escrita única, com política para o resultado incerto
π ::= pause | accept_loss | verify(r)        r: tool read sobre os mesmos parâmetros
```

Os argumentos `e` são expressões puras sobre os parâmetros do grafo e os
passos anteriores. Um grafo com ramos paralelos se reduz a esse núcleo pelas
suas intercalações. A ordem entre escritas que o programa não fixa é o que o
`W0602` avisa (seção 4).

Cada chamada tem uma **chave de diário** `κ_i`, determinada pela posição no
grafo realizado (passo, volta de laço, item de `for each`). Ela não depende
dos valores, então é a mesma em toda retomada.

## 2. Regras estáticas

O compilador recusa (erros) ou avisa:

| Regra | Condição | Código |
|---|---|---|
| S1 | `write once` tem `on_uncertain` | E0304 |
| S2 | `verify(r)`: `r` é `read`, sobre parâmetros da tool, e devolve `Bool`, ou `List[T]` com `T` o tipo da resposta | E0631–E0633 |
| S3 | `accept_loss`, e `verify` com `Bool`, só em tools que devolvem `Unit` (seguir sem resposta exige que não haja resposta a usar) | E0634 |
| S4 | um agente não usa `write once` (o modelo decide quantas vezes chamar) | E0640 |
| S5 | `write` sem chave | aviso W0601 |
| S6 | `write once` num laço com argumentos que não mudam de volta para volta | aviso W0605 |
| S7 | duas escritas sem ordem | aviso W0602 |

S1 garante que **toda** escrita única tem uma regra para o caso incerto, e
S3, que essa regra é sempre aplicável. Sem as duas, a regra D1 abaixo
poderia ficar sem passo.

## 3. Semântica

Uma configuração é `⟨P, J, W⟩`:
- `P`: o programa;
- `J = (J_disco, J_SO)`: o diário. `J_disco` é o que já está no disco;
  `J_SO` foi escrito, mas pode se perder se a máquina cair;
- `W`: o mundo, com o conjunto de efeitos que cada serviço aplicou.

Uma entrada do diário é `begin(κ)` ou `done(κ, v)`. `J(κ)` é a última
entrada para `κ` em `J_disco ++ J_SO`.

**Gravar.** `grava(e, sync)` acrescenta `e` a `J_SO`. Com `sync`, move
`J_SO` inteiro para `J_disco`: o arquivo só cresce, e um `fsync` leva tudo
o que veio antes. `sincroniza` move `J_SO` sem acrescentar nada.

**Transporte.** Mandar um pedido `q` com chave opcional `k` dá um destes
resultados:
- `ok(v)`: aplicado, e a resposta chegou;
- `perdido`: não aplicado; para o cliente, é um `Timeout`;
- `sem_resposta`: aplicado, mas a resposta não chegou; também é `Timeout`.

O cliente não distingue `perdido` de `sem_resposta`. Um serviço que
**respeita chaves** aplica `(t, k)` no máximo uma vez.

Regras para a chamada `x_i = t(ē)` com chave `κ = κ_i`. Cada regra lista as
condições e o que acontece:

- **R0 (já feita).** Condição: `J(κ) = done(κ, v)`. Resultado: `x_i := v`,
  sem chamar.
- **R1 (`read`, modelo).** Chama, repetindo erros temporários até 4 vezes, e
  grava `done(κ, v)` sem sincronizar.
- **R2 (`write key p`).**
  1. Sincroniza o diário.
  2. Manda o pedido com `k = ⟦p⟧`, repetindo erros temporários com a mesma
     chave.
  3. Na resposta, grava `done(κ, v)` sincronizado.
- **R3 (`write once π`).** Condição: `J(κ)` vazio.
  1. Grava `begin(κ)` sincronizado.
  2. Manda o pedido.
  3. Com `ok(v)`, grava `done(κ, v)` sincronizado. Com `Timeout`,
     `Unavailable` ou `Network`, aplica D1.
- **R4 (retomada incerta).** Condição: `J(κ) = begin(κ)` sem `done`.
  Aplica D1.

A regra de decisão **D1** escolhe pela política `π`:

| `π` | Resultado |
|---|---|
| `accept_loss` | grava `done(κ, ())` e segue |
| `verify(r)`, `r` responde que aconteceu (ou acha `v`) | grava `done(κ, v)` e segue |
| `verify(r)`, `r` responde que não aconteceu | volta a R3.2 e manda de novo |
| `pause` | a execução para |
| `pause`, e uma pessoa retoma com `--uncertain d` | `d = done` grava `done(κ, ())`; `d = retry` volta a R3.2 |

**Quedas.** Uma queda pode acontecer entre dois passos quaisquer dessas
regras:
- **queda do processo:** `J_SO` chega ao disco, porque o sistema operacional
  já tinha os dados;
- **queda da máquina:** `J_SO` se perde.

Retomar é executar `P` de novo desde o início, com o mesmo `J`.

## 4. Hipóteses

| | Hipótese | O que a quebra no mundo real | Modo do LIMBO |
|---|---|---|---|
| H1 | O diário obedece ao `fsync` (o que foi sincronizado sobrevive) | disco que mente sobre o `fsync` | — |
| H2 | Um serviço declarado com chave respeita a chave | declarar chave numa tool que a ignora (S5 não pega; as anotações MCP também não) | contrato `native` |
| H3 | `verify` é fresco: vê toda escrita já confirmada e nenhuma em trânsito vira escrita depois da leitura | commit atrasado, listagem com atraso | `timeout_late` |
| H4 | O transporte entrega cada pedido no máximo uma vez | reentrega | `duplicate_delivery` |
| H5 | A pessoa que retoma uma pausa decide conforme o que aconteceu | erro humano | — |

## 5. Teoremas

**T1 (no máximo uma vez).** Sob H1–H5, para qualquer sequência de falhas
de transporte, quedas de processo ou de máquina e retomadas, cada chamada
`write once` e cada `write key p` é aplicada **no máximo uma vez** por
chave de diário.

*Esboço.* Seja `κ` a chave de uma `write once`.

1. Por R3, `begin(κ)` está em `J_disco` antes de qualquer envio, por H1 e
   pela sincronização de R3.1. Logo, toda execução que chega a R3.2 de novo
   para `κ` passa antes por R4. Isso porque R3 exige `J(κ)` vazio, e
   `begin(κ)` nunca sai do diário, que só cresce.
2. Cada passagem por D1 só leva a um novo envio em dois casos:
   - `verify` responde "não aconteceu". Por H3, nenhum envio anterior foi
     aplicado nem vai ser. Por H4, nenhum foi aplicado duas vezes.
   - A pessoa diz `retry`. Por H5, isso também quer dizer que nada foi
     aplicado.
   Então, na hora de cada envio, o número de aplicações de `κ` é zero.
3. Cada envio aplica no máximo uma vez (H4), e o próximo envio só acontece
   depois de outra decisão D1 com zero aplicações. Por indução no número de
   envios, nunca há duas aplicações.

Para `write key p`: o valor `k = ⟦p⟧` só depende dos parâmetros e de
passos anteriores.
1. Por R2.1, toda entrada desses passos está em `J_disco` antes do envio.
2. Depois de qualquer queda, R0 devolve os mesmos valores, e não um novo
   pedido ao modelo. Então `k` é o mesmo em todo envio para `κ`.
3. Por H2, o serviço aplica `(t, k)` no máximo uma vez. ∎

**T2 (exatamente uma vez ao terminar).** Sob H1–H5, se a execução termina,
cada `write key p` e cada `write once` com `verify` ou `pause` foi aplicada
**exatamente** uma vez. Com `accept_loss`, foi aplicada no máximo uma vez.

*Esboço.* Terminar exige `done(κ)` para toda chamada.
1. `done(κ)` só é gravado em quatro casos: depois de `ok(v)`, que implica
   uma aplicação; depois de `verify` achar a escrita, o que por H3 implica
   aplicação; depois de `--uncertain done`, que por H5 implica aplicação; ou
   por `accept_loss`, que não implica.
2. Junto com T1, isso dá exatamente uma vez nos três primeiros casos. ∎

**T3 (nada feito é refeito).** Uma chamada com `done(κ, v)` em `J_disco`
nunca é enviada de novo, e a retomada usa `v`. Isso vale para toda chamada,
inclusive `read` e modelo. Basta H1.

*Esboço.* R0 tem prioridade sobre as demais regras, e o diário só cresce. ∎

**O que T1–T3 não dizem.**
- **Escritas sem chave (`write`, W0601).** O runtime as repete depois de
  erros temporários. Elas só são seguras se a tool for idempotente, e é isso
  que o aviso diz e que a conferência com as anotações MCP (`W0702`)
  checa.
- **A ordem entre escritas** só é garantida quando o programa a fixa (S7).
- **Progresso.** Sob falhas permanentes, a execução para. Os teoremas são de
  segurança, não de progresso.

## 6. Verificação exaustiva limitada

`bench/formal/model.py` implementa as regras R0–R4 e D1, o diário com
`J_disco` e `J_SO`, o transporte e as quedas. Ele enumera **todas** as
escolhas de falha até um limite. O programa de teste tem sete chamadas:
- um modelo;
- uma `write key` cuja chave vem da resposta do modelo;
- uma `write key` com chave constante;
- três `write once`, uma com cada política;
- uma leitura.

Com até 2 falhas de transporte e 2 quedas por execução, considerando todas
as retomadas, as propriedades conferidas são T1, T2 e T3.

| Caso | Execuções | Com violação | Exemplo |
|---|---|---|---|
| Hipóteses valem, quedas de processo | 53.166 | **0** | — |
| Hipóteses valem, quedas de máquina também | 217.928 | **0** | — |
| Sem sincronizar antes de uma `write key` (o bug corrigido, abaixo), quedas de máquina | 257.488 | 18.500 | `pay` aplicada 2 vezes |
| `begin` da `write once` fora do disco, quedas de máquina | 268.360 | 49.346 | `note` aplicada 2 vezes |
| Serviço que ignora a chave (sem H2) | 53.166 | 24.524 | `ref` aplicada 2 vezes |
| Leitura velha e commit atrasado (sem H3) | 101.662 | 11.248 | `mail` aplicada 2 vezes |
| Reentrega no transporte (sem H4) | 95.556 | 24.856 | `note` aplicada 2 vezes |

As três últimas linhas são, no modelo, as três causas de duplicata que o
LIMBO mediu na Calyx (`docs/evaluation/limbo.md`):
- a chave que o serviço ignora (contrato `native`);
- o commit atrasado (`timeout_late`);
- a reentrega (`duplicate_delivery`).

As duas do meio também são as que nenhum cliente resolve: no LIMBO, até o
oráculo duplica sob reentrega. O modelo e o benchmark concordam sobre
**onde** a garantia acaba.

## 7. O que formalizar achou no runtime

Escrever a regra R3 com a hipótese H1 explícita mostrou dois furos, já
corrigidos:

1. **O `begin` podia falhar em silêncio.** `cx_journal_begin` devolvia o
   resultado, mas `run_job` o ignorava. O `fsync` também não era conferido.
   Com o disco cheio, a `write once` saía sem `begin` no disco, e uma queda
   depois dela levava a retomada a mandá-la de novo, sem passar por D1.
   Agora a chamada não sai se o `begin` não chegou ao disco.
2. **Uma `write` com chave não sincronizava o diário antes de sair.** Se a
   chave vem da resposta de um modelo (R1, que grava sem sincronizar) e a
   máquina cai depois do envio, a resposta se perde. Na retomada o modelo é
   chamado de novo, pode responder outra coisa, e a escrita sai com outra
   chave. É a terceira linha da tabela. Agora R2.1 sincroniza antes de toda
   `write`. O custo é um `fsync` por escrita externa, e a `write once` já
   pagava esse custo.

## 8. Relação com o λ_A

O λ_A (2026) dá um cálculo tipado para composição de agentes, com segurança
de tipos e terminação provadas em Coq, e trata chamadas de tool como um
efeito único. O núcleo acima é ortogonal a ele: refina esse efeito em
`read`, `write key` e `write once π`, e acrescenta o diário e as quedas, que
o λ_A não modela. Mecanizar T1–T3 estendendo o λ_A é o caminho natural para
uma versão com prova verificada.

## 9. Limites

- **Fora do núcleo:** agentes (S4 os exclui das escritas únicas), entidades
  (cuja garantia, a mensagem aplicada uma vez, tem argumento próprio: `flock`
  e os ids aplicados), corridas (W0604) e sandboxes.
- **O paralelismo** entra como intercalação. O verificador explora uma
  ordem só, a sequencial; para escritas com ordem não fixada, T1 vale por
  chamada, mas a ordem não é garantida (S7).
- **O modelo é escrito à mão.** A correspondência com o C é conferida por
  leitura e pelos testes de queda do próprio runtime (`CALYX_CRASH_AFTER`,
  `CALYX_CRASH_IN_SEND`), não por construção.
