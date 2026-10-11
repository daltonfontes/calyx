# Fuzzing do runtime em C

O runtime em C lê o que vem de fora:

- as respostas de modelos e tools;
- o diário de uma execução retomada;
- o IR, que pode ser um grafo escrito por um modelo, conferido antes pelo verificador.

Aqui cada uma dessas entradas é testada com o libFuzzer, sob AddressSanitizer e UBSan. Os alvos ligam só os fontes em C. O lugar da camada Rust fica com `stubs.c`, que não tem banco nem telemetria, e cujas respostas de modelos e tools vêm do fuzzer.

| Alvo | Entrada | O que tem de valer |
|---|---|---|
| `fuzz_json` | bytes quaisquer | o parser nunca quebra; o que ele aceita é escrito de volta como um texto que dá o mesmo valor |
| `fuzz_journal` | as linhas de um `journal.jsonl` (cortado, corrompido, escrito à mão) | abrir para retomar e para refazer, e procurar chamadas, nunca quebra |
| `fuzz_answers` | as respostas de modelos e tools de `answers.clyx`, na ordem das chamadas, separadas por `0x1e` | todo resultado é um valor ou uma falha: decodificar no tipo, guardar só os campos declarados, cortar em `max_output`, as chamadas de um agente, políticas, novas tentativas |
| `fuzz_ir` | um IR qualquer | o interpretador dá um resultado ou um erro, nunca acessa memória fora do lugar |

## Como rodar

São necessários o clang 18 e o `libclang-rt-18-dev`.

```sh
make -C runtime fuzz                          # compila os alvos em runtime/build/
make -C runtime fuzz-run FUZZ_SECONDS=600     # cada alvo por 10 min sobre o seu corpus
```

O que quebra fica em `runtime/build/crash-<alvo>-*`. Para reproduzir um caso:

```sh
CALYX_FUZZ_IR=runtime/fuzz/answers.ir.json runtime/build/fuzz_answers runtime/build/crash-answers-...
```

Uma entrada que quebrou e foi corrigida entra em `corpus/<alvo>/regression-*`, e assim nunca mais deixa de ser testada. O CI roda cada alvo por um minuto sobre esse corpus em todo PR (job `fuzz`). Compilado com `-DCALYX_FUZZ`, o runtime não espera entre novas tentativas.

## O que foi achado

1. **Infinito virava `null`.** Um número grande demais para um double (`123490023455e23456891`) era lido como infinito e escrito no diário como `null`. A execução retomada via outro valor. Agora o parser o recusa (`number out of range`), como o `serde_json` faz.
2. **Índices do IR sem conferência.** Um IR que nomeava um parâmetro ou uma variável local além do fim do vetor lia e escrevia fora dele. Agora todo índice de parâmetro, de variável local e de `case` é conferido.
3. **Conversão de número para inteiro sem conferência.** Um modelo que informava `input_tokens = 1.1e61` tornava indefinida a conversão para inteiro. A mesma conversão era feita em índices do IR (tools, modelos, entradas, o `N` de `race first N`, `max_output`). Agora números viram índices ou contagens só por `as_index` e `as_count`, e itens de listas são lidos por `str_at`.
