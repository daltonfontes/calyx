# Guia de operação

Como pôr a Calyx em produção e mantê-la lá: instalar, rodar, guardar o estado, atualizar de versão, trocar chaves e acompanhar. A linguagem e cada garantia estão na [especificação](spec/calyx.md); aqui fica o que quem opera precisa saber.

## 1. Instalar

```sh
curl -fsSL https://raw.githubusercontent.com/daltonfontes/calyx/main/install.sh | CALYX_VERSION=v0.4.1 sh
```

- **Fixe a versão** (`CALYX_VERSION`) em produção. Sem ela, o script instala a mais recente, e duas máquinas instaladas em dias diferentes podem ficar com versões diferentes.
- O binário é estático: não precisa de Rust, de compilador C nem de bibliotecas. Há binários para Linux x86_64 e ARM (musl), para macOS e para Windows x86_64. O script confere o SHA-256 do arquivo baixado.
- O script põe o binário em `~/.calyx/bin/calyx` (ou `$CALYX_INSTALL/bin`) e guarda uma cópia em `~/.calyx/versions/<versão>/calyx`. A cópia serve para atualizar sem perder execuções em andamento (seção 4).
- Em imagens de contêiner, rode o script no build, com `CALYX_INSTALL=/opt/calyx`, e mantenha `/opt/calyx/versions` na imagem.

**No Windows** (PowerShell, sem WSL):

```powershell
$env:CALYX_VERSION = 'v0.4.1'; irm https://raw.githubusercontent.com/daltonfontes/calyx/main/install.ps1 | iex
```

- O `install.ps1` faz o mesmo que o `install.sh`: confere o SHA-256, instala em `~\.calyx\bin\calyx.exe` (ou `$env:CALYX_INSTALL\bin`), guarda a cópia em `versions\<versão>\calyx.exe` e acrescenta `bin` ao PATH do usuário (`CALYX_NO_MODIFY_PATH=1` evita).
- O que muda no Windows:
  - **Servidores MCP em Python:** a Calyx define `PYTHONIOENCODING=utf-8` para os servidores que inicia, porque o MCP é UTF-8 e o Python no Windows escreve na codificação local. Na configuração, use `python` ou `py` em vez de `python3`, conforme o que estiver instalado.
  - **`calyx worker` como serviço:** o equivalente ao systemd é o Agendador de Tarefas ("ao iniciar o sistema", reiniciar se falhar) ou um wrapper de serviço como o NSSM. O `calyx tick` vai no Agendador, a cada minuto.
  - **Chaves privadas de quem assina entregas** (`~\.calyx\keys`) ficam com as permissões da pasta do usuário: no Windows a Calyx não restringe o arquivo como faz no Unix (`0600`).

## 2. Onde fica o estado

A Calyx não tem servidor próprio. O estado fica em arquivos ou no PostgreSQL.

| O quê | Com o diário em arquivo | Com `CALYX_DATABASE_URL` |
|---|---|---|
| Diário de cada execução | `.calyx/runs/<id>/journal.jsonl` e `blobs/` | tabelas `calyx_runs`, `calyx_journal` e `calyx_blobs` |
| Esperas e mensagens (`receive`) | `.calyx/runs/<id>/waits.jsonl` e `inbox.jsonl` | `calyx_files` |
| Entidades | `.calyx/entities/` | `calyx_entities` |
| Chaves de idempotência do `calyx serve` | `.calyx/serve/keys.json` | `.calyx/serve/keys.json`, no diretório do programa |
| Sandboxes | `.calyx/runs/<id>/` | não são aceitas |
| Chaves privadas de quem assina entregas | `~/.calyx/keys/` (ou `$CALYX_KEYS_DIR`) | o mesmo |

- **`.calyx/` fica no diretório em que o comando roda.** Rode `run`, `resume`, `tick` e `deliver` sempre do mesmo diretório; em serviços, fixe o diretório de trabalho.
- **O diário guarda o caminho absoluto do programa.** Para retomar, o `.clyx` precisa estar no mesmo lugar, nesta máquina ou na outra que retoma. Não mova o projeto com execuções em andamento.
- **Uma máquina ou várias.** Uma máquina pode usar o diário em arquivo. Para várias máquinas, ou para que uma máquina que morre não leve execuções junto, use o PostgreSQL ([spec, 9.3](spec/calyx.md#93-diário-e-recuperação-d6-d14-d20)). As tabelas são criadas na primeira conexão; o usuário precisa de `CREATE` no banco nessa hora, e depois só de leitura e escrita.
- **TLS com o banco:** use `sslmode=verify-full` na URL. A Calyx sempre confere o certificado quando usa TLS; `sslrootcert=ARQUIVO` acrescenta uma CA própria.

### Backup

- **Em arquivo:** copie `.calyx/` inteiro. O diário só cresce no fim (nunca é reescrito), então uma cópia feita com execuções rodando no máximo perde as últimas linhas, que a retomada refaz (são leituras ou chamadas de modelo; escritas `write once` têm o `begin` gravado antes).
- **PostgreSQL:** o backup normal do banco (`pg_dump` ou o do serviço gerenciado). Todas as tabelas começam com `calyx_`.

### Limpeza

Nada é apagado sozinho: um diário é o registro do que a execução fez. Apague só execuções terminadas (`finished` ou `failed` em `calyx runs`) que não vão mais ser consultadas:

```sh
rm -rf .calyx/runs/<id>                     # em arquivo
```

```sql
-- PostgreSQL: execuções terminadas há mais de 90 dias
DELETE FROM calyx_journal WHERE run IN (SELECT id FROM calyx_runs WHERE status IN ('finished', 'failed') AND updated < now() - interval '90 days');
DELETE FROM calyx_files   WHERE run IN (SELECT id FROM calyx_runs WHERE status IN ('finished', 'failed') AND updated < now() - interval '90 days');
DELETE FROM calyx_runs    WHERE status IN ('finished', 'failed') AND updated < now() - interval '90 days';
```

`calyx_blobs` é compartilhada (o mesmo conteúdo, de execuções diferentes, é guardado uma vez), então não apague blobs por execução.

## 3. Rodar

| Peça | Como roda em produção |
|---|---|
| Execuções iniciadas por outro sistema | `calyx run programa.clyx --quiet --PARAM valor`, ou um executável de `calyx build` |
| Programa como tool de agentes | `calyx serve programa.clyx --http 127.0.0.1:8080`, atrás de um proxy com TLS ([spec, 11.2](spec/calyx.md#112-como-o-runtime-chama-modelos-e-tools)) |
| Retomar quem caiu e quem recebeu resposta | com PostgreSQL: `calyx worker`, como serviço em cada máquina; em arquivo: `calyx tick` no cron, a cada minuto |
| Respostas de pessoas (`receive`) | `calyx deliver <id> <Mensagem> '<json>' --as <nome>` |

**Códigos de saída**, para quem chama a Calyx de outro sistema:

| Código | Significado |
|---|---|
| 0 | terminou; o resultado vai para o stdout |
| 1 | o programa tem erros (nada rodou) |
| 2 | uso errado ou erro de E/S (arquivo, banco, configuração) |
| 3 | a execução falhou; o que terminou está no diário, e `calyx resume <id>` continua |
| 4 | a execução espera uma mensagem (`receive`): `deliver` e depois `resume` ou `tick` |
| 5 | outro processo já está com essa execução (PostgreSQL) |

**`calyx worker` como serviço** (systemd):

```ini
[Service]
WorkingDirectory=/srv/atendimento
Environment=CALYX_DATABASE_URL=postgres://calyx@db:5432/calyx?sslmode=verify-full
EnvironmentFile=/etc/calyx/segredos.env     # GEMINI_API_KEY=..., tokens das tools
ExecStart=/opt/calyx/bin/calyx worker --quiet
Restart=always
```

O worker sobe um processo por execução que retoma, sem esperar uns pelos outros: uma execução longa não atrasa as demais. `--max N` limita quantas correm ao mesmo tempo em cada máquina (16 por padrão). Cada processo lê o ambiente quando começa. Para que o worker veja segredos novos, reinicie-o.

**Sem PostgreSQL**, o `tick` no cron faz a parte das esperas:

```cron
* * * * * cd /srv/atendimento && /opt/calyx/bin/calyx tick --quiet >> /var/log/calyx-tick.log 2>&1
```

Uma execução que caiu no meio, com o diário em arquivo, não é retomada sozinha: quem a iniciou vê o código de saída diferente de 0 e roda `calyx resume <id>`. `calyx runs` mostra as `interrupted`.

## 4. Atualizar a versão da Calyx

**A regra (D23):** uma execução termina na versão em que começou. Uma versão nova retoma a execução de uma anterior quando compila o programa dela para o mesmo programa. Quando compila diferente, a execução vai para o binário da versão que a começou.

Como funciona:

- O diário de cada execução grava a versão que a começou (`"calyx"` no cabeçalho; `calyx runs` mostra na última coluna) e o hash do programa compilado.
- **O hash ignora os campos vazios da IR.** Quase toda versão acrescenta campos à IR (`decreases`, `policy`...), vazios em programas que não os usam. Até a 0.3.6, o hash era do texto inteiro da IR, e cada campo novo impedia a versão seguinte de retomar qualquer execução. Da 0.3.5 para a 0.3.6, foi assim com todos os exemplos.
- `calyx resume` (e `tick` e `worker`, que usam o `resume`) procura o binário da versão que começou a execução: em `$CALYX_VERSIONS_DIR/<versão>/calyx`, em `versions/` ao lado da instalação e em `~/.calyx/versions/`. Se o binário estiver lá, passa a execução para ele. Se não estiver, a versão atual tenta retomar e só aceita se o programa compilado for o mesmo.
- **Uma mudança no compilador que muda o programa compilado de algum exemplo é vista na CI:** `compiler/calyx-cli/tests/versions.rs` fixa o hash de cada exemplo e de cada programa de teste. Quando a mudança é de propósito, a versão diz isso nas notas da release, e quem atualiza precisa manter o binário anterior até as execuções dele terminarem.

**O procedimento:**

1. Instale a versão nova com o `install.sh` (`CALYX_VERSION=vX.Y.Z`). O script troca `bin/calyx` e guarda a nova em `versions/`; as anteriores continuam em `versions/`.
2. Reinicie os `calyx worker` e o `calyx serve`. Execuções novas começam na versão nova.
3. As execuções em andamento terminam na versão delas, sozinhas, pelo `worker` ou pelo `tick`.
4. Apague `versions/<antiga>` quando `calyx runs` não mostrar mais nenhuma execução `waiting` ou `interrupted` com ela.

Com várias máquinas, atualize todas antes de apagar uma versão antiga em qualquer uma: o `worker` de qualquer máquina pode retomar qualquer execução.

**Execuções da 0.3.6 ou anteriores** não gravam a versão (`<=0.3.6` em `calyx runs`). A 0.3.7 retoma as da 0.3.6 quando o programa compila igual, que é o caso dos exemplos. As outras são retomadas com o binário antigo, chamado direto: `~/.calyx/versions/0.3.6/calyx resume <id>`. Para ter esse binário, rode o `install.sh` com `CALYX_VERSION=v0.3.6` e `CALYX_INSTALL` num diretório à parte, e copie `bin/calyx` para `~/.calyx/versions/0.3.6/calyx`.

**Mudar o programa** (o `.clyx`) com execuções em andamento é a mesma coisa: uma execução termina no programa com que começou, e a retomada recusa um programa diferente. Para mudar um programa com execuções esperando, ponha a versão nova num arquivo novo (`atendimento_v2.clyx`); as execuções antigas terminam no arquivo antigo, e as novas começam no novo.

**Executáveis de `calyx build`** levam o programa e a versão da Calyx dentro. Para atualizar, gere outro executável com outro nome, e mantenha o antigo até as execuções dele terminarem.

## 5. Segredos e chaves

Nenhum segredo fica no `calyx.toml`: o arquivo diz o nome da variável de ambiente (`key_env`, `token_env`, `headers_env`). Carregue os valores de um gerenciador de segredos no ambiente do processo.

| Segredo | Onde é lido | Como trocar |
|---|---|---|
| Chaves dos provedores de modelos (`GEMINI_API_KEY`...) | em cada chamada, do ambiente do processo | mude o ambiente e reinicie o `worker` e o `serve`; um `run` novo já usa a nova |
| Tokens de servidores MCP remotos (`token_env`) | na conexão com o servidor | o mesmo |
| Tokens de quem chama o `calyx serve --http` (`[serve.callers]`) | quando o servidor sobe | ponha o token novo, reinicie o `serve` e depois tire o antigo do cliente |
| Chave de quem assina entregas (`[senders]`) | quando a execução toma a mensagem | abaixo |

**Trocar a chave de quem assina entregas.** Uma entrega é conferida quando a execução a toma, não quando é feita. Por isso, uma entrega assinada com a chave antiga e ainda não tomada é recusada se a chave sair do `[senders]` antes. A ordem:

1. `calyx keys new ana-2026`, e ponha a chave pública nova no `[senders]` com o nome novo.
2. Ponha `ana-2026` ao lado de `ana` em cada `[messages.X] senders`. A pessoa passa a assinar com `--as ana-2026`.
3. Rode um `calyx tick` (ou espere o `worker`) para as entregas pendentes serem tomadas.
4. Tire `ana` do `[senders]` e dos `[messages]`, e apague `~/.calyx/keys/ana.ed25519`.

Uma chave privada vazada sai do `[senders]` na hora: as entregas assinadas com ela e ainda não tomadas são recusadas, e quem pode entregar de novo entrega com a chave nova.

## 6. Acompanhar

- **Traces e métricas:** OpenTelemetry por OTLP/HTTP, configurado com as variáveis `OTEL_*` padrão ou com `[telemetry]` ([spec, 11.3](spec/calyx.md#113-observabilidade-opentelemetry)). Um trace por execução, que continua quando ela é retomada, até em outra máquina. Prompts, argumentos e respostas nunca saem.
- **Alertas que valem a pena:**
  - `calyx.writes.uncertain` maior que zero: uma escrita `write once` de resultado incerto. Dependendo da `on_uncertain` da tool, a execução pode estar pausada esperando uma pessoa (`calyx resume <id> --uncertain done|retry|failed`).
  - `calyx.policy.denials` subindo: um agente tentando o que uma política proíbe, talvez por injeção de prompt.
  - `calyx.runs` com `failed`, e execuções `interrupted` que ficam assim (o `worker` parou?).
  - `calyx.cost` por modelo, contra o orçamento.
- **Sem coletor:** `calyx runs` lista cada execução, com a situação e a versão; `calyx replay <id>` refaz uma execução só com o diário, sem chamar nada, para entender o que ela fez.

## 7. Ensaio de produção

`bench/ensaio/run_ensaio.sh` encena a seção 3 com serviços de verdade, para rodar antes de pôr um programa em produção e depois de cada atualização:

1. a máquina A começa um atendimento: consulta um pedido no Stripe (modo de teste), o Gemini propõe o reembolso, e a execução espera a aprovação;
2. uma aprovação sem assinatura é recusada;
3. o gerente aprova, assinado; o `calyx worker` da máquina B retoma e paga;
4. a máquina B morre (`kill -9`) depois de o Stripe aplicar o reembolso e antes de a resposta chegar;
5. o worker toma a execução de novo e a termina.

No fim, ele confere no Stripe que há um reembolso e um crédito, nem mais nem menos.

```sh
CALYX=~/.calyx/bin/calyx CALYX_DATABASE_URL=postgres://... \
STRIPE_API_KEY=sk_test_... GEMINI_API_KEY=... sh bench/ensaio/run_ensaio.sh
```

O primeiro ensaio, com a 0.3.8, passou (1 reembolso, 1 crédito) e achou três coisas, corrigidas na 0.3.9: o worker tentava a cada passada, para sempre, uma execução que não podia retomar; `timeout 1 day` não era aceito (só `1 days`); e `after` dentro de um bloco dava um erro que não dizia que, num bloco, a ordem é a do texto.

## 8. Antes de ir para produção

- [ ] Versão fixada no `install.sh`, e `versions/` preservado entre deploys
- [ ] `calyx check --tools` sem avisos contra os servidores MCP de produção (efeitos declarados conferem com as anotações)
- [ ] `calyx check --tools --probe` rodado contra o ambiente de teste de cada serviço com `write` e chave de idempotência
- [ ] Preços em `[prices]` para cada modelo, e `budget` nos grafos
- [ ] PostgreSQL com `sslmode=verify-full`, se mais de uma máquina
- [ ] `calyx worker` como serviço em cada máquina (PostgreSQL) ou `calyx tick` no cron (arquivo)
- [ ] `[messages.X] senders` em todo tipo de mensagem que aprova algo, e as chaves privadas só com cada pessoa
- [ ] `calyx serve --http` só em `127.0.0.1`, atrás de um proxy com TLS, com um chamador por cliente
- [ ] Telemetria ligada, com os alertas da seção 6
- [ ] Backup de `.calyx/` ou do banco
- [ ] O ensaio (seção 7) rodado com a versão e o banco de produção, contra o ambiente de teste dos serviços
