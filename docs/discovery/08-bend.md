# Bend: BendRT e BendTT

Dois papers da Higher Order Company (Rio de Janeiro) sobre o **Bend**, uma linguagem funcional pura e paralela:

| Paper | Assunto | Base da leitura |
|---|---|---|
| *BendRT: A Parallel Runtime for CPUs and GPUs* (6 p.) | O runtime: compilação para C, memória sem coletor de lixo, escalonamento, GPU | PDF completo |
| *BendTT: An Affine Dependent Type Theory* (9 p.) | O sistema de tipos: tipos dependentes afins, consistência, terminação | PDF completo |

Fonte: [bendlang/bend, pasta `paper/`](https://github.com/bendlang/bend/tree/main/paper). Os dois papers declaram que a linguagem foi projetada pelo autor humano e que o texto foi escrito por um modelo Claude a partir do código e das notas do autor, com revisão do autor; as provas do BendTT são mecanizadas em Lean 4.

**Aviso de escopo:** o Bend resolve um problema diferente do da Calyx. Ele paraleliza **computação** (milhões de tarefas de microssegundos, em CPU e GPU). A Calyx orquestra **espera** (dezenas a milhares de nós que levam segundos ou minutos, aguardando LLMs e tools). Por isso, parte das ideias não se transfere, e saber **qual parte** é útil para a D10.

---

## BendRT: o runtime

1. **Problema:** rodar o mesmo programa funcional em uma thread, em várias threads de CPU e na GPU, sem coletor de lixo, sem *kernels* escritos à mão e com desempenho próximo de C.
2. **Agente:** não se aplica.
3. **Workflow:** um programa Bend. O compilador gera **um único arquivo C** que contém o runtime, o programa e os efeitos importados; o executor (uma thread, várias, ou GPU) é escolhido na hora de rodar. Também há um backend JavaScript.
4. **Grafo:** não há grafo explícito; há uma árvore de tarefas criada em tempo de execução pelas bifurcações do programa.
5. **Execução:**
   - Cada função vira uma **árvore de casos** dentro de uma única função de laço, **sem pilha de chamadas de C**. Cada ponto de chamada tem duas leituras, sequencial e paralela, e o mesmo código serve a todos os executores.
   - Três formas de chamada: *tail* (vira um salto), *cut* (uma chamada com resultado nomeado; a continuação vira uma definição) e *fork* (várias chamadas em paralelo; a continuação vira um nó de junção).
6. **Estado / memória:**
   - **Afinidade:** todo valor tem **um dono**. Quem consome um valor (um `match`) libera a memória dele; por isso não há coletor de lixo.
   - Compartilhamento existe só onde o compilador pôs um **contador de referências** ou um **empréstimo** (*borrow*), descobertos por análise do programa inteiro.
   - **Uma tarefa bifurcada é dona dos seus argumentos**: uma bifurcação **move** memória, nunca compartilha. O único protocolo entre threads é **entregar uma resposta a uma junção** (uma escrita num slot próprio e um decremento atômico do contador de filhos pendentes).
7. **Concorrência:**
   - O paralelismo é **marcado pelo programador** com o *parallel let* (`l r = f(x) g(y)`: "rode essas chamadas em paralelo; elas dividem o trabalho em partes iguais"). `f!(x)` manda a chamada para a GPU. Nada mais cria paralelismo.
   - **Sem roubo de trabalho** (*work stealing*), sem fila compartilhada, sem travas. As tarefas vivem num "cubo" (grade fixa 128 × 128 de filas circulares) e o runtime alterna **ondas**: uma fase de *crescer* (espalha as bifurcações) e uma de *trabalhar* (cada faixa drena a sua coluna em código sequencial). É o modelo *bulk-synchronous* (BSP).
   - **O preço é um contrato:** o programa precisa dividir o trabalho em partes iguais. Se não dividir, o paralelismo se perde em silêncio.
   - **Efeitos:** o núcleo é puro. `IO` é uma continuação sobre um tipo de dados com pedidos (`Emit`, `Halt`). Um laço de eventos, numa thread, avalia o programa até um pedido, executa o efeito em C, aplica a continuação à resposta e continua. **Efeitos nunca rodam dentro do avaliador.**
8. **Sem solução** (declarado pelos autores): o contrato de partes iguais não é verificado; CPU e GPU nunca computam ao mesmo tempo; vários limites fixos geram falha em vez de alternativa; o runtime em C não é verificado formalmente.

**Resultados** (Apple M4 Max): versão sequencial entre 0,8× e 1,5× o tempo de C escrito à mão; 16 threads dão de 8,8× a 12,1× sobre uma; a GPU dá de 52× a 67× em trabalho uniforme, mas **perde** para 16 threads em trabalho irregular (n-rainhas, regressão simbólica).

## BendTT: o sistema de tipos

1. **Problema:** assistentes de prova (Coq, Agda, Lean) proíbem `Type : Type` e tipos que aparecem à esquerda de uma seta nos próprios construtores, porque cada um permite provar o falso. O BendTT permite os dois e continua consistente.
2. **Agente:** não se aplica.
3. **Workflow:** programas e provas escritos juntos; uma prova é um programa comum (indução é recursão).
4. **Grafo:** não se aplica.
5. **Execução:** cada termo tem duas partes:
   - **código vivo**, que roda;
   - **código morto**, que só é verificado (tipos, anotações, argumentos apagados antes de rodar).

   O código vivo segue três regras: (1) uma variável é usada **no máximo uma vez** (afinidade); (2) pode ser usada várias vezes só se o tipo dela for do *kind* `Data` (rótulos, pares e provas de igualdade, **sem funções**); (3) uma definição só chama a si mesma com **argumentos menores** (recursão estrutural). O código morto não segue nenhuma.
6. **Estado:** cada variável tem uma **quantidade**: 0 (apagada), 1 (afim) ou 2 (copiável). Uma *checagem viva* separada conta os usos sem ler tipos.
7. **Concorrência:** não é o foco; a afinidade é o que permite ao BendRT mover memória entre tarefas sem compartilhar.
8. **Sem solução / custo da afinidade** (declarado pelos autores):
   - um `map` comum é rejeitado (a função é usada duas vezes); há três contornos (chamar uma definição pelo nome, parâmetros de template, ou escrever o laço como definição própria);
   - não há recursão mútua;
   - **um laço cujos argumentos não diminuem, como o laço principal de um servidor, precisa de um contador**;
   - a conversão de tipos é indecidível com `Type : Type`; o verificador usa um orçamento de passos e pode rejeitar um programa correto (nunca aceitar um errado).

As provas (confluência, preservação de tipos, progresso, terminação do código vivo e consistência) estão num único arquivo Lean 4 de 4.247 linhas.

---

## O que se transfere para a Calyx

### 1. Afinidade para **recursos**, não para valores (decisão nova D26)

É a ideia mais útil dos dois papers. No Bend, um valor afim tem um dono, uma bifurcação **move** os argumentos para a tarefa filha, e copiar exige ação explícita.

Na Calyx, valores comuns (texto, JSON, `conversation`) são imutáveis e podem ser copiados livremente para vários ramos; seriam o *kind* `Data` do Bend. Mas existe uma categoria de coisas que **não deveria** ir para dois ramos ao mesmo tempo:

| Recurso | Problema se dois ramos o recebem |
|---|---|
| Sandbox (D13) | Dois agentes editando o mesmo repositório (o exemplo do paper de controle de concorrência) |
| Orçamento | O *write skew* da D25: dois ramos veem 100 e cada um gasta 70 |
| Capacidade de escrita `write once` | Duas mensagens enviadas para o mesmo destino |

**Proposta:** esses recursos são **afins**. Um recurso tem um dono por vez; passar para um ramo **move** o recurso. Para dois ramos usarem, o programador precisa **dividi-lo explicitamente** (orçamento de 100 → 60 + 40; sandbox → duas cópias), e o compilador verifica.

Consequências:
- **O exemplo de *write skew* do orçamento passa a ser impossível de escrever**: não se resolve na junção, é rejeitado na compilação. A D25 (validação na junção) continua necessária para invariantes de *valores*, mas recursos saem do problema.
- **Concorrência sobre a sandbox** vira uma questão de tipo: ou cada ramo recebe uma cópia (merge na junção), ou a sandbox fica com um ramo só. As duas estratégias da D13 continuam possíveis, mas a escolha passa a ser **explícita e verificada**.
- Isso responde, em tempo de compilação, parte do que o paper de controle de concorrência pede em tempo de execução: *"acesso estruturado a recursos compartilhados"*.

### 2. Efeitos fora do núcleo: segunda fonte independente

O Bend executa efeitos assim: o núcleo puro avalia até produzir um **pedido de efeito**; um laço de eventos executa o efeito e devolve a resposta para a continuação. É a mesma arquitetura dos *Commands* do Temporal, vinda de outra direção (linguagens funcionais, não sistemas distribuídos).

Para a Calyx, isso fixa a arquitetura do runtime: **o avaliador do grafo é puro e só emite pedidos** (`llm`, `read`, `write`); um executor de efeitos os realiza, grava no diário e devolve o resultado. Replay, retentativa e recuperação acontecem inteiramente no executor de efeitos.

### 3. Terminação: o Bend paga o preço que a Calyx já decidiu pagar

O BendTT exige recursão com argumentos que diminuem, e reconhece que *"o laço principal de um servidor precisa de um contador"*. É exatamente a regra da Calyx para ciclos (D5: limite obrigatório) e recursão (D17: parâmetro que decresce). O Bend mostra que dá para viver com essa restrição numa linguagem real, e documenta onde ela incomoda.

### 4. Código vivo × código morto: uma forma de resolver expressividade × verificabilidade

O BendTT deixa a parte que só é verificada (tipos, provas) **muito expressiva**, e restringe só a parte que **roda**. É uma resposta concreta à tensão que o survey de ACG apontou.

Para a Calyx: o que é só verificado (orçamentos de contexto e de custo, invariantes, limites, tipos das variantes) pode ser rico, porque nunca executa; o que roda (o grafo) é que precisa das restrições de efeito, afinidade e limite.

### 5. Granularidade: por que derivar paralelismo funciona na Calyx e o Bend preferiu marcar

O Bend exige que o programador **marque** onde há paralelismo. Para computação fina (tarefas de microssegundos), paralelizar automaticamente criaria tarefas demais e pequenas demais. Na Calyx, cada nó leva **segundos** (uma chamada de LLM), então o custo de criar uma tarefa é desprezível frente ao trabalho. **Derivar todo o paralelismo do grafo (D3) é viável justamente porque os nós são grossos.** É um argumento a favor da hipótese que vale registrar.

### 6. Reprodutibilidade entre executores

O BendRT exige que todos os executores (1 thread, 16 threads, GPU) **imprimam os mesmos bytes**. Na Calyx, as junções com ordem fixa (D7: resultados na ordem da entrada, não na de término) dão a mesma propriedade: o resultado não depende de quantas threads rodaram nem de quem terminou primeiro.

## O que **não** se transfere

| Ideia do Bend | Por que não serve para a Calyx |
|---|---|
| Ondas BSP sem roubo de trabalho | Exigem partes iguais. Nós de LLM têm durações muito desiguais (um leva 2 s, outro 90 s); uma onda esperaria sempre o mais lento. A Calyx precisa de escalonamento dinâmico por lista (D24) |
| Memória sem coletor, cubo de tarefas, GPU | Otimizam computação. Na Calyx o tempo está na espera de rede; o runtime fica parado a maior parte do tempo |
| Paralelismo marcado no código | Desnecessário com nós grossos (item 5) |

## Impacto na D10 (C# ou C)

O BendRT mostra que **C é ótimo quando o gargalo é computação**: sem coletor, sem pilha, mesmo código na GPU. Mas o gargalo da Calyx é **esperar LLMs e tools**. O que pesa para ela é: E/S assíncrona, durabilidade do diário, facilidade para HTTP/JSON e clientes de LLM, e o custo de escrever o compilador e o verificador.

**Isso enfraquece o argumento de "C por desempenho" para o runtime da Calyx.** Continua valendo o argumento de **embutir** (C é mais fácil de embutir em outras linguagens), e o exemplo do Bend de **gerar um único arquivo C** como alvo de compilação.

---

## Decisões novas ou revisadas

| # | Decisão | Proposta |
|---|---|---|
| D26 | Recursos afins | Sandbox, orçamento e capacidades `write once` têm um dono por vez; passar para um ramo move o recurso; dividir entre ramos é explícito e verificado pelo compilador |
| D25 (revisada) | Invariantes entre ramos | Para **recursos**, resolvido por afinidade (D26) em tempo de compilação; para **valores**, continua a validação declarada na junção |
| D10 (critério) | Plataforma | O runtime é limitado por E/S, não por computação; desempenho bruto pesa menos que E/S assíncrona, durabilidade e custo de escrever o compilador |
| Arquitetura | Efeitos | Avaliador do grafo puro, emitindo pedidos de efeito; executor de efeitos separado, dono do diário (confirmado por Temporal e Bend) |
