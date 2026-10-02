//! The `calyx` command-line tool.
//!
//! Exit codes: 0 = ok, 1 = the program has errors, 2 = usage or I/O error,
//! 3 = the execution failed, 4 = the run waits for a message (`receive`).

mod bundle;
mod runs;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Instant;

use calyx_runtime::Mode;

const USAGE: &str = "\
usage: calyx <command> [options]

commands:
  check <file.clyx> [--format human|json] [--time] [--ir | --ir-json]
      Verify a program without generating code.
      --ir prints the compiled graph template when there are no errors;
      --ir-json prints it in the JSON form the runtime loads.
  run <file.clyx> [--graph NAME] [--fake-models] [--quiet] [--config FILE]
                 [--no-journal] [--deterministic] [--budget USD] [--PARAM VALUE ...]
      Check and run a graph. Each parameter of the graph is passed as
      `--name value` (e.g. --topic \"energia solar\"); lists and records
      as JSON, or `@file.json` to read the JSON from a file. Every call is
      recorded in the run's journal, in .calyx/runs/<id>/.
      --graph chooses the graph (default: the only one, or `main`).
      --fake-models answers every model call with fake values shaped by the
      prompt's type, with no network or API key.
      --quiet hides the trace (one line per node and effect, on stderr).
      --config uses this calyx.toml instead of looking for one next to the
      program and in its parent directories.
      --no-journal runs without a journal (nothing can be resumed).
      --deterministic runs one call at a time, always in the same order.
      --budget replaces the program's budget (in USD).
      Independent calls run in parallel, up to the graph's `limits threads`
      (8 by default).
  resume <run> [--fake-models] [--quiet] [--config FILE] [--budget USD]
               [--uncertain done|retry|failed]
      Continue an interrupted or failed run. Calls already in its journal
      are taken from it, not made (or paid for) again. --budget raises the
      budget of a run that used it up. --uncertain says what happened to
      `write once` calls of unknown outcome (after you checked): `done`
      (it happened), `retry` (it did not: make it) or `failed`; without
      it, each tool's `on_uncertain` decides.
  replay <run> [--quiet]
      Run again using only the journal: no model or tool is called.
  runs
      List the runs in .calyx/runs.
  deliver <run> <Message> <json>
      Deliver a message to a run waiting on `receive Message`: checked
      against the message's type, then given to the oldest such receive.
      Continue the run with `calyx resume <run>` or `calyx tick`.
  tick [--fake-models] [--quiet] [--config FILE]
      Resume every waiting run that got a message or whose deadline passed.
      Meant for a scheduler such as cron: no server is needed.
  build <file.clyx> [-o FILE] [--graph NAME] [--config FILE | --no-config]
      Check a program and write a standalone executable that runs it: a
      copy of this binary with the program (and its calyx.toml) inside.
      It needs neither Calyx nor a C compiler where it runs; graph
      parameters become its options (`./research --topic ...`).
      -o names the executable (default: the file's name without .clyx).
      --graph chooses the graph it runs (default: the only one, or `main`).
      --no-config leaves the calyx.toml out.
  version
      Print the version.
";

/// How the user calls this binary in messages: `calyx`, or the name of a
/// built program.
static COMMAND: OnceLock<String> = OnceLock::new();

fn command() -> &'static str {
    COMMAND.get().map_or("calyx", String::as_str)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some((exe, bundle)) = bundle::current() {
        return built(exe, bundle, &args);
    }
    match args.first().map(String::as_str) {
        Some("check") => check(&args[1..]),
        Some("run") => run(&args[1..]),
        Some("build") => build(&args[1..]),
        Some("resume") => rerun(&args[1..], Mode::Resume),
        Some("replay") => rerun(&args[1..], Mode::Replay),
        Some("runs") => list_runs(),
        Some("deliver") => deliver(&args[1..]),
        Some("tick") => tick(&args[1..]),
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

/// A binary made by `calyx build`: runs the program inside it.
fn built(exe: PathBuf, bundle: bundle::Bundle, args: &[String]) -> ExitCode {
    let name = exe.file_name().map_or_else(
        || "program".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let _ = COMMAND.set(name);
    let origin = Origin::Bundle(exe, bundle);
    match args.first().map(String::as_str) {
        Some("resume") => rerun(&args[1..], Mode::Resume),
        Some("replay") => rerun(&args[1..], Mode::Replay),
        Some("runs") => list_runs(),
        Some("deliver") => deliver(&args[1..]),
        Some("tick") => tick(&args[1..]),
        Some("--version" | "-V") => {
            println!("{} (calyx {})", command(), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{}", built_usage(&origin));
            ExitCode::SUCCESS
        }
        _ => match parse_run(args, false) {
            Ok((_, flags)) => start(origin, flags),
            Err(code) => code,
        },
    }
}

fn built_usage(origin: &Origin) -> String {
    let Origin::Bundle(_, b) = origin else {
        return USAGE.to_owned();
    };
    let params = compile(origin)
        .ok()
        .and_then(|p| p.graphs.into_iter().find(|g| g.name == b.graph))
        .map(|g| {
            g.params
                .iter()
                .map(|(p, ty)| format!(" --{p} <{ty}>"))
                .collect::<String>()
        })
        .unwrap_or_default();
    let cmd = command();
    format!(
        "\
usage: {cmd}{params} [options]
       {cmd} resume <run> | replay <run> | runs

Runs graph `{graph}` of {file}, built with calyx {version}.
Values that are lists or records are written as JSON.

options:
  --fake-models     answer model calls with fake values (no network, no key)
  --quiet           hide the trace
  --config FILE     use this calyx.toml instead of the one built in
  --no-journal      run without a journal (nothing can be resumed)
  --deterministic   one call at a time, always in the same order
  --budget USD      replace the program's budget

resume also takes --uncertain done|retry|failed (see `calyx help`).
",
        graph = b.graph,
        file = b.file,
        version = env!("CARGO_PKG_VERSION"),
    )
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

/// Where a program comes from.
enum Origin {
    File(PathBuf),
    /// A binary made by `calyx build` (D35), and the program inside it.
    Bundle(PathBuf, bundle::Bundle),
}

impl Origin {
    /// A `.clyx` file, or a built binary (a run's journal names either).
    fn open(path: &Path) -> Origin {
        match bundle::read(path) {
            Some(b) => Origin::Bundle(path.to_path_buf(), b),
            None => Origin::File(path.to_path_buf()),
        }
    }

    fn path(&self) -> &Path {
        match self {
            Origin::File(p) | Origin::Bundle(p, _) => p,
        }
    }
}

/// The options of `calyx run` (and of a built binary).
#[derive(Default)]
struct RunFlags {
    graph: Option<String>,
    config_path: Option<PathBuf>,
    fake_models: bool,
    quiet: bool,
    no_journal: bool,
    deterministic: bool,
    budget_usd: Option<f64>,
    values: Vec<(String, String)>,
}

/// Parses run options; with `with_file`, also the program's file.
fn parse_run(args: &[String], with_file: bool) -> Result<(Option<PathBuf>, RunFlags), ExitCode> {
    let mut file = None;
    let mut f = RunFlags::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--graph" => match it.next() {
                Some(g) => f.graph = Some(g.clone()),
                None => return Err(usage_error("--graph expects a name")),
            },
            "--config" => match it.next() {
                Some(c) => f.config_path = Some(PathBuf::from(c)),
                None => return Err(usage_error("--config expects a file")),
            },
            "--fake-models" => f.fake_models = true,
            "--quiet" => f.quiet = true,
            "--no-journal" => f.no_journal = true,
            "--deterministic" => f.deterministic = true,
            "--budget" => match it.next().map(|b| parse_budget(b)) {
                Some(Ok(b)) => f.budget_usd = Some(b),
                Some(Err(e)) => return Err(usage_error(&e)),
                None => return Err(usage_error("--budget expects an amount in USD")),
            },
            a if a.starts_with("--") => match it.next() {
                Some(v) => f.values.push((a[2..].to_owned(), v.clone())),
                None => return Err(usage_error(&format!("`{a}` expects a value"))),
            },
            a if with_file && file.is_none() => file = Some(PathBuf::from(a)),
            a if with_file => {
                return Err(usage_error(&format!(
                    "run takes a single file, got `{a}` too"
                )));
            }
            a => return Err(usage_error(&format!("unexpected `{a}`"))),
        }
    }
    Ok((file, f))
}

fn run(args: &[String]) -> ExitCode {
    match parse_run(args, true) {
        Ok((Some(file), flags)) => start(Origin::File(file), flags),
        Ok((None, _)) => usage_error("run needs a file"),
        Err(code) => code,
    }
}

/// The graph to run: the one asked for, the only one, or `main`.
fn choose_graph(program: &calyx_ir::Program, asked: Option<String>) -> Result<String, ExitCode> {
    let names: Vec<&str> = program.graphs.iter().map(|g| g.name.as_str()).collect();
    let graph = match asked {
        Some(g) => g,
        None if names.len() == 1 => names[0].to_owned(),
        None if names.contains(&"main") => "main".to_owned(),
        None => {
            return Err(usage_error(&format!(
                "the program has several graphs ({}); choose one with --graph",
                names.join(", ")
            )));
        }
    };
    if !names.contains(&graph.as_str()) {
        return Err(usage_error(&format!("the program has no graph `{graph}`")));
    }
    Ok(graph)
}

/// Starts a new run.
fn start(origin: Origin, flags: RunFlags) -> ExitCode {
    let program = match compile(&origin) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let asked = match (&origin, flags.graph) {
        (_, Some(g)) => Some(g),
        (Origin::Bundle(_, b), None) => Some(b.graph.clone()),
        (Origin::File(_), None) => None,
    };
    let graph = match choose_graph(&program, asked) {
        Ok(g) => g,
        Err(code) => return code,
    };
    let g = program
        .graphs
        .iter()
        .find(|g| g.name == graph)
        .expect("choose_graph checked it");

    // Each `--param value` becomes a value of the parameter's type.
    let mut args = serde_json::Map::new();
    for (name, raw) in &flags.values {
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

    let config = match load_config(&origin, flags.config_path.as_deref()) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let id = (!flags.no_journal).then(runs::new_id);
    let path = origin.path();
    let opts = calyx_runtime::RunOptions {
        config,
        fake_models: flags.fake_models,
        trace: !flags.quiet,
        journal: id.as_deref().map(runs::dir),
        mode: Mode::New,
        program: Some(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())),
        deterministic: flags.deterministic,
        budget_usd: flags.budget_usd,
        uncertain: None,
    };
    if let Some(id) = &id {
        eprintln!("{}: run {id}", command());
    }
    execute(
        &program,
        &graph,
        serde_json::Value::Object(args),
        opts,
        id.as_deref(),
    )
}

/// `calyx build`: a standalone executable for one graph of a program.
fn build(args: &[String]) -> ExitCode {
    let mut file = None;
    let mut out = None;
    let mut graph = None;
    let mut config_path = None;
    let mut no_config = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" | "--output" => match it.next() {
                Some(o) => out = Some(PathBuf::from(o)),
                None => return usage_error("-o expects a file"),
            },
            "--graph" => match it.next() {
                Some(g) => graph = Some(g.clone()),
                None => return usage_error("--graph expects a name"),
            },
            "--config" => match it.next() {
                Some(c) => config_path = Some(PathBuf::from(c)),
                None => return usage_error("--config expects a file"),
            },
            "--no-config" => no_config = true,
            a if a.starts_with('-') => return usage_error(&format!("unknown option `{a}`")),
            a if file.is_none() => file = Some(PathBuf::from(a)),
            _ => return usage_error("build takes a single file"),
        }
    }
    let Some(file) = file else {
        return usage_error("build needs a file");
    };
    if no_config && config_path.is_some() {
        return usage_error("--config and --no-config exclude each other");
    }
    let origin = Origin::File(file.clone());
    let program = match compile(&origin) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let graph = match choose_graph(&program, graph) {
        Ok(g) => g,
        Err(code) => return code,
    };
    // The calyx.toml goes in as text; it holds no secrets (keys come from
    // the environment, `key_env`). Check it now rather than at run time.
    let config_file = match config_path {
        _ if no_config => None,
        Some(p) => Some(p),
        None => calyx_runtime::Config::find(dir_of(&file)),
    };
    let config = match &config_file {
        None => None,
        Some(p) => match std::fs::read_to_string(p)
            .map_err(|e| format!("cannot read `{}`: {e}", p.display()))
            .and_then(|text| {
                calyx_runtime::Config::parse(&text, Path::new("."))
                    .map_err(|e| format!("{}: {e}", p.display()))?;
                Ok(text)
            }) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("calyx: {e}");
                return ExitCode::from(2);
            }
        },
    };
    // `research.clyx` → `./research`.
    let out = out.unwrap_or_else(|| PathBuf::from(file.file_stem().unwrap_or(file.as_os_str())));
    let runtime = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("calyx: cannot find the calyx executable: {e}");
            return ExitCode::from(2);
        }
    };
    let bundle = bundle::Bundle {
        file: file.file_name().map_or_else(
            || file.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        source: std::fs::read_to_string(&file).unwrap_or_default(),
        graph: graph.clone(),
        config,
    };
    if let Err(e) = bundle::write(&runtime, &bundle, &out) {
        eprintln!("calyx: {e}");
        return ExitCode::from(2);
    }
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    let shown = if out.components().count() == 1 {
        format!("./{}", out.display())
    } else {
        out.display().to_string()
    };
    println!(
        "built {shown} ({:.1} MB): runs graph `{graph}`{}",
        size as f64 / 1_048_576.0,
        match &config_file {
            Some(p) => format!(", with {}", p.display()),
            None => String::new(),
        }
    );
    println!("try `{shown} --help`");
    ExitCode::SUCCESS
}

/// `calyx resume <id>` and `calyx replay <id>`.
fn rerun(args: &[String], mode: Mode) -> ExitCode {
    let mut id = None;
    let mut config_path = None;
    let mut fake_models = false;
    let mut quiet = false;
    let mut deterministic = false;
    let mut budget_usd = None;
    let mut uncertain = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => match it.next() {
                Some(c) => config_path = Some(PathBuf::from(c)),
                None => return usage_error("--config expects a file"),
            },
            "--uncertain" if mode == Mode::Resume => match it.next().map(String::as_str) {
                Some(d @ ("done" | "retry" | "failed")) => uncertain = Some(d.to_owned()),
                _ => return usage_error("--uncertain expects `done`, `retry` or `failed`"),
            },
            "--fake-models" => fake_models = true,
            "--quiet" => quiet = true,
            "--deterministic" => deterministic = true,
            "--budget" => match it.next().map(|b| parse_budget(b)) {
                Some(Ok(b)) => budget_usd = Some(b),
                Some(Err(e)) => return usage_error(&e),
                None => return usage_error("--budget expects an amount in USD"),
            },
            a if a.starts_with('-') => return usage_error(&format!("unknown option `{a}`")),
            a if id.is_none() => id = Some(a.to_owned()),
            _ => return usage_error("give a single run id"),
        }
    }
    let Some(id) = id else {
        return usage_error(&format!("which run? `{} runs` lists them", command()));
    };
    let header = match runs::header(&id) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{}: {e}", command());
            return ExitCode::from(2);
        }
    };
    // The run finishes on the program it started with (D23): the runtime
    // compares the hash of the compiled program with the journal's.
    let origin = Origin::open(&header.program);
    let program = match compile(&origin) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let config = if mode == Mode::Replay {
        calyx_runtime::Config::builtin()
    } else {
        match load_config(&origin, config_path.as_deref()) {
            Ok(c) => c,
            Err(code) => return code,
        }
    };
    let opts = calyx_runtime::RunOptions {
        config,
        fake_models,
        trace: !quiet,
        journal: Some(runs::dir(&id)),
        mode,
        program: Some(header.program.clone()),
        deterministic,
        budget_usd,
        uncertain,
    };
    execute(&program, &header.graph, header.args, opts, Some(&id))
}

/// `calyx deliver <run> <Message> <json>`.
fn deliver(args: &[String]) -> ExitCode {
    let [id, message, raw] = args else {
        return usage_error("deliver takes a run, a message type and the message as JSON");
    };
    let header = match runs::header(id) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{}: {e}", command());
            return ExitCode::from(2);
        }
    };
    let program = match compile(&Origin::open(&header.program)) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let Some((_, schema)) = program.messages.iter().find(|(n, _)| n == message) else {
        let known: Vec<&str> = program.messages.iter().map(|(n, _)| n.as_str()).collect();
        return usage_error(&format!(
            "the program has no message `{message}` (messages: {})",
            known.join(", ")
        ));
    };
    let schema: serde_json::Value = serde_json::from_str(schema).unwrap_or_default();
    // A bare variant name, as text or JSON text, for variants without fields.
    let value =
        serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.clone()));
    let value = normalize(value, &schema);
    if let Err(e) = conforms(&value, &schema) {
        eprintln!("{}: not a `{message}`: {e}", command());
        return ExitCode::from(2);
    }
    match runs::deliver(id, message, &value) {
        Ok(key) => {
            println!(
                "delivered `{message}` to `{key}`; continue the run with `{} resume {id}` or `{} tick`",
                command(),
                command()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", command());
            ExitCode::from(2)
        }
    }
}

/// `Approved` for `{"kind": "Approved"}`, when the type has variants with
/// fields (where every value is a record with `kind`).
fn normalize(v: serde_json::Value, schema: &serde_json::Value) -> serde_json::Value {
    if let (serde_json::Value::String(s), Some(_)) = (&v, schema.get("anyOf")) {
        return serde_json::json!({ "kind": s });
    }
    v
}

/// Does `v` fit `schema` (the subset of JSON Schema the compiler emits)?
fn conforms(v: &serde_json::Value, schema: &serde_json::Value) -> Result<(), String> {
    use serde_json::Value;
    if let Some(any) = schema.get("anyOf").and_then(Value::as_array) {
        // Variants: name the one that does not exist, or check that one.
        let kinds: Vec<&str> = any
            .iter()
            .filter_map(|a| a["properties"]["kind"]["enum"][0].as_str())
            .collect();
        if kinds.len() == any.len() {
            let kind = v["kind"].as_str().unwrap_or_default();
            return match kinds.iter().position(|k| *k == kind) {
                Some(i) => conforms(v, &any[i]),
                None => Err(format!(
                    "`{}` is not one of its variants ({})",
                    v.get("kind")
                        .map_or_else(|| v.to_string(), |k| k.as_str().unwrap_or("?").to_owned()),
                    kinds.join(", ")
                )),
            };
        }
        let mut last = String::from("no alternative fits");
        for alt in any {
            match conforms(v, alt) {
                Ok(()) => return Ok(()),
                Err(e) => last = e,
            }
        }
        return Err(last);
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(v)
    {
        return Err(format!(
            "{v} is not one of {}",
            Value::Array(options.clone())
        ));
    }
    let ok = match schema["type"].as_str() {
        Some("string") => v.is_string(),
        Some("integer") => v.is_i64() || v.is_u64(),
        Some("number") => v.is_number(),
        Some("boolean") => v.is_boolean(),
        Some("array") => {
            let Some(items) = v.as_array() else {
                return Err(format!("expected a list, got {v}"));
            };
            for i in items {
                conforms(i, &schema["items"])?;
            }
            true
        }
        Some("object") => {
            let Some(obj) = v.as_object() else {
                return Err(format!("expected an object, got {v}"));
            };
            for r in schema["required"].as_array().into_iter().flatten() {
                let name = r.as_str().unwrap_or_default();
                let Some(field) = obj.get(name) else {
                    return Err(format!("missing `{name}`"));
                };
                conforms(field, &schema["properties"][name])?;
            }
            true
        }
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "expected a {}, got {v}",
            schema["type"].as_str().unwrap_or("value")
        ))
    }
}

/// `calyx tick`: resumes the waiting runs that can go on.
fn tick(args: &[String]) -> ExitCode {
    let now = runs::now();
    let mut resumed = 0;
    let mut waiting = 0;
    let mut worst = ExitCode::SUCCESS;
    for r in runs::list() {
        if r.status != "waiting" {
            continue;
        }
        let waits = runs::waits(&r.id);
        if !waits.iter().any(|w| w.delivered || w.until <= now) {
            waiting += 1;
            continue;
        }
        eprintln!("{}: resuming {}", command(), r.id);
        let mut a = vec![r.id.clone()];
        a.extend(args.iter().cloned());
        let code = rerun(&a, Mode::Resume);
        if code != ExitCode::SUCCESS && code != ExitCode::from(4) {
            worst = code;
        }
        resumed += 1;
    }
    eprintln!(
        "{}: resumed {resumed} run(s); {waiting} still waiting",
        command()
    );
    worst
}

fn list_runs() -> ExitCode {
    let all = runs::list();
    if all.is_empty() {
        println!("no runs in {}", runs::RUNS_DIR);
        return ExitCode::SUCCESS;
    }
    println!(
        "{:<22} {:<16} {:<12} {:>6}  resumed",
        "run", "graph", "status", "calls"
    );
    for r in all {
        println!(
            "{:<22} {:<16} {:<12} {:>6}  {}",
            r.id, r.graph, r.status, r.calls, r.resumes
        );
    }
    ExitCode::SUCCESS
}

fn dir_of(file: &Path) -> &Path {
    file.parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Reads and checks a program. On errors, prints them.
fn compile(origin: &Origin) -> Result<calyx_ir::Program, ExitCode> {
    let (name, text) = match origin {
        Origin::File(file) => {
            let text = std::fs::read_to_string(file).map_err(|e| {
                eprintln!("{}: cannot read `{}`: {e}", command(), file.display());
                ExitCode::from(2)
            })?;
            (file.display().to_string(), text)
        }
        Origin::Bundle(_, b) => (b.file.clone(), b.source.clone()),
    };
    let report = calyx_check::check(&name, &text);
    if report.has_errors() {
        print!("{}", report.render());
        return Err(ExitCode::from(1));
    }
    Ok(report.ir)
}

/// `--config FILE`; or the `calyx.toml` next to the program or above it;
/// or, for a built binary, the one built in, whose tool commands run in the
/// binary's directory.
fn load_config(
    origin: &Origin,
    explicit: Option<&Path>,
) -> Result<calyx_runtime::Config, ExitCode> {
    let config = match (explicit, origin) {
        (Some(path), _) => calyx_runtime::Config::load(path),
        (None, Origin::File(file)) => calyx_runtime::Config::discover(dir_of(file)),
        (None, Origin::Bundle(_, b)) if b.config.is_none() => Ok(calyx_runtime::Config::builtin()),
        (None, Origin::Bundle(exe, b)) => {
            let text = b.config.as_deref().unwrap_or_default();
            let label = format!("{} (built-in calyx.toml)", exe.display());
            calyx_runtime::Config::parse(text, dir_of(exe))
                .map(|mut c| {
                    c.path = Some(PathBuf::from(&label));
                    c
                })
                .map_err(|e| format!("{label}: {e}"))
        }
    };
    config.map_err(|e| {
        eprintln!("{}: {e}", command());
        ExitCode::from(2)
    })
}

fn execute(
    program: &calyx_ir::Program,
    graph: &str,
    args: serde_json::Value,
    opts: calyx_runtime::RunOptions,
    id: Option<&str>,
) -> ExitCode {
    let mode = opts.mode;
    match calyx_runtime::run(&program.to_json(), graph, &args, opts) {
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
        Err(e) if e.starts_with(runs::WAITING) => {
            let cmd = command();
            eprintln!("{cmd}: the run is {e}");
            if let Some(id) = id {
                for w in runs::waits(id) {
                    if let Some(about) = &w.about {
                        let mut shown = about.to_string();
                        if shown.len() > 500 {
                            let cut = (0..=500)
                                .rev()
                                .find(|&i| shown.is_char_boundary(i))
                                .unwrap_or(0);
                            shown.truncate(cut);
                            shown.push('…');
                        }
                        eprintln!("{cmd}: `{}` is about: {shown}", w.message);
                    }
                    eprintln!(
                        "{cmd}: deliver it with `{cmd} deliver {id} {} '<json>'`; after {} it continues with `on timeout` (`{cmd} resume {id}` or `{cmd} tick`)",
                        w.message,
                        runs::utc(w.until)
                    );
                }
            }
            ExitCode::from(4)
        }
        Err(e) => {
            let cmd = command();
            eprintln!("{cmd}: execution failed: {e}");
            if let Some(id) = id
                && mode != Mode::Replay
            {
                eprintln!(
                    "{cmd}: finished calls are in the journal; continue with `{cmd} resume {id}`"
                );
            }
            ExitCode::from(3)
        }
    }
}

/// `--budget 5` or `--budget 5USD`: an amount in USD.
fn parse_budget(raw: &str) -> Result<f64, String> {
    raw.trim()
        .trim_end_matches("USD")
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|b| *b > 0.0)
        .ok_or_else(|| format!("--budget expects a positive amount in USD, got `{raw}`"))
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
        // A directory: the run works on a copy of it (decision D13).
        "Sandbox" => {
            if std::path::Path::new(raw).is_dir() {
                Ok(serde_json::Value::String(raw.to_owned()))
            } else {
                Err(format!("expected a directory for a `Sandbox`, got `{raw}`"))
            }
        }
        "Bool" => match raw {
            "true" => Ok(true.into()),
            "false" => Ok(false.into()),
            _ => Err(format!("expected `true` or `false`, got `{raw}`")),
        },
        // Lists and records are written as JSON, or `@file` to read the JSON
        // from a file (a command-line argument is limited to 128 KB).
        _ => match raw.strip_prefix('@') {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read `{path}`: {e}"))?;
                serde_json::from_str(&text)
                    .map_err(|e| format!("expected JSON for a `{ty}` in `{path}`: {e}"))
            }
            None => {
                serde_json::from_str(raw).map_err(|e| format!("expected JSON for a `{ty}`: {e}"))
            }
        },
    }
}

fn usage_error(msg: &str) -> ExitCode {
    if COMMAND.get().is_some() {
        eprintln!("{}: {msg}\n(see `{} --help`)", command(), command());
        return ExitCode::from(2);
    }
    eprintln!("calyx: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}
