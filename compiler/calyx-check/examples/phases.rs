//! Times each phase of `check` on a file: `cargo run --release --example phases -- file.clyx`
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: phases <file.clyx>");
    let text = std::fs::read_to_string(&path).unwrap();
    let t = Instant::now();
    let (tokens, _) = calyx_syntax::lex(&text);
    println!(
        "lex   {:>8.2} ms ({} tokens)",
        t.elapsed().as_secs_f64() * 1e3,
        tokens.len()
    );
    let t = Instant::now();
    let (program, _) = calyx_syntax::parse(&text);
    println!(
        "parse {:>8.2} ms ({} decls, includes lex)",
        t.elapsed().as_secs_f64() * 1e3,
        program.decls.len()
    );
    let t = Instant::now();
    let report = calyx_check::check(&path, &text);
    println!(
        "check {:>8.2} ms total ({} diagnostics)",
        t.elapsed().as_secs_f64() * 1e3,
        report.diagnostics.len()
    );
}
