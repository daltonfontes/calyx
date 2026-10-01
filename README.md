# Calyx

Calyx é uma linguagem de programação **graph-native** para agentes de IA.

O projeto está na fase de **discovery**: ainda não há implementação. O objetivo desta fase é entender o estado da arte e definir a hipótese central e as decisões de design. Princípios do projeto: **compilar para código nativo, rodar rápido e verificar um programa em até 1 segundo**, para que um agente de IA possa verificar a cada mudança.

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
| [Sintaxe](docs/discovery/09-sintaxe.md) | Proposta de sintaxe testada com 9 programas em `examples/` |
| [SVBE](docs/discovery/10-svbe.md) | Consistência de estado entre agentes concorrentes: validação semântica no momento do efeito |
| [Mapa da orquestração](docs/discovery/11-mapa-orquestracao.md) | Onde a Calyx está na pilha de orquestração de agentes, e o que falta |
| [Arquitetura do runtime](docs/discovery/12-arquitetura-runtime.md) | Compilador e runtime nativos com modelo de atores; princípio de rodar e compilar rápido |
| [Concorrência](docs/discovery/13-concorrencia.md) | Os três problemas da concorrência (descobrir, executar, estado) e o modelo de concorrência da Calyx |
