//! The Calyx verifier.
//!
//! There is exactly one verifier (decision D10): the CLI uses it as a Rust
//! library, and the C runtime links it through the C ABI in `calyx-runtime`,
//! to check graphs generated at run time before running them.
//!
//! Every analysis is linear or compositional, so `calyx check` stays under
//! one second.

mod lower;
mod sema;
mod types;

use calyx_syntax::{Diagnostic, Severity, Source, parse};

/// Result of checking one source file.
#[derive(Debug)]
pub struct Report {
    pub source: Source,
    /// Sorted by position in the file.
    pub diagnostics: Vec<Diagnostic>,
    /// The compiled template. Only meaningful when there are no errors.
    pub ir: calyx_ir::Program,
}

impl Report {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Human-readable output, one block per diagnostic.
    pub fn render(&self) -> String {
        self.diagnostics
            .iter()
            .map(|d| d.render(&self.source))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Machine-readable output for agents and the runtime.
    pub fn to_json(&self) -> String {
        Diagnostic::list_to_json(&self.diagnostics, &self.source)
    }
}

/// Checks a Calyx program.
pub fn check(name: &str, text: &str) -> Report {
    let source = Source::new(name, text);
    let (program, mut diagnostics) = parse(&source.text);
    let mut ir = sema::check_program(&program, &mut diagnostics);
    // "Value is never used" is noise while the program still has errors.
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        diagnostics.retain(|d| d.code != "W0801");
    } else {
        lower::lower(&program, &mut ir);
    }
    diagnostics.sort_by_key(|d| (d.span.start, d.span.end));
    Report {
        source,
        diagnostics,
        ir,
    }
}

/// Lexes a program without parsing it. Used to keep files that use
/// constructs from later milestones (such as the examples) lexically valid.
pub fn lex_only(name: &str, text: &str) -> Report {
    let source = Source::new(name, text);
    let (_, diagnostics) = calyx_syntax::lex(&source.text);
    Report {
        source,
        diagnostics,
        ir: calyx_ir::Program::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(src: &str) -> Vec<&'static str> {
        check("t.clyx", src)
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect()
    }

    const PRELUDE: &str = r#"
model claude = "m"

tool search(q: Text) -> Text:
    effect read

type Plan:
    questions: List[Text] max 5

prompt split(topic: Text) -> Plan:
    """{topic}"""

prompt summarize(items: List[Text]) -> Text:
    """{items}"""
"#;

    fn with_prelude(body: &str) -> String {
        format!("{PRELUDE}\n{body}")
    }

    #[test]
    fn valid_program_has_no_diagnostics() {
        let r = check(
            "ok.clyx",
            &with_prelude(
                "graph g(topic: Text) -> Text:\n    plan = claude(split(topic))\n    found = for each q in plan.questions: search(q)\n    out = claude(summarize(found))\n    return out\n",
            ),
        );
        assert!(r.diagnostics.is_empty(), "{}", r.render());
        let g = &r.ir.graphs[0];
        assert_eq!(g.effect, Some(calyx_ir::Effect::Read));
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.nodes[1].ty, "List[Text] max 5");
    }

    #[test]
    fn steps_may_be_written_in_any_order() {
        let r = check(
            "t.clyx",
            &with_prelude(
                "graph g(topic: Text) -> Text:\n    out = claude(summarize(plan.questions))\n    plan = claude(split(topic))\n    return out\n",
            ),
        );
        assert!(r.diagnostics.is_empty(), "{}", r.render());
        assert_eq!(r.ir.graphs[0].nodes[0].name, "plan");
    }

    #[test]
    fn pure_steps_are_inferred() {
        let r = check(
            "t.clyx",
            &with_prelude(
                "graph g(topic: Text) -> Text:\n    label = \"tema: {topic}\"\n    return label\n",
            ),
        );
        assert!(r.diagnostics.is_empty(), "{}", r.render());
        assert_eq!(r.ir.graphs[0].effect, Some(calyx_ir::Effect::Pure));
    }

    #[test]
    fn reports_semantic_errors() {
        assert_eq!(
            codes(&with_prelude(
                "graph g() -> Text:\n    a = search(1)\n    return a\n"
            )),
            vec!["E0608"]
        );
        assert_eq!(
            codes(&with_prelude(
                "graph g(t: Text) -> Text:\n    a = split(t)\n    return a\n"
            )),
            vec!["E0606"]
        );
        assert_eq!(
            codes(&with_prelude(
                "graph g() -> Nat:\n    a = search(\"x\")\n    return a\n"
            )),
            vec!["E0610"]
        );
        assert_eq!(
            codes(&with_prelude(
                "graph g() -> Text:\n    a = b\n    b = a\n    return a\n"
            )),
            vec!["E0506"]
        );
    }

    #[test]
    fn effect_limit_is_enforced() {
        assert_eq!(
            codes(&with_prelude(
                "graph g(t: Text) -> Text:\n    effect llm\n    a = search(t)\n    return a\n"
            )),
            vec!["E0701"]
        );
    }

    #[test]
    fn write_once_needs_a_policy() {
        assert_eq!(
            codes("tool send(to: Text) -> Unit:\n    effect write once\n"),
            vec!["E0304"]
        );
    }

    #[test]
    fn unused_llm_result_is_a_warning() {
        let r = check(
            "t.clyx",
            &with_prelude("graph g(t: Text) -> Text:\n    a = claude(split(t))\n    return t\n"),
        );
        assert_eq!(r.diagnostics.len(), 1, "{}", r.render());
        assert_eq!(r.diagnostics[0].code, "W0801");
        assert!(!r.has_errors());
    }

    #[test]
    fn the_same_write_once_on_every_turn_is_a_warning() {
        let program = |args: &str| {
            format!(
                "tool send(to: Text, text: Text) -> Unit:\n    effect write once\n    on_uncertain pause\n\n\
                 graph g(to: Text) -> Int:\n    r = loop n = 1, max 3:\n        \
                 sent = send({args})\n        next n + 1\n        on limit: last\n    return r\n"
            )
        };
        assert_eq!(codes(&program("to, \"oi\"")), vec!["W0605"]);
        // What changes from turn to turn, directly or inside a text, is a new write.
        assert!(codes(&program("to, \"tentativa {n}\"")).is_empty());
        assert!(codes(&program("\"{to}\", \"oi {n}\"")).is_empty());
    }

    #[test]
    fn sending_back_what_was_read_warns_only_when_it_overwrites() {
        let program = |update: &str| {
            format!(
                "entity Notes(key id: Text):\n    state notes: List[Text] = []\n\n    \
                 on All() -> List[Text]:\n        return notes\n\n    \
                 on Put(note: Text):\n        next notes = {update}\n\n\
                 graph g(id: Text) -> Text:\n    seen = ask Notes(id).All()\n    \
                 send Notes(id).Put(\"{{seen}}\")\n    return \"ok\"\n"
            )
        };
        // A new value computed from what was read: another run's note is lost.
        assert_eq!(codes(&program("[note]")), vec!["W0603"]);
        // A change to the current value: nothing is lost.
        assert!(codes(&program("notes + [note]")).is_empty());
    }
}
