//! The `calyx` command-line tool.
//!
//! Exit codes: 0 = ok, 1 = the program has errors, 2 = usage or I/O error,
//! 3 = the execution failed.

use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
usage: calyx <command> [options]

commands:
  check <file.clyx> [--format human|json] [--time] [--ir | --ir-json]
      Verify a program without generating code.
      --ir prints the compiled graph template when there are no errors;
      --ir-json prints it in the JSON form the runtime loads.
  run <file.clyx> [--graph NAME] [--fake-models] [--quiet] [--config FILE]
                 [--PARAM VALUE ...]
      Check and run a graph. Each parameter of the graph is passed as
      `--name value` (e.g. --topic \"energia solar\").
      --graph chooses the graph (default: the only one, or `main`).
      --fake-models answers every model call with fake values shaped by the
      prompt's type, with no network or API key.
      --quiet hides the trace (one line per node and effect, on stderr).
      --config uses this calyx.toml instead of looking for one next to the
      program and in its parent directories.
  version
      Print the version.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => check(&args[1..]),
        Some("run") => run(&args[1..]),
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
    let mut ir_json = false;
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
            "--ir-json" => ir_json = true,
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
    if ir_json && !report.has_errors() {
        println!("{}", report.ir.to_json());
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

fn run(args: &[String]) -> ExitCode {
    let mut file = None;
    let mut graph = None;
    let mut config_path = None;
    let mut fake_models = false;
    let mut quiet = false;
    let mut values: Vec<(String, String)> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--graph" => match it.next() {
                Some(g) => graph = Some(g.clone()),
                None => return usage_error("--graph expects a name"),
            },
            "--config" => match it.next() {
                Some(c) => config_path = Some(std::path::PathBuf::from(c)),
                None => return usage_error("--config expects a file"),
            },
            "--fake-models" => fake_models = true,
            "--quiet" => quiet = true,
            a if a.starts_with("--") => match it.next() {
                Some(v) => values.push((a[2..].to_owned(), v.clone())),
                None => return usage_error(&format!("`{a}` expects a value")),
            },
            a if file.is_none() => file = Some(a.to_owned()),
            _ => return usage_error("run takes a single file"),
        }
    }
    let Some(file) = file else {
        return usage_error("run needs a file");
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("calyx: cannot read `{file}`: {e}");
            return ExitCode::from(2);
        }
    };
    let report = calyx_check::check(&file, &text);
    if report.has_errors() {
        print!("{}", report.render());
        return ExitCode::from(1);
    }
    let program = &report.ir;

    let names: Vec<&str> = program.graphs.iter().map(|g| g.name.as_str()).collect();
    let graph = match graph {
        Some(g) => g,
        None if names.len() == 1 => names[0].to_owned(),
        None if names.contains(&"main") => "main".to_owned(),
        None => {
            return usage_error(&format!(
                "the program has several graphs ({}); choose one with --graph",
                names.join(", ")
            ));
        }
    };
    let Some(g) = program.graphs.iter().find(|g| g.name == graph) else {
        return usage_error(&format!("the program has no graph `{graph}`"));
    };

    // Each `--param value` becomes a value of the parameter's type.
    let mut args = serde_json::Map::new();
    for (name, raw) in &values {
        let Some((_, ty)) = g.params.iter().find(|(p, _)| p == name) else {
            let params: Vec<&str> = g.params.iter().map(|(p, _)| p.as_str()).collect();
            return usage_error(&format!(
                "graph `{graph}` has no parameter `{name}` (parameters: {})",
                params.join(", ")
            ));
        };
        match parse_arg(ty, raw) {
            Ok(v) => {
                args.insert(name.clone(), v);
            }
            Err(e) => return usage_error(&format!("--{name}: {e}")),
        }
    }
    for (p, ty) in &g.params {
        if !args.contains_key(p) {
            return usage_error(&format!("missing --{p} (a `{ty}`) for graph `{graph}`"));
        }
    }

    let config = match config_path {
        Some(path) => calyx_runtime::Config::load(&path),
        None => {
            let dir = std::path::Path::new(&file)
                .parent()
                .filter(|d| !d.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            calyx_runtime::Config::discover(dir)
        }
    };
    let config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("calyx: {e}");
            return ExitCode::from(2);
        }
    };
    let opts = calyx_runtime::RunOptions {
        config,
        fake_models,
        trace: !quiet,
    };
    match calyx_runtime::run(
        &program.to_json(),
        &graph,
        &serde_json::Value::Object(args),
        opts,
    ) {
        Ok(serde_json::Value::String(s)) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Ok(v) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("calyx: execution failed: {e}");
            ExitCode::from(3)
        }
    }
}

/// Converts a command-line text into a value of a parameter's type.
fn parse_arg(ty: &str, raw: &str) -> Result<serde_json::Value, String> {
    match ty {
        "Text" => Ok(serde_json::Value::String(raw.to_owned())),
        "Nat" | "Int" => raw
            .parse::<i64>()
            .ok()
            .filter(|n| ty == "Int" || *n >= 0)
            .map(serde_json::Value::from)
            .ok_or_else(|| format!("expected a `{ty}`, got `{raw}`")),
        "Float" => raw
            .parse::<f64>()
            .map(serde_json::Value::from)
            .map_err(|_| format!("expected a `Float`, got `{raw}`")),
        "Bool" => match raw {
            "true" => Ok(true.into()),
            "false" => Ok(false.into()),
            _ => Err(format!("expected `true` or `false`, got `{raw}`")),
        },
        // Lists and records are written as JSON.
        _ => serde_json::from_str(raw).map_err(|e| format!("expected JSON for a `{ty}`: {e}")),
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("calyx: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}
