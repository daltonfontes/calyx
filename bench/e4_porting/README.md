# E4: os bugs portados por outra pessoa

Obrigado por ajudar. Este experimento mede **onde um bug de workflow de
agentes aparece em Python**: nos checadores de tipo antes de rodar, ao rodar,
ou em lugar nenhum. Os mesmos bugs já foram escritos numa outra linguagem;
para a comparação valer, quem os escreve em Python precisa ser outra pessoa,
escrevendo do seu jeito.

## O que fazer

Para cada bug de [`bugs.md`](bugs.md), escreva um programa em
`programas/bNN_nome.py` (o número com dois dígitos) que:

1. faz o que a coluna "Programa" descreve, **com o erro** da coluna "O erro";
2. roda sozinho, do começo ao fim, com `python programas/bNN_nome.py`
   (inclusive a queda e a retomada, quando o bug tem uma: simule a queda
   como achar mais natural, por exemplo com uma exceção no meio e uma
   segunda chamada que retoma do checkpoint);
3. começa com este cabeçalho:

   ```python
   # Bug NN: <a frase da coluna "O erro">
   # Dano: <onde, no código, o dano acontece, se acontece>
   ```

**Escreva como escreveria normalmente** um código de produção cuidadoso:

- com tipos em tudo (`TypedDict` no estado, funções anotadas);
- com LangGraph (`StateGraph`) quando houver um fluxo de passos, e Python
  comum quando não houver;
- usando o mundo falso de [`world.py`](world.py) (loja, contas, repositório,
  modelo); pode estendê-lo se precisar.

Não tente esconder o erro dos checadores nem facilitar para eles: o erro é o
que um programador cometeria sem perceber.

**Não abra**, até terminar: `tests/state_bugs/`, `bench/q2_bugs/` e
`docs/evaluation/`. Eles têm as outras versões e os resultados esperados.

Se um bug não tiver equivalente natural em Python, crie o arquivo só com o
cabeçalho e `# Não se aplica: <motivo>`. Isso também é um resultado.

## Preparar

```sh
python3 -m venv /tmp/e4 && /tmp/e4/bin/pip install langgraph langgraph-checkpoint-sqlite pyright mypy
```

## Conferir enquanto escreve

```sh
/tmp/e4/bin/python bench/e4_porting/run_e4.py              # todos
/tmp/e4/bin/python bench/e4_porting/run_e4.py b13          # só um
```

O script roda, para cada programa, o **pyright** e o **mypy no modo
estrito** e depois o próprio programa, e diz o que cada um encontrou. Não
mude o programa para "passar" ou "falhar": rode só para ver se ele roda.

## Entregar

Os arquivos de `programas/`, e uma nota curta em `programas/NOTAS.md`:
quanto tempo levou, quais bugs foram difíceis de escrever em Python e por
quê, e qualquer coisa estranha que tenha notado.

## Como os resultados são lidos

Para cada bug, em que ponto ele aparece primeiro:

| Onde | Quando |
|---|---|
| **pyright** ou **mypy** | O checador aponta um erro relacionado ao bug |
| **ao rodar, sem dano** | O programa para (exceção) antes do dano |
| **ao rodar, com dano** | O programa para, mas o dano já aconteceu |
| **em lugar nenhum** | O programa termina normalmente com o dano |

As duas primeiras linhas saem do script. Separar "sem dano" de "com dano" e
conferir se um erro dos checadores é mesmo sobre o bug é feito depois, lendo
o programa, por alguém que não o escreveu.
