# State Consistency in Concurrent LLM Agent Workflows (SVBE)

**Paper:** Basavaraju, *State Consistency in Concurrent LLM Agent Workflows: An Experimental Evaluation of Concurrency Control and Semantic Effect-Time Validation*. Preprint no SSRN (submetido à *Science of Computer Programming*), **ainda não revisado por pares**. Código: [bhuvingowda89/paper-agentic-concurrency](https://github.com/bhuvingowda89/paper-agentic-concurrency).

**Base da leitura:** PDF completo, enviado pelo usuário (o SSRN está bloqueado no ambiente). O autor declara ter usado o ChatGPT para organizar e redigir o texto.

---

## As 8 perguntas

1. **Problema:** agentes que alteram sistemas transacionais compartilhados (estoque, saldo, pedidos, alocação de recursos) seguem o padrão **ler → raciocinar → agir**. Entre ler e agir, o LLM pensa por segundos, e outros agentes ou serviços mudam o estado. É a mesma assimetria de tempo do *position paper* de controle de concorrência, agora **medida**.
2. **Agente:** um trabalhador que lê um objeto, raciocina (com atraso simulado ou com um LLM real) e tenta um efeito. A saída do modelo é tratada como **especificação de ação não confiável**, nunca como autorização para alterar o estado.
3. **Workflow:** uma operação lógica por vez: ler, raciocinar, agir.
4. **Grafo:** não se aplica.
5. **Execução:** seis estratégias comparadas:
   - **S0 nenhuma** proteção;
   - **S1 trava pessimista**, segurada durante todo o raciocínio;
   - **S2 OCC por versão**: lê a versão, valida antes de escrever, repete se mudou;
   - **S3 CAS**: a condição de negócio vai escrita à mão dentro do `UPDATE`;
   - **S4 SERIALIZABLE** do PostgreSQL, com retentativa;
   - **S5 SVBE** (*Semantic Validate-Before-Effect*), a proposta do paper.
6. **Estado:** no SVBE, o LLM propõe uma **ação** (de uma lista permitida, com argumentos tipados) **e as precondições que a justificam**, em formato verificável por máquina (igualdade, desigualdade, ordem, pertencimento; sem SQL, sem `eval`, sem código do modelo). No momento do efeito, a infraestrutura abre uma **transação curta**, recarrega e trava o objeto, avalia as precondições sobre o estado **atual** e só então executa a ação confiável, gravando o efeito na mesma transação.
7. **Concorrência:** a trava existe, mas é curta: **nunca é segurada durante o raciocínio do modelo**.
8. **Sem solução / limites** (declarados pelo autor): um único computador, um único banco (PostgreSQL), atrasos de raciocínio em boa parte simulados, um único modelo real, efeitos de uma linha só; no experimento com LLM real, só o SVBE chamou o modelo de verdade, então não há comparação de custo real entre estratégias.

## Resultados principais

| Achado | Número |
|---|---|
| Sem proteção, invariantes violadas | 65% das execuções; **97,5%** com 16 ou 32 operações simultâneas; 0% com uma só |
| Todas as estratégias protegidas | **0 violações** |
| Raciocínio mais longo aumenta violações sem proteção? | **Não**: com sobreposição, a falha já satura. O raciocínio longo aumenta o **custo** de proteger |
| Trava pessimista com 1 s → 5 s de raciocínio | P95 de ~5,9 s → **29,2 s** |
| OCC e SERIALIZABLE | Mesma frequência de conflito, mas cada conflito **joga fora** cada vez mais raciocínio |
| CAS e SVBE com 1 / 3 / 5 s | P95 ≈ 1,1 / 3,1 / **5,1 s**: praticamente só o tempo do raciocínio |
| Estado mudou, mas as precondições continuam válidas | **75,6%** dos casos |
| LLM real gerando o contrato (ação + precondições) | 299/300 exatos; 0 violações em 480 operações concorrentes; janela ler→agir de 2 a 5 s (P95) |

**A conclusão que mais importa:** *"versão mudou"* não é o mesmo que *"a ação deixou de fazer sentido"*. OCC por versão rejeitaria todos esses casos; validar a **semântica** no momento do efeito aproveita três em cada quatro.

---

## O que muda na Calyx

### 1. Um princípio para toda a concorrência da Calyx

Juntando o *position paper*, o STORM e o SVBE, o princípio fica claro:

> **Nunca segurar trava durante a inferência. Validar no momento do efeito.**

A Calyx já seguia isso sem ter dado o nome: snapshot na bifurcação e validação na junção (D1, D25). O SVBE mostra, com medição, que é o caminho certo.

### 2. Três níveis de validação, cada um no seu lugar (D13 resolvida)

| Onde está o estado | Estratégia | Fonte |
|---|---|---|
| Dentro da execução (valores, estado nomeado) | Snapshot na bifurcação + redutor e invariantes na junção | D1, D25 |
| Sandbox / arquivos | Validação pelo **conjunto de leitura** a cada escrita (o diário já sabe o que cada ramo leu) | STORM |
| Sistemas externos transacionais (estoque, saldo, pedidos) | **Precondições semânticas** validadas no momento do efeito | SVBE |

As três evitam travas longas e aproveitam trabalho que continua válido. A escolha entre cópia + merge e validação por conjunto de leitura na D13 pende para a segunda.

### 3. Precondições como parte da linguagem (decisão nova D29)

O SVBE precisa de duas coisas que a Calyx já tem: **saída do modelo tipada** (prompts com tipo de saída) e **efeitos declarados** (tools com contrato). Falta uma: **precondições** na chamada de um efeito.

```
tool refund(order: OrderId, amount: Money) -> Unit {
  effect          write
  idempotency_key order
  checks          OrderState            // estado que a tool expõe para validação no momento do efeito
}

node paid = refund(order, p.amount) requires {
  state.status == Delivered
  state.refunded + p.amount <= state.total
}
```

- O `requires` é escrito na **camada pura** da linguagem (D27): só comparações e operadores permitidos, nunca código arbitrário. Pode vir do programador ou ser **proposto pelo LLM** como parte da saída tipada de um prompt, e o compilador verifica que só usa operadores permitidos.
- A **tool** (o adaptador do sistema externo) é obrigada a avaliar as precondições e executar o efeito **na mesma transação**. Esse é o contrato do `checks`.
- Se uma precondição falhar, o resultado é um valor `Failed(PreconditionFailed { ... })` (D11), que pode voltar para o agente como observação, em vez de refazer todo o raciocínio.

O W3 (reembolso) foi atualizado com esse exemplo.

### 4. Mais um argumento contra travas e retentativas cegas

O paper mostra que OCC e SERIALIZABLE **jogam fora raciocínio** a cada conflito, e que esse desperdício cresce com o tempo de inferência. Na Calyx isso significa: quando um efeito é rejeitado, **não se refaz o nó inteiro por padrão**. O conflito vira um valor, e o programador (ou o agente, como observação) decide o que refazer.
