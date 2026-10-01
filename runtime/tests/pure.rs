//! The pure layer (milestone M7): `def`, `true`/`false`, `[... for ...]`,
//! `in` and the built-in functions, run by the interpreter.

use calyx_runtime::{Config, RunOptions, run};
use serde_json::{Value, json};

fn eval(src: &str, args: Value) -> Value {
    let report = calyx_check::check("t.clyx", src);
    assert!(!report.has_errors(), "{}", report.render());
    let opts = RunOptions {
        config: Config::builtin(),
        ..RunOptions::default()
    };
    run(&report.ir.to_json(), "g", &args, opts).unwrap()
}

const DEFS: &str = r#"
type Contract:
    parties: List[Text]
    value: Float

def check(c: Contract) -> List[Text]:
    problems = []
    if len(c.parties) < 2:
        problems = problems + ["menos de duas partes"]
    if c.value <= 0:
        problems = problems + ["valor inválido"]
    elif c.value > 1000:
        note = "valor alto"
        problems = problems + [note]
    return problems

def grade(score: Nat) -> Text:
    label = "baixa"
    if score >= 80:
        label = "alta"
    elif score >= 50:
        label = "média"
    return label
"#;

#[test]
fn defs_give_names_new_values_and_branch() {
    let src = format!(
        "{DEFS}\ngraph g(parties: List[Text], value: Float) -> List[Text]:\n    return check(Contract(parties=parties, value=value))\n"
    );
    assert_eq!(
        eval(&src, json!({"parties": ["a"], "value": 5000})),
        json!(["menos de duas partes", "valor alto"])
    );
    assert_eq!(
        eval(&src, json!({"parties": ["a", "b"], "value": 0})),
        json!(["valor inválido"])
    );
    assert_eq!(
        eval(&src, json!({"parties": ["a", "b"], "value": 10})),
        json!([])
    );
}

#[test]
fn elif_chains_pick_the_first_that_holds() {
    let src = format!("{DEFS}\ngraph g(s: Nat) -> Text:\n    return grade(s)\n");
    assert_eq!(eval(&src, json!({"s": 90})), json!("alta"));
    assert_eq!(eval(&src, json!({"s": 60})), json!("média"));
    assert_eq!(eval(&src, json!({"s": 10})), json!("baixa"));
}

#[test]
fn comprehensions_membership_and_builtins() {
    let src = r#"
graph g(words: List[Text]) -> Text:
    short = [upper(w) for w in words if len(w) <= 4]
    lens = [len(w) for w in words]
    has = "rio" in words
    part = "ont" in "montanha"
    yes = true and not false
    first = join(take(words, 2), "+")
    trimmed = trim("  oi  ")
    total = sum(lens)
    low = lower("AÇÃO")
    return "{short}|{lens}|{has}|{part}|{yes}|{first}|{trimmed}|{total}|{low}"
"#;
    let out = eval(src, json!({"words": ["sol", "montanha", "rio", "ação"]}));
    let text = out.as_str().unwrap();
    assert!(text.starts_with("- SOL\n- RIO\n- AÇÃO|"), "{text}");
    assert!(
        text.contains("|true|true|true|sol+montanha|oi|18|"),
        "{text}"
    );
}

#[test]
fn entity_handlers_use_defs_and_comprehensions() {
    let src = r#"
type Fact:
    topic: Text
    content: Text

def matches(f: Fact, topic: Text) -> Bool:
    return lower(topic) in lower(f.topic)

entity Notes(key id: Text):
    state facts: List[Fact] = []

    on About(topic: Text) -> List[Text]:
        return [f.content for f in facts if matches(f, topic)]

    on Add(f: Fact):
        next facts = facts + [f]

graph g(id: Text) -> List[Text]:
    send Notes(id).Add(Fact(topic="Gatos", content="Pipoca"))
    send Notes(id).Add(Fact(topic="Cidade", content="Recife"))
    return ask Notes(id).About("gato")
"#;
    let dir = std::env::temp_dir().join(format!("calyx-pure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let here = std::env::current_dir().unwrap();
    std::env::set_current_dir(&dir).unwrap();
    let out = eval(src, json!({"id": "x"}));
    std::env::set_current_dir(here).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out, json!(["Pipoca"]));
}
