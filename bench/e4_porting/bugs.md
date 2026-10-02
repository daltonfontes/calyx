# Os 54 bugs

Cada um é um workflow de agentes com um erro de programação. Escreva o
programa **com o erro**, do jeito mais natural em Python (com LangGraph
quando houver um fluxo de passos), e deixe que as ferramentas o encontrem,
se encontrarem. "O que observar" diz o que conta como dano.

Alguns bugs podem não ter equivalente natural em Python. Nesse caso, escreva
o programa mais parecido que conseguir, ou crie o arquivo só com o cabeçalho
e `# Não se aplica: <motivo>`.

## Efeitos externos (pagamento, e-mail)

| # | Programa | O erro | O que observar |
|---|---|---|---|
| 01 | Dois passos em paralelo calculam um valor e gravam no mesmo campo do estado | O último a terminar sobrescreve o outro | Um dos valores se perde |
| 02 | Escreve um e-mail (modelo) e manda | O processo cai no meio do envio; na retomada, o programa não decidiu se reenvia ou não | E-mail duplicado ou perdido na retomada |
| 03 | Um agente (laço modelo → ferramentas) resolve um pedido e tem a ferramenta de mandar e-mail | O agente pode chamar o envio a cada volta | E-mails duplicados |
| 04 | Faz um reembolso e manda o e-mail de confirmação | O e-mail pode sair antes do reembolso, ou sem ele se o reembolso falhar | Confirmação de um reembolso que não aconteceu |
| 05 | Antes de pagar, passa ao serviço de pagamento uma condição a conferir ("só se o pedido foi entregue") | O serviço não sabe conferir condições e a ignora sem erro | Paga mesmo com a condição falsa |
| 06 | Paga só se uma condição sobre o pedido for verdadeira | A condição usa um campo com nome errado (erro de digitação) | A condição nunca é verdadeira, ou quebra |
| 07 | Paga só se uma condição sobre o pedido for verdadeira | A condição compara texto com número | A condição nunca é verdadeira, ou quebra |
| 08 | Paga só se uma condição for verdadeira | A condição chama outra ferramenta (consulta externa) em vez de olhar o estado lido junto com o pagamento | Entre a consulta e o pagamento o estado muda |
| 09 | Chama um serviço que paga e devolve o id do pagamento | Se a chamada fica sem resposta (timeout), o programa segue como se tivesse dado certo e inventa um id | O resto do fluxo usa um id que não existe |
| 10 | Manda um e-mail; se a resposta não vem, confere se o e-mail saiu | A conferência usa uma função que também escreve (por exemplo, "conferir e reenviar") | A conferência manda outro e-mail |
| 11 | Manda um e-mail; se a resposta não vem, confere se ele saiu | A conferência olha algo que não identifica este envio (por exemplo, "algum e-mail para o cliente hoje") | Conclui que saiu quando não saiu, ou o contrário |
| 12 | Uma operação que escreve recebe tratamento de leitura (por exemplo, nova tentativa automática como se fosse segura) | A escrita é repetida em falhas | Efeito duplicado |
| 13 | Paga e, se der timeout, tenta de novo | O pagamento não leva chave de idempotência | Paga duas vezes |
| 14 | Define que o e-mail sai depois do pagamento | A dependência aponta para o passo com nome errado | A ordem pretendida não existe |
| 15 | Dois passos que escrevem, com ordem entre eles | Cada um espera o outro | A execução nunca termina |
| 16 | Um fluxo que deveria só ler (para rodar sem aprovação) | Ele chama um pagamento | Paga sem aprovação |
| 17 | Faz um reembolso que pode ser recusado (`None`) | Usa o resultado como se tivesse dado certo | Segue (e avisa o cliente) com um reembolso que não houve |
| 18 | Manda um e-mail | O processo cai depois de mandar e antes de receber a resposta; a retomada reenvia | E-mail duplicado |
| 19 | Decide um reembolso (modelo) e paga | O pedido é cancelado entre a decisão e o pagamento | Paga um pedido cancelado |
| 20 | Confere o status do pedido numa leitura e paga num passo seguinte | Entre os dois o pedido pode mudar | Paga com base num status velho |
| 21 | Paga com chave de idempotência | A chave é o pedido, não a solicitação de reembolso | Um segundo reembolso legítimo do mesmo pedido é descartado em silêncio |

## Arquivos e repositório

| # | Programa | O erro | O que observar |
|---|---|---|---|
| 22 | Processa uma lista de itens em paralelo, cada um gravando um resultado | Todos gravam o mesmo arquivo | O resultado depende da ordem |
| 23 | Corrige um repositório com vários itens em paralelo (um por arquivo) | Todos editam o mesmo repositório ao mesmo tempo | Edições se sobrescrevem |
| 24 | Um passo edita o repositório enquanto outro só lê | O passo que edita foi tratado como leitura, e os dois rodam juntos | A leitura vê uma edição pela metade |
| 25 | Edita o repositório e roda os testes | Os testes rodam ao mesmo tempo que a edição | O resultado dos testes depende de quem chega primeiro |
| 26 | Um passo cria um repositório de trabalho | Ele é guardado no estado e usado depois por outros passos sem controle | Ninguém sabe quem é o dono; dois usos se misturam |
| 27 | Uma função que edita o repositório | É tratada como leitura; uma falha no meio não desfaz nada | A edição fica pela metade |
| 28 | Um agente tem a ferramenta de editar arquivos | O caminho (diretório) é um argumento que o modelo escolhe | O modelo pode escrever em qualquer lugar |
| 29 | Trabalha numa cópia do repositório | Uma função escreve no diretório original, não na cópia | A cópia não protege nada |
| 30 | Edita um arquivo e tenta de novo se falhar | A falha acontece depois de escrever parte do arquivo | A nova tentativa parte de um arquivo pela metade |
| 31 | Faz várias edições em sequência, com retomada | O processo cai no meio de uma edição | A retomada encontra o repositório num estado que o checkpoint não conhece |
| 32 | Roda os testes, tratado como leitura | Os testes escrevem arquivos (caches) no repositório | Uma "leitura" mudou o repositório |

## Estado compartilhado entre execuções (contas)

| # | Programa | O erro | O que observar |
|---|---|---|---|
| 33 | Deposita numa conta | Lê o saldo, soma e grava; duas execuções ao mesmo tempo | Um depósito se perde |
| 34 | A função que atualiza a conta | Chama um modelo enquanto atualiza | A conta fica travada esperando, e o resultado muda a cada vez |
| 35 | Pede uma resposta a uma operação da conta | A operação muda o estado e não devolve nada | O programa usa uma resposta que não existe |
| 36 | Manda um depósito para a conta | O nome da operação está errado (erro de digitação) | O depósito some |
| 37 | Uma operação da conta muda um campo | O campo não existe | A mudança se perde |
| 38 | Deposita e depois consulta o saldo na mesma execução | Sem ordem entre os dois | A consulta pode ver o saldo antigo |
| 39 | Várias execuções do mesmo usuário depositam ao mesmo tempo | — | Depósitos perdidos |
| 40 | Deposita, com retomada | O processo cai depois de o depósito ser aplicado e antes de a execução registrar isso; a retomada deposita de novo | Depósito duplicado |
| 41 | Deposita | O usuário clicou duas vezes: duas execuções com o mesmo depósito | Depósito duplicado |

## Esperas por pessoas

| # | Programa | O erro | O que observar |
|---|---|---|---|
| 42 | Espera uma aprovação | Sem prazo | Se ninguém responde, fica parada para sempre |
| 43 | Espera uma resposta de fora | Espera um valor de um tipo que nenhuma interface externa sabe entregar | Ninguém consegue responder |
| 44 | Espera uma aprovação | A mesma aprovação é entregue duas vezes (dois cliques) | A segunda é aplicada de novo |
| 45 | Espera uma aprovação com prazo de 3 dias | A máquina reinicia durante a espera e o prazo é contado de novo | O prazo nunca vence |
| 54 | Espera uma aprovação com prazo, e o processo está parado quando o prazo vence | A resposta chega depois do prazo, antes de alguém retomar | A resposta atrasada é aceita e o reembolso sai |

## Corridas, rodadas e laços

| # | Programa | O erro | O que observar |
|---|---|---|---|
| 46 | Duas estratégias de cobrança correm; fica a primeira que termina | A perdedora também cobra antes de a corrida ser decidida | O cliente é cobrado duas vezes |
| 47 | Duas estratégias de correção correm | As duas editam o mesmo repositório ao mesmo tempo | As edições se misturam |
| 48 | Uma corrida entre duas estratégias, com retomada | O processo cai depois de a corrida ser decidida; na retomada, outra termina primeiro | A execução segue com um vencedor diferente do que já usou |
| 49 | Um debate em rodadas entre agentes | Um agente lê as respostas da rodada atual enquanto os outros ainda respondem | Cada execução vê um conjunto diferente |
| 50 | Duas estratégias correm; a rápida vence | A lenta continua chamando o modelo | A conta sobe sem uso |
| 51 | Uma corrida com uma condição para aceitar o resultado | Nenhum ramo passa, e o programa não diz o que fazer | Segue com um valor que não existe |
| 52 | Uma corrida com uma condição para aceitar o resultado | A condição chama um modelo para julgar cada resposta | Cada ramo gera mais uma chamada paga, e o resultado muda a cada execução |
| 53 | Repete uma tarefa até uma conferência passar | O pagamento está dentro do laço e sai de novo a cada volta | Paga várias vezes |
