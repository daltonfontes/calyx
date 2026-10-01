//! The `calyx` command-line tool.
//!
//! Exit codes: 0 = ok, 1 = the program has errors, 2 = usage or I/O error.

use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
usage: calyx <command> [options]

commands:
  check <file.clyx> [--format human|json] [--time] [--ir]
      Verify a program without generating code.
      --ir prints the compiled graph template when there are no errors.
  version
      Print the version.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => check(&args[1..]),
        Some("version" | "--version" | "-V") => {
            println!("calyx {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help" | "--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn check(args: &[String]) -> ExitCode {
    let mut file = None;
    let mut json = false;
    let mut time = false;
    let mut ir = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some("json") => json = true,
                Some("human") => json = false,
                _ => return usage_error("--format expects `human` or `json`"),
            },
            "--time" => time = true,
            "--ir" => ir = true,
            a if a.starts_with('-') => return usage_error(&format!("unknown option `{a}`")),
            a if file.is_none() => file = Some(a.to_owned()),
            _ => return usage_error("check takes a single file"),
        }
    }
    let Some(file) = file else {
        return usage_error("check needs a file");
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("calyx: cannot read `{file}`: {e}");
            return ExitCode::from(2);
        }
    };

    let started = Instant::now();
    let report = calyx_check::check(&file, &text);
    let elapsed = started.elapsed();

    if json {
        println!("{}", report.to_json());
    } else if !report.diagnostics.is_empty() {
        print!("{}", report.render());
    }
    if ir && !report.has_errors() {
        print!("{}", report.ir);
    }
    if time {
        eprintln!("checked in {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    }
    if report.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("calyx: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}
