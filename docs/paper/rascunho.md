# Calyx: um compilador que exige o contrato dos efeitos em workflows de agentes

*Rascunho. Os números vêm de [`docs/evaluation/`](../evaluation/) e de
`bench/results/`; o que ainda falta medir está marcado com **[falta]**.*

## Resumo

Workflows de agentes de LLM chamam modelos e tools com efeitos fora da
execução: pagam, mandam e-mails, editam repositórios, esperam pessoas. Os
frameworks atuais (LangGraph, CrewAI, AutoGen) e os motores de execução
durável (Temporal) retomam uma execução interrompida, mas deixam ao
programador o contrato de cada efeito: se pode ser repetido, com que chave,
o que fazer quando não se sabe se aconteceu. Quando esse código falta, o
resultado é o pagamento duplicado, o e-mail repetido, a atualização perdida.

Apresentamos a Calyx, uma linguagem de workflows em que cada tool declara o
seu efeito (`read`, `write` com chave de idempotência, `write once` com uma
política para o resultado incerto) e o compilador deriva do grafo de
dependências o paralelismo e as verificações. Um programa que pode duplicar
um efeito, perder uma atualização ou deixar duas escritas sem ordem é
recusado ou recebe um aviso antes de rodar; o runtime grava cada chamada num
diário e retoma sem refazer trabalho nem repetir efeitos.

Comparada com LangGraph e Temporal nos mesmos workflows, a Calyx acerta todos
os cenários de queda, espera e concorrência medidos (W2: 6/6, W3: 6/6, W7:
3/3), enquanto os baselines acertam de 0% a 83% sem código de cuidado
manual, e todos com ele. **A diferença está no padrão, não no teto:** os
baselines chegam lá quando o programador escreve o cuidado; a Calyx o exige.
O paralelismo derivado empata com asyncio escrito à mão, e o custo do runtime
é linear até 10⁵ itens (0,12–0,16 ms por item). Num corpus de 54 bugs de
estado, o compilador recusa 35 antes de rodar e 48 nunca causam dano; num
estudo de 44 bugs reais relatados nas issues dos três frameworks, porém, o
compilador não pegou nenhum: as issues relatam o framework errando, não o
programador. Discutimos o que isso diz sobre a avaliação de linguagens desse
tipo. No LIMBO, um benchmark de efeitos duplicados de outros autores, os
programas Calyx empatam com os melhores modelos no contrato nativo das tools
(76% de efeito único, contra 74–79%) e chegam a 100% quando toda escrita
aceita chave: a garantia está no contrato, e a Calyx o torna obrigatório.

## 1. Introdução

Um agente de atendimento recebe o pedido de reembolso de uma cliente, propõe
um valor, uma pessoa aprova, ele paga e manda o e-mail de confirmação. Se o
processo cai depois de o pagamento ser feito e antes de o resultado ser
registrado, a retomada vai pagar de novo? Se cai com o e-mail em andamento,
reenvia ou não? Se a aprovação chega depois do prazo, mas antes de alguém
retomar a execução, ela vale?

Nos frameworks atuais essas perguntas têm resposta, mas a resposta é código
que o programador escreve, ou deixa de escrever, sem que nada o lembre.
Medimos isso (seção 6): com o padrão do LangGraph, a mesma queda que a Calyx
atravessa sem dano produz dois pagamentos; com o Temporal, que registra cada
passo num histórico durável, um efeito em andamento no momento da queda é
repetido.

A tese deste trabalho é que **o contrato de um efeito pertence à declaração
da tool, e o compilador deve exigi-lo**:

> Quando cada passo de um workflow de agentes declara o tipo do seu efeito,
> o compilador consegue derivar o paralelismo e recusar, antes de rodar, a
> maior parte dos bugs de estado; o runtime consegue retomar uma execução
> interrompida sem refazer trabalho nem repetir efeitos, sem código de
> recuperação escrito pelo programador.

Os mecanismos de runtime para isso já existem: diário por chamada
(Temporal, Restate), chave de idempotência e conferir antes de repetir
(Mansoor et al., 2026). E há evidência independente de que a garantia de
efeito único depende do contrato da tool, não do modelo: no benchmark LIMBO
(2026), oferecer uma chave de idempotência em toda escrita baixa a duplicação
de 28% para 4%, e nas falhas que uma releitura não resolve (commit atrasado,
reentrega) só o contrato resolve. O que falta é quem **exija** o contrato.

Contribuições:

1. Uma linguagem de workflows de agentes (seção 3) em que o grafo de
   execução é implícito nas dependências de dados, e cada tool declara o seu
   efeito e o que fazer com um resultado incerto.
2. Um conjunto de verificações (seção 4) que usam essas declarações para
   recusar programas que podem duplicar um efeito, perder uma atualização,
   deixar escritas sem ordem, esperar para sempre ou repetir um pagamento a
   cada volta de um laço.
3. Um runtime (seção 5) com diário por chamada, que retoma sem refazer
   trabalho e leva a chave de idempotência até o servidor da tool. Os
   mecanismos não são novos; o que é novo é o compilador exigir as
   declarações que eles usam.
4. Uma avaliação (seção 6) contra LangGraph e Temporal, que inclui a matriz
   completa de quedas, esperas com prazo, memória compartilhada, custo do
   runtime e um estudo de bugs reais, com os resultados desfavoráveis.

## 2. O problema

Os bugs que nos interessam não são de lógica de negócio, mas de **estado**:
o que acontece com os efeitos quando a execução é paralela, interrompida,
repetida ou concorrente. Classificamos sete categorias (tabela 1), tiradas do
estudo de issues da seção 6.6.

| Categoria | Exemplo |
|---|---|
| Efeito repetido | Uma nova tentativa de uma tarefa paga de novo |
| Escrita concorrente | Dois ramos paralelos gravam o mesmo campo |
| Atualização perdida | Duas execuções leem o saldo, somam e gravam |
| Espera | Uma aprovação sem prazo; uma resposta aplicada duas vezes |
| Laço de agente | O agente repete a mesma chamada até o limite |
| Ordem de efeitos | O e-mail de confirmação sai antes do pagamento |
| Recuperação | A retomada refaz trabalho ou restaura um estado inconsistente |

*Tabela 1: categorias de bugs de estado em workflows de agentes.*

O que esses bugs têm em comum é que o programa não diz o que precisa ser
dito: que o pagamento não pode ser repetido sem uma chave, que o e-mail
precisa de uma regra para quando não se sabe se saiu, que o e-mail vem depois
do pagamento. Em Python, essa informação não tem onde ser escrita de forma
que uma ferramenta a confira.

## 3. A linguagem

Um programa Calyx declara modelos, tools, prompts tipados e grafos. A sintaxe
lembra Python; a semântica é de fluxo de dados.

```python
tool refund(request: Text, order: Text, amount: Float) -> Unit:
    effect write
    idempotency_key request
    checks OrderState

tool email(to: Text, subject: Text, body: Text) -> Unit:
    effect write once
    timeout 10 s
    on_uncertain verify(email_sent(to, subject))

graph handle_refund(request: Text, order: Text, message: Text) -> Text:
    found = get_order(order)
    proposal = gemini(decide(found, message))

    paid = refund(request, order, proposal.amount):
        requires state.status == Delivered
        requires state.refunded + proposal.amount <= state.total

    body = gemini(reply(found, proposal.amount))
    notice = email(found.email, "Reembolso do pedido {order}", body)
    notice after paid

    return "reembolso de {proposal.amount} enviado para {found.email}"
```

*Figura 1: o reembolso em Calyx. Nenhuma linha diz o que roda em paralelo
nem o que fazer depois de uma queda.*

**Grafo implícito.** Cada `nome = valor` é um passo; um passo depende dos
que ele usa. O compilador ordena os passos pelas dependências e roda em
paralelo os que não dependem um do outro (na figura 1, `reply` pode rodar
junto com `refund`). `after` acrescenta uma ordem sem dados.

**Efeitos declarados.** Uma tool é `read`, `write` ou `write once`. Uma
`write` pode ser repetida se tiver chave de idempotência; uma `write once`
nunca é repetida sozinha e precisa dizer o que fazer quando o resultado é
incerto (`on_uncertain pause | accept_loss | verify(...)`). `requires`
passa ao servidor da tool condições que ele confere na mesma transação do
efeito.

**Outros construtos**, cada um com regras próprias no compilador: `loop` e
`rounds` com limite obrigatório; `for each` (fan-out); `race` entre
estratégias, com cancelamento dos perdedores; `agent` (o ciclo ReAct), que só
recebe tools sem efeito irreversível; `entity`, estado entre execuções com
handlers puros; `receive`, a espera durável por uma mensagem de fora, com
prazo obrigatório; e um roteador de modelos (`route`) que tenta o mais barato
primeiro.

## 4. O compilador

As verificações usam três coisas: o tipo de cada valor, o efeito de cada
passo e o grafo de dependências. A tabela 2 lista as que dizem respeito a
estado.

| Código | O que recusa ou avisa |
|---|---|
| `E0304` | `write once` sem política para o resultado incerto |
| `W0601` | `write` sem chave de idempotência |
| `W0602` | Duas escritas externas sem ordem definida |
| `W0603` | Um `send` a uma entidade calculado a partir de um `ask` a ela, quando o handler grava um valor novo (atualização perdida) |
| `W0604` | Um ramo de corrida que escreve fora da execução |
| `W0605` | Uma `write once` num laço cujos argumentos não mudam de volta para volta |
| `E0640` | Um agente com uma tool `write once` |
| `E0671` | `receive` sem prazo |
| `E0683` | Corrida sem `on none` |
| `E0645` | Dois ramos editando o mesmo repositório |

*Tabela 2: verificações de estado (parcial; a especificação lista todas).*

Duas delas vieram da avaliação: `W0605`, do estudo de bugs reais (seção
6.6), e o refinamento de `W0603`, que deixou de avisar quando o handler
aplica uma mudança sobre o estado atual (`notas + [nota]`), depois que o
exemplo de atendimento mostrou o falso positivo.

**[falta]** Formalizar as regras de efeito e o que cada verificação garante
(e não garante).

## 5. O runtime

O compilador gera uma representação intermediária em JSON que um runtime em C
executa, com uma camada de E/S em Rust (modelos, MCP, sandbox).

**Diário por chamada.** Cada chamada tem uma chave estável, o seu lugar no
grafo realizado (`grafo/passo[item]#posição`), e cada resposta vai para um
diário só de acréscimo. Na retomada, uma chamada já no diário não é feita de
novo. Para uma `write once`, uma entrada `begin` é gravada antes do efeito:
achar o `begin` sem o fim é saber que o resultado é incerto, e a política da
tool decide.

**Chave até o servidor.** A chave de idempotência e as condições
(`requires`) vão para o servidor da tool nos metadados da chamada MCP. Assim,
mesmo uma repetição causada por um bug do próprio runtime não paga duas
vezes.

**Entidades.** O estado de uma entidade fica em arquivo, com uma mudança por
vez por chave (`flock`) e o id de cada mensagem aplicada gravado junto: uma
mensagem nunca é aplicada duas vezes, mesmo se a execução cair entre a
entidade aplicar e o diário registrar.

**Esperas.** Um `receive` grava o seu prazo absoluto uma vez; a execução sai
(estado `waiting`) e um agendador (`calyx tick`, num cron) a retoma quando
chega a mensagem ou vence o prazo. A hora em que a mensagem chegou é
gravada, e uma resposta atrasada não vale, mesmo que ninguém tenha retomado a
execução ainda (seção 6.3).

## 6. Avaliação

Perguntas: o paralelismo derivado é bom (Q1)? O compilador pega bugs de
estado que Python não pega (Q2)? A recuperação é correta sem código manual
(Q3)? O custo do runtime é desprezível (A4)?

Os baselines são Python sequencial, asyncio escrito à mão, LangGraph 1.2.12
(com `durability` padrão e `sync`) e Temporal (SDK Python 1.34.0, servidor
1.32.0). Todos rodam contra o mesmo mundo falso: modelos com latência fixa e
as mesmas tools MCP (uma loja com pagamentos e e-mail). "Cuidado manual" é o
código que a documentação de cada sistema recomenda e um programador atento
escreve: chave de idempotência, conferir antes de reenviar, prazo guardado no
estado, e assim por diante.

### 6.1 Paralelismo e custo (W1, E2)

Com cada chamada de modelo levando 1 s, a Calyx fica a 30–50 ms do limite
teórico (⌈N/8⌉+1 s), o asyncio escrito à mão a 80–95 ms e o LangGraph a ~0,8 s
(quase tudo inicialização). Sem latência, o custo da Calyx é linear até
100.000 itens, de 0,12 a 0,16 ms por item; o diário custa até 14%. O asyncio é
~5× mais barato por item (a Calyx faz uma chamada MCP de verdade por item),
e o LangGraph chega a 7 ms por item e cresce.

O E2 achou um custo cúbico no número de voltas de um agente: a cada resposta
de uma chamada, o passo do agente era reavaliado desde a primeira volta. A
correção guarda o progresso do agente entre avaliações (400 voltas: de 70 s
para 1,5 s); o que sobra é quadrático e vem do protocolo, que manda a conversa
inteira a cada volta.

### 6.2 Recuperação com efeitos externos (W2)

O reembolso da figura 1, com o processo morto em 6 pontos: depois de cada
passo registrado e com cada efeito em andamento. O certo é 1 pagamento, 1
e-mail e nenhuma chamada de modelo refeita.

| Sistema | Certos | Com cuidado manual |
|---|---|---|
| **Calyx** | **6/6** | — |
| Temporal | 4/6 | 6/6 |
| LangGraph `durability="sync"` | 4/6 | 6/6 |
| LangGraph padrão | 3/6 (e refaz 2 chamadas de modelo) | 6/6 |
| Python sem checkpoint | 2/6 | 6/6 |

*Tabela 3: W2, recuperação depois de `kill -9`.*

Entre passos, Temporal e LangGraph `sync` acertam como a Calyx. Com um efeito
em andamento, nenhum registro por passo resolve: é preciso o contrato do
efeito (chave, ou conferir antes de reenviar), que nos baselines é opcional.

### 6.3 Espera por humano com prazo (W3)

O reembolso com aprovação humana e prazo, com o processo parado durante a
espera, em 6 cenários.

| Cenário | Calyx | LangGraph | Temporal |
|---|---|---|---|
| Aprovada | ✅ | ✅ | ✅ |
| Prazo vence com tudo parado | ✅ | ❌ espera para sempre | ✅ |
| Resposta em dobro | ✅ | ✅ | ✅ |
| Resposta atrasada, depois da recusa | ✅ | ❌ paga | ✅ |
| Resposta atrasada, antes da retomada | ✅ | ❌ paga | ❌ paga |
| Resposta no prazo, retomada depois | ✅ | ✅ | ✅ |

*Tabela 4: W3. Com cuidado manual, LangGraph e Temporal fazem 6/6.*

O LangGraph não tem prazo para um `interrupt()`. O Temporal tem, mas, sem
worker no momento do prazo, o timer e o *signal* atrasado chegam juntos ao
próximo worker, e o SDK entrega o *signal* primeiro. **A Calyx tinha o mesmo
bug**, achado por esta medição e corrigido (a hora da entrega é gravada e
conferida contra o prazo); antes da correção, fazia 5/6.

### 6.4 Memória compartilhada (W7)

Um turno de conversa que soma fatos à memória do usuário, com execuções
simultâneas e quedas entre gravar a memória e registrar a gravação.

| Cenário | Calyx | LangGraph *Store* |
|---|---|---|
| 20 execuções juntas | ✅ | ❌ em 3 de 6 rodadas, perde 1 a 3 conversas |
| Queda depois de gravar, e retomada | ✅ | ❌ grava de novo |
| 10 juntas, todas caem e são retomadas | ✅ | ❌ 30 fatos repetidos |

*Tabela 5: W7. Com cuidado manual (um item por fato, chave pelo id da
mensagem), o LangGraph faz 3/3.*

### 6.5 Bugs antes de rodar (Q2)

Um corpus de 54 bugs de estado (`tests/state_bugs/`), cada um um programa
Calyx com o resultado esperado no cabeçalho: o compilador pega 35, 2 não
podem ser escritos (por construção), o runtime pega 11 e 6 escapam; **48 de
54 nunca causam dano**. Dos 16 portados para Python + LangGraph com tipos, o
pyright e o mypy pegam 2 dos 14 que a Calyx recusa.

O corpus foi escrito por quem fez o compilador, e os 16 portados também. É a
maior ameaça à validade deste resultado. **[falta]** O E4: os 54 bugs
portados por outra pessoa, sem ver as versões em Calyx, com pyright e mypy no
modo estrito (kit em `bench/e4_porting/`).

### 6.6 Bugs reais

Para não depender de bugs escritos pelo autor, buscamos nas issues de
LangGraph, CrewAI e AutoGen, com 8 buscas fixadas antes de ler os resultados,
e classificamos cada relato pelo que a Calyx faria com o mesmo workflow; na
dúvida, a classe menos favorável à Calyx. De 79 issues, 44 entraram.

| Classe | Issues |
|---|---|
| Bug do próprio framework (neutro) | 29 |
| Calyx evita no runtime | 5 |
| Não pode ser escrito em Calyx | 2 |
| **Calyx deixa passar** | **8** |
| **Compilador pega** | **0** |

*Tabela 6: 44 bugs reais de LangGraph, CrewAI e AutoGen.*

O compilador não pegou nenhum. As issues relatam o framework se comportando
mal, não o erro do programador: quem esquece a chave de idempotência não abre
uma issue, descobre o pagamento duplicado em produção. Issues públicas não
medem bem a afirmação Q2, e não a confirmam. O estudo achou uma lacuna real
(um `write once` dentro de um laço de novas tentativas paga a cada volta),
que virou o aviso `W0605`; a contagem acima é a da Calyx anterior ao estudo.

### 6.7 Escrever um workflow real

O exemplo de atendimento ao cliente (`examples/atendimento.clyx`) junta
triagem tipada, um agente só com tools de leitura, reembolso com aprovação e
prazo, e memória do cliente. Rodado com o Gemini, funcionou nos três caminhos
e expôs dois problemas da linguagem, ambos corrigidos: a espera pela
aprovação começava antes de a proposta existir, e não havia como mostrar à
pessoa o que ela aprovava (`receive ... about`); e o `W0603` avisava num caso
em que nada se perdia. Também expôs um limite que a linguagem não resolve: o
modelo prometeu ações que não fez.

### 6.8 Um benchmark de outros autores: LIMBO

O LIMBO (2026) injeta falhas em seis serviços simulados e confere, num
livro-razão, o que cada um fez de fato. Escrevemos as 12 tarefas dele como
programas Calyx, com as tools declaradas só pela documentação que o agente
vê, e as rodamos na grade E2 do artigo (205 episódios com falha), com o
injetor e o avaliador do próprio LIMBO, sem mudar o código dele. Não há
modelo: cada tarefa é um programa fixo, então o que se compara é a
recuperação. Detalhes em `docs/evaluation/limbo.md`.

| | Efeito único (EOS) | Duplicata | Tarefa (TS) |
|---|---|---|---|
| Calyx, contrato nativo | 76% | 23% | 99,5% |
| 3 modelos de ponta, vanilla | 74–79% | 20–26% | 99,5–100% |
| Melhor harness com contratos (`guard`) | 77% | 23% | 100% |
| Oráculo de resultado | 88% | 12% | 100% |
| Calyx, chave em toda escrita | 100% | 0% | 100% |

No contrato nativo, a Calyx empata com os melhores modelos e não os supera.
Todas as suas duplicatas vêm de dois modos que nenhum cliente resolve sem
chave: a reentrega no transporte (74% em todos, inclusive no oráculo) e o
commit atrasado, em que a escrita ainda está em trânsito quando a releitura
olha (68%; o `guard` tem 69%). Nos modos que a releitura resolve, a Calyx
não duplicou nenhuma vez. Quando toda escrita aceita chave, a duplicação
some por construção: a chave está na declaração da tool, e não na decisão
do modelo a cada chamada. O resultado reforça, de fora, a conclusão do
LIMBO: a garantia mora no contrato da tool. A contribuição da Calyx é
tornar esse contrato escrito e conferido, igual em toda execução e sem
tokens.

O LIMBO achou duas lacunas na Calyx, corrigidas antes desses números:
- o `verify` só dizia se a escrita tinha acontecido, e não devolvia o que
  ela fez (o id do ticket); agora a releitura pode devolver o registro, e
  foi esse caminho que evitou a duplicata em 40 dos 205 episódios;
- um servidor de tools não tinha como dizer que o serviço atrás dele não
  respondeu.

Ficaram duas, sem correção:
- o lote que fica pela metade: a resposta a uma pausa não tem como dizer
  "faça só o que falta";
- a pessoa que retoma uma pausa não tem como informar a resposta da tool.

## 7. Ameaças à validade

- **Mesmo autor nas duas linguagens.** Os baselines e o corpus foram escritos
  pelo autor da Calyx. Mitigações: o LIMBO (seção 6.8), com tarefas, falhas,
  avaliador e baselines de outros autores; as regras de "cuidado manual"
  tiradas da documentação de cada sistema; e o E4 **[falta]**. No LIMBO, os
  programas Calyx e o adaptador ainda são do autor, mas o adaptador só usa
  tools públicas do LIMBO.
- **O que a avaliação achou na própria Calyx.** Três bugs da Calyx foram
  achados medindo (resposta depois do prazo, custo cúbico do agente, `write
  once` em laço) e corrigidos antes destes números. Relatamos os números de
  antes onde eles mudam a comparação.
- **Modelos falsos.** A latência fixa isola o runtime e esconde a variância
  dos provedores. **[falta]** Repetir W1 e W2 com um modelo real.
- **Um computador só, sem rede real**, e versões que mudam rápido: fixamos as
  versões e reportamos os dois modos de durabilidade do LangGraph.
- **Estudo de bugs reais:** leitura das issues por resumo, um classificador
  só, amostra limitada à busca do GitHub.

## 8. Trabalhos relacionados

*Levantamento e limitações em [`relacionados.md`](relacionados.md): os
artigos foram lidos pelos resumos da busca, e cada afirmação abaixo precisa
ser conferida no texto antes da submissão.*

**Efeitos duplicados em agentes.** O LIMBO ("Where Does Exactly-Once
Live?", 2026) mede, em 25.930 episódios com 9 modelos e 3 harnesses, onde
deve morar a garantia de efeito único, e conclui que, nas falhas que uma
releitura não resolve, ela depende do contrato da tool. Mansoor et al.
(2026) propõem um wrapper de tool com verificação de pós-condição,
*verify-before-retry* e chave de idempotência, o mesmo mecanismo do
`on_uncertain verify(...)` e da `idempotency_key` da Calyx, como biblioteca
opcional. A Calyx parte da mesma conclusão e torna o contrato obrigatório e
conferido pelo compilador. As anotações de tools do MCP (`readOnlyHint`,
`destructiveHint`, `idempotentHint`) descrevem o mesmo vocabulário como dicas
para o cliente; a Calyx o exige na declaração.

**Verificação estática de agentes.** O Agentproof (2026) extrai um grafo
abstrato de LangGraph, CrewAI, AutoGen e Google ADK e confere propriedades
estruturais e políticas temporais; o IAL-Scan (2026) acha laços infinitos em
projetos reais de agentes; o AgentFlow (2026) analisa dependências entre
prompts e tools. Esses trabalhos analisam programas escritos em frameworks
existentes, e conferem estrutura, ordem de nós ou terminação; a Calyx confere
o contrato dos efeitos externos, que nesses frameworks não está escrito em
lugar nenhum.

**Cálculos e linguagens para programas com LLM.** O λ_A (2026) dá um cálculo
lambda tipado para composição de agentes, com segurança de tipos e terminação
provadas em Coq; modela chamadas de tool como efeito, sem distinguir as
repetíveis das irreversíveis. O LLMbda (Garby, Gordon e Sands, 2026) trata
fluxo de informação e injeção de prompt. Pangolin (Tan et al., 2025) e Wang
(2025) usam efeitos algébricos para compor chamadas a LLM e paralelizá-las.
A formalização das regras de efeito da Calyx (**[falta]**, seção 4) pode
partir do λ_A, estendendo-o com efeitos externos e recuperação.

**Frameworks de agentes.** O ReAct (Yao et al., 2022) intercala raciocínio e
ação num laço decidido pelo modelo; a Calyx o oferece como um construto
(`agent`) com limites obrigatórios. O AutoGen (Wu et al., 2023) organiza
agentes em conversas; o DSPy (Khattab et al., 2023) otimiza os prompts de
pipelines tipados; o AgentSPEX (Wang et al., 2026) descreve workflows em YAML
com checkpoints; o LangGraph torna o grafo explícito e grava checkpoints por
passo.

**Execução durável.** O Temporal e o Restate registram cada passo num diário e
retomam sem refazê-lo; o AWS Durable Execution SDK deixa o programador
escolher, por passo, entre *at-least-once* e *at-most-once*. Nos três, a
idempotência de um efeito externo é responsabilidade do programador. A Calyx
empata com o Temporal na recuperação quando o cuidado é escrito (seção 6.2) e
se diferencia por exigi-lo.

## 9. Conclusão

Medimos a Calyx contra LangGraph e Temporal em quedas, esperas e concorrência,
e o resultado é consistente: **os baselines acertam quando o programador
escreve o cuidado, e a Calyx exige que ele seja escrito**. O custo é baixo
(linear, menos de 0,2 ms por item), e o paralelismo sai das dependências sem
palavras a mais. A parte mais fraca da avaliação é a que mais importa para a
tese: se o compilador pega, antes de rodar, os bugs que programadores reais
cometem. O corpus diz que sim, mas foi escrito pelo autor; as issues públicas
não dizem nem que sim nem que não. No LIMBO, um benchmark de outros
autores, a Calyx empata com os melhores modelos quando as tools não aceitam
chave e chega a zero duplicatas quando aceitam. Isso confirma que a garantia
mora no contrato, e que exigi-lo vale. O E4, um estudo com programadores, é
o próximo passo.
