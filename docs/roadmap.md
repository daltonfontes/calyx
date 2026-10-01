# Plano de implementação

## Estratégia: provar o mais arriscado primeiro

A parte mais barata de mudar é a sintaxe. A parte que ainda não foi provada, e pode derrubar o projeto, é o **modelo de execução**: o paralelismo sai mesmo sozinho do grafo? A retomada pelo diário funciona com chamadas reais? O `check` cabe em 1 segundo?

Por isso a implementação começa por uma **fatia vertical**: um caminho completo, de ponta a ponta, para um subconjunto pequeno da linguagem.

```text
arquivo .clyx → parser → check → representação intermediária → runtime → resultado
```

O runtime começa pelo **interpretador** de grafos (já previsto na arquitetura para grafos gerados por LLM). Gerar C nativo por programa vem depois, quando o modelo de execução estiver provado.

Os exemplos de `examples/` entram aos poucos: em cada marco, os programas de que ele precisa viram **testes**.

## Marcos

Cada marco termina com algo que roda e com uma medida ligada a uma pergunta de pesquisa da [hipótese](discovery/01-hipotese.md).

| Marco | Entrega | Mede |
|---|---|---|
| **M0** | Estrutura do repositório: compilador em Rust (sintaxe, verificação, representação intermediária, CLI), runtime em C, verificador ligado ao runtime como biblioteca estática, testes com programas `.clyx` de referência, CI | O verificador em Rust é chamado de dentro do runtime em C |
| **M1** | `calyx check` para o subconjunto inicial: `model`, `prompt`, `tool` de leitura, `type` (registros), `graph` com `node`, fan-out e `return`. Tipos, variáveis dos prompts, inferência de efeitos, erros estruturados | Tempo do `check` (meta: até 1 s) |
| **M2** | Runtime em C interpretando o grafo numa thread, com chamadas reais de LLM e tools via MCP | O exemplo do README roda de ponta a ponta |
| **M3** | Diário, retomada após queda, `calyx replay` | **Q3:** quanto trabalho é refeito depois de uma falha |
| **M4** | Workers com roubo de trabalho, limites, prioridade pelo caminho crítico | **Q1:** quanto paralelismo sai sozinho |
| **M5** | `loop`, `agent`, `match` com variantes, `try` | O ReAct como ciclo funciona |
| **M6** | `write`, `write once`, `requires`, sandbox, entidades | **Q2:** quantos bugs de estado o compilador pega |
| **Depois** | Geração de C nativo, várias máquinas, roteador, `rounds`, `race` | Desempenho e cobertura da especificação |

## Estado

| Marco | Situação |
|---|---|
| M0 | ✅ Concluído: workspace Rust (`calyx-syntax`, `calyx-check`, `calyx-ir`, `calyx-cli`), lexer completo com diagnósticos estruturados, verificador ligado ao runtime em C como biblioteca estática, testes de referência, CI |
| M1 | ✅ Concluído: parser com recuperação de erros; verificação de nomes, tipos, variáveis dos prompts, contratos das tools, limites, estrutura do grafo (ciclos, `return`) e efeitos (inferência e limite declarado); geração da representação intermediária (`calyx check --ir`); construções de marcos futuros reportadas com o marco em que chegam |
| M2 | Próximo |
| M3–M6 | Não iniciados |

## Medidas

### M1: tempo do `calyx check`

Programas sintéticos (grafos de 21 nós com fan-out), binário de release, máquina de 4 núcleos:

| Tamanho | Tempo | Instruções executadas |
|---|---|---|
| 1,3 mil linhas (50 grafos) | ~3 ms | — |
| 12,5 mil linhas (500 grafos) | 37–39 ms | 342 milhões |
| 125 mil linhas (5000 grafos) | 460–1170 ms (variável entre execuções) | 3,34 bilhões |

- **Meta atingida** para projetos de tamanho médio: dezenas de milissegundos, muito abaixo de 1 s.
- **O algoritmo é linear:** 10× mais código executa 9,75× mais instruções (medido com o callgrind). A variação do tempo de relógio no arquivo grande vem de alocação de memória no ambiente, não do algoritmo.
- **Otimizações possíveis, ainda não necessárias:** cerca de 20% das instruções são alocação (`malloc`/`free`) e 5% são o hash padrão de `HashMap`. Trocar o hash por um mais rápido e reduzir cópias de texto deve reduzir o tempo do arquivo grande.

Para repetir: `cargo run --release -p calyx-check --example phases -- arquivo.clyx`.

### M1: erros encontrados pelo próprio verificador

Ao escrever o `examples/research.clyx`, o verificador pegou um erro real do autor: dentro de um fan-out, a lista inteira de resultados era passada onde o prompt esperava o resultado de uma pergunta (`E0608`).
