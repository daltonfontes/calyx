"""The classification of the real-bug study, as data, and its counts.

Every candidate in candidates.json is either excluded (with the reason) or
classified (category C1-C7 and class). Notes say what the issue reports and
why the class; `verify` names the check in run_verify.py, where there is one.

    python bench/real_bugs/classify.py      # writes classification.json, prints the counts
"""
import collections
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
LG, CR, AG = "langchain-ai/langgraph", "crewAIInc/crewAI", "microsoft/autogen"

EXCLUDED = {
    LG: {
        8515: "desempenho (max_concurrency ignorado)", 8516: "desempenho (duplicata de 8515)",
        8517: "desempenho (duplicata de 8515)", 8226: "documentação", 8644: "pedido de recurso",
        4833: "interface da API", 5109: "erro do provedor de modelo", 7591: "desempenho",
        7127: "duplicata de 7128", 6907: "pedido de recurso (merge semântico entre ramos)",
        6818: "pedido de garantia e de testes, sem bug relatado",
        2734: "contexto de execução da tool (event loop da thread), fora das categorias",
    },
    CR: {
        7460: "pedido de recurso", 2239: "pedido de recurso", 4682: "pedido de recurso",
        7783: "erro do provedor de modelo", 7349: "erro do provedor de modelo",
        2383: "comportamento do modelo", 2288: "erro de parâmetro", 173: "timeout do provedor",
        6267: "TypeError do framework, fora das categorias", 6529: "interface de hooks",
        3154: "comportamento do modelo", 7775: "observabilidade (flag de cache no evento)",
        7657: "proposta de fornecedor, sem bug",
    },
    AG: {
        3354: "uso", 3640: "pedido de recurso", 4225: "provedor (ollama)",
        7115: "provedor (anthropic)", 6823: "comportamento do modelo", 5044: "provedor (mistral)",
        6843: "provedor (gemini)", 4894: "pedido de recurso (aprovação antes da tool)",
        5510: "pedido de recurso (encadear tools em ordem)", 5340: "pedido de recurso",
    },
}

F, R, B, N = "bug do framework", "runtime", "construção", "não pega"
# number: (category, class, note, verify)
CLASSIFIED = {
    LG: {
        9164: ("C4", F, "retomada logo após o interrupt perde resultados: o servidor anuncia o interrupt antes de gravá-lo", None),
        9002: ("C1", F, "error_handler com efeito roda de novo na retomada após falhas em paralelo", None),
        8817: ("C6", N, "prompt injection faz o ToolNode executar uma tool perigosa sem confirmação; um agente da Calyx pode usar tools `write` sem confirmação", None),
        7780: ("C4", F, "interrupt() dentro de um while reentrega respostas antigas", None),
        7412: ("C2", F, "asyncio.gather descarta os resultados das outras tools quando uma falha", None),
        8551: ("C7", F, "update_state num checkpoint antigo grava no ramo errado (DeltaChannel)", None),
        9106: ("C1", F, "retomar um de dois interrupts paralelos refaz o efeito do nó irmão", None),
        8714: ("C2", F, "aupdate_state diverge de update_state", None),
        8382: ("C2", F, "replay do DeltaChannel muda a ordem das escritas paralelas", None),
        6446: ("C2", F, "InvalidUpdateError falso em subgrafos paralelos com chaves diferentes", None),
        8234: ("C7", F, "durability=sync: put_writes e checkpoint não são atômicos", None),
        6208: ("C4", R, "nó com dois interrupts roda de novo inteiro depois da primeira resposta; na Calyx cada `receive` tem sua chave no diário", "two_waits"),
        7128: ("C7", F, "estado do subgrafo inacessível com checkpointer por thread", None),
        9076: ("C4", F, "update_state apaga os interrupts pendentes; o id guardado pela interface morre", None),
        9001: ("C4", F, "retomada após remover um campo do estado fica presa no interrupt_before", None),
        8836: ("C4", F, "resposta com chave em hex de 32 caracteres vira no-op silencioso", None),
        8837: ("C4", F, "invoke(None) reentrega a resposta já consumida ao segundo interrupt", None),
        5157: ("C5", F, "ids de tool call repetidos pelo modelo encerram o agente antes da hora", None),
        8204: ("C5", F, "remaining_steps com return_direct aborta com um passo suficiente", None),
        6064: ("C4", N, "a próxima mensagem do usuário volta ao mediador em vez do subagente que esperava; a Calyx não tem passagem de controle entre agentes numa conversa", None),
        6731: ("C5", N, "agente alterna tools diferentes até o limite de recursão; o `on stuck` da Calyx só pega a mesma chamada repetida", None),
        5099: ("C5", R, "agente repete a mesma chamada com argumentos errados; na Calyx o `on stuck` (obrigatório) para na terceira volta igual", "stuck_agent"),
        8582: ("C7", R, "a tarefa retomada perde o UntrackedValue da entrada; na Calyx o recurso de execução (sandbox) volta pelo snapshot na retomada (tests/sandbox.rs)", None),
        5672: ("C7", N, "cancelar no meio do streaming perde o que já foi mostrado; a Calyx também perde a resposta parcial de uma chamada em andamento", None),
        6491: ("C7", F, "estado inválido gravado no checkpoint sem validação", None),
        6623: ("C7", F, "checkpointer Postgres grava thread_id em dois formatos", None),
        4987: ("C7", F, "fork por time travel reaproveita os ids de checkpoint", None),
        2610: ("C1", F, "tool chamada duas vezes com dois agentes em paralelo; causa não explicada na issue (na dúvida, a classe neutra)", None),
    },
    CR: {
        5802: ("C1", N, "nova tentativa da tarefa repete o pagamento; na Calyx uma tool `write` com chave paga uma vez, mas uma `write once` num `loop` paga a cada volta, sem aviso", "retry_pays_once, write_once_in_loop"),
        2881: ("C1", F, "regressão: tools chamadas várias vezes com resposta válida", None),
        1978: ("C1", F, "kickoff roda duas vezes e o e-mail sai duas vezes", None),
        3462: ("C1", F, "regressão: toda tool roda exatamente duas vezes", None),
        3489: ("C1", F, "structured_tool.invoke chama a função duas vezes", None),
        737: ("C5", R, "agente repete a mesma tool com a mesma entrada até o timeout; na Calyx o `on stuck` para na terceira volta igual", "stuck_agent"),
        6706: ("C7", F, "restaurar o checkpoint apaga campos novos do estado", None),
        6125: ("C3", R, "agentes concorrentes gravam o mesmo estado e perdem 249 de 250 atualizações; na Calyx o estado compartilhado é uma entidade, uma mudança por vez com flock (tests/entities.rs, concurrent_runs_lose_no_update)", None),
        960: ("C4", B, "o fluxo segue para o próximo agente sem a aprovação humana; na Calyx o passo seguinte usa o valor do `receive` e não começa antes", "human_gate"),
        3679: ("C1", F, "tools do agente copiadas para o gerente; a mesma tool roda duas vezes", None),
        2294: ("C5", N, "agente continua chamando a tool depois de ter a resposta; não se sabe se com os mesmos argumentos (na dúvida, não pega)", None),
        2209: ("C5", N, "agente reenvia a mesma pergunta pelo websocket duas vezes; o `on stuck` só para na terceira", None),
    },
    AG: {
        6882: ("C2", F, "duas chamadas da TeamTool: a segunda falha porque o time ainda está rodando", None),
        7956: ("C7", F, "cancelar com uma tool em andamento deixa o stream pendurado para sempre", None),
        6819: ("C4", B, "a conversa segue o procedimento sem esperar a resposta do cliente; na Calyx a espera é um `receive` e o passo seguinte depende dela", "human_gate"),
        833: ("C5", N, "o agente às vezes usa 2 das 3 tools pedidas; um agente da Calyx faria o mesmo", None),
    },
}


def main() -> None:
    cands = json.load(open(os.path.join(HERE, "candidates.json")))
    out, counts = [], collections.Counter()
    by_cat = collections.defaultdict(collections.Counter)
    for repo in (LG, CR, AG):
        seen = {n for q in cands[repo].values() for n, _ in q}
        titles = {n: t for q in cands[repo].values() for n, t in q}
        ex, cl = EXCLUDED[repo], CLASSIFIED[repo]
        assert seen == set(ex) | set(cl), (repo, seen ^ (set(ex) | set(cl)))
        assert not set(ex) & set(cl), repo
        for n in sorted(seen):
            row = {"repo": repo, "issue": n, "title": titles[n],
                   "url": f"https://github.com/{repo}/issues/{n}"}
            if n in ex:
                row["excluded"] = ex[n]
                counts[(repo, "excluída")] += 1
            else:
                cat, cls, note, verify = cl[n]
                row.update(category=cat, **{"class": cls}, note=note)
                if verify:
                    row["verify"] = verify
                counts[(repo, cls)] += 1
                by_cat[cat][cls] += 1
            out.append(row)
    with open(os.path.join(HERE, "classification.json"), "w") as f:
        json.dump(out, f, indent=1, ensure_ascii=False)
    classes = ["excluída", "compilador", R, B, N, F]
    print("repo".ljust(24) + "".join(c.rjust(18) for c in classes))
    for repo in (LG, CR, AG, None):
        cells = [sum(v for (r, c), v in counts.items() if c == cl and (repo is None or r == repo))
                 for cl in classes]
        print((repo or "total").ljust(24) + "".join(str(x).rjust(18) for x in cells))
    print()
    for cat in sorted(by_cat):
        print(cat, dict(by_cat[cat]))


if __name__ == "__main__":
    main()
