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

#[test]
fn the_remainder_has_the_sign_of_the_divisor() {
    let src = "graph g(a: Int, b: Int) -> Int:\n    return a % b\n";
    // As in Python: 7 % 3 == 1, -7 % 3 == 2, 7 % -3 == -2, -7 % -3 == -1.
    for (a, b, want) in [(7, 3, 1), (-7, 3, 2), (7, -3, -2), (-7, -3, -1), (6, 3, 0)] {
        assert_eq!(eval(src, json!({"a": a, "b": b})), json!(want), "{a} % {b}");
    }
    let report = calyx_check::check("t.clyx", src);
    let opts = RunOptions {
        config: Config::builtin(),
        ..RunOptions::default()
    };
    let err = run(&report.ir.to_json(), "g", &json!({"a": 1, "b": 0}), opts).unwrap_err();
    assert!(format!("{err:?}").contains("division by zero"), "{err:?}");
}

#[test]
fn maps_keep_the_order_keys_were_added_and_the_right_one_wins() {
    let src = r#"
type Seen:
    names: List[Text]
    values: List[Nat]
    merged: Map[Text, Nat]
    ana: Nat
    missing: Nat
    has_bia: Bool
    size: Nat

graph g(m: Map[Text, Nat]) -> Seen:
    merged = m + {"bia": 5, "ana": 9}
    return Seen(names = keys(merged), values = values(merged), merged = merged, ana = merged["ana"], missing = get(merged, "zé", 0), has_bia = "bia" in merged, size = len({}))
"#;
    assert_eq!(
        eval(src, json!({"m": {"ana": 3, "caio": 1}})),
        json!({
            "names": ["ana", "caio", "bia"],
            "values": [9, 1, 5],
            "merged": {"ana": 9, "caio": 1, "bia": 5},
            "ana": 9,
            "missing": 0,
            "has_bia": true,
            "size": 0,
        })
    );
    // A key given twice in one literal: its first place, its last value.
    let src = "graph g(x: Nat) -> List[Nat]:\n    m = {\"a\": 1, \"b\": 2, \"a\": x}\n    return values(m)\n";
    assert_eq!(eval(src, json!({"x": 7})), json!([7, 2]));
}

#[test]
fn a_missing_key_is_a_failure_that_try_catches() {
    let src = "graph g(m: Map[Text, Nat]) -> Nat:\n    return m[\"zé\"]\n";
    let report = calyx_check::check("t.clyx", src);
    let opts = RunOptions {
        config: Config::builtin(),
        ..RunOptions::default()
    };
    let err = run(&report.ir.to_json(), "g", &json!({"m": {"a": 1}}), opts).unwrap_err();
    assert!(
        format!("{err:?}").contains("the map has no key `zé`"),
        "{err:?}"
    );
}

#[test]
fn a_list_is_read_by_position_from_zero() {
    let src = "graph g(xs: List[Text], n: Int) -> Text:\n    return xs[n]\n";
    assert_eq!(
        eval(src, json!({"xs": ["a", "b", "c"], "n": 0})),
        json!("a")
    );
    assert_eq!(
        eval(src, json!({"xs": ["a", "b", "c"], "n": 2})),
        json!("c")
    );
    let report = calyx_check::check("t.clyx", src);
    for (n, said) in [(3, "no item 3 (it has 3)"), (-1, "no item -1")] {
        let opts = RunOptions {
            config: Config::builtin(),
            ..RunOptions::default()
        };
        let err = run(
            &report.ir.to_json(),
            "g",
            &json!({"xs": ["a", "b", "c"], "n": n}),
            opts,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains(said), "{err:?}");
    }
    // `try` catches it, as it does a missing key.
    let src = "graph g(xs: List[Nat]) -> Nat:\n    x = try:\n        xs[5]\n    return match x:\n        case Ok(v): v\n        case Failed(e): 0\n";
    assert_eq!(eval(src, json!({"xs": [1]})), json!(0));
}
