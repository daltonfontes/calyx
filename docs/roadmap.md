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
| M1 | Próximo |
| M2–M6 | Não iniciados |
