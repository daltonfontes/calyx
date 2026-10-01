# Calyx

Calyx é uma linguagem de programação **graph-native** para agentes de IA.

O projeto está na fase de **discovery**: ainda não há implementação. O objetivo desta fase é entender o estado da arte e definir a hipótese central e as decisões de design antes de escolher a plataforma (C# ou C).

## Documentos de discovery

| Documento | Conteúdo |
|---|---|
| [Hipótese](docs/discovery/01-hipotese.md) | A ideia central, o que a linguagem precisa garantir e como validar |
| [Papers](docs/discovery/02-papers.md) | Leitura dos trabalhos de referência a partir de 8 perguntas |
| [Decisões](docs/discovery/03-decisoes.md) | Decisões de design, com opções e recomendação |
| [Teste no papel](docs/discovery/04-teste-no-papel.md) | 8 workflows reais usados para testar a hipótese |
| [Temporal](docs/discovery/05-temporal.md) | Leitura do Temporal (execução durável) e impacto nas decisões de recuperação |
| [ReAct como ciclo](docs/discovery/06-react-como-ciclo.md) | Como representar o laço de raciocínio e ação como o primeiro ciclo do grafo |
| [Escalonamento e concorrência](docs/discovery/07-escalonamento-e-concorrencia.md) | Controle de concorrência entre agentes e escalonamento de grafos de tarefas |
| [Bend](docs/discovery/08-bend.md) | Runtime paralelo e tipos afins do Bend, e o que se transfere para a Calyx |
