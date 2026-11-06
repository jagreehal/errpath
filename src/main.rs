use cargo_errpath::review::{Options, run};
use cargo_errpath::{analyse_path, render};
use std::path::Path;
use std::process::exit;

const USAGE: &str = "usage:
  cargo errpath ir [path]            JSON IR for a file or directory (default: src)
  cargo errpath diagram <path> <fn>  Mermaid diagram for one function (id or name)
  cargo errpath review --base <ref> [--head <ref>] [paths...]
      [--format markdown|json] [--diagrams open|collapsed] [-o <file>]";

fn fail(code: i32, msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    exit(code)
}

fn main() {
    // `cargo errpath x` runs `cargo-errpath errpath x`.
    let args: Vec<String> = std::env::args()
        .skip(1)
        .skip_while(|a| a == "errpath")
        .collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let load =
        |p: &str| analyse_path(Path::new(p)).unwrap_or_else(|e| fail(1, format!("{p}: {e}")));
    match args[..] {
        ["ir"] | ["ir", _] => {
            let report = load(args.get(1).copied().unwrap_or("src"));
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("IR serializes")
            );
        }
        ["diagram", path, name] => {
            let report = load(path);
            let suffix = format!("::{name}");
            let hits: Vec<_> = report
                .functions
                .iter()
                .filter(|f| f.id == name || f.id.ends_with(&suffix) || f.name == name)
                .collect();
            match hits[..] {
                [f] => print!("{}", render(&f.name, &f.nodes)),
                [] => fail(1, format!("no fn `{name}` in {path}")),
                _ => {
                    let ids: Vec<_> = hits.iter().map(|f| f.id.as_str()).collect();
                    fail(1, format!("`{name}` is ambiguous:\n  {}", ids.join("\n  ")))
                }
            }
        }
        ["review", ref rest @ ..] => review(rest),
        _ => fail(2, USAGE),
    }
}

fn review(args: &[&str]) {
    let (mut base, mut head, mut format, mut diagrams, mut out) =
        (None, "HEAD", "markdown", "open", None);
    let mut paths = vec![];
    let mut args = args.iter().copied();
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| fail(2, USAGE));
        match a {
            "--base" => base = Some(value()),
            "--head" => head = value(),
            "--format" => format = value(),
            "--diagrams" => diagrams = value(),
            "-o" => out = Some(value()),
            flag if flag.starts_with('-') => fail(2, USAGE),
            path => paths.push(path),
        }
    }
    let Some(base) = base else { fail(2, USAGE) };
    if !["markdown", "json"].contains(&format) || !["open", "collapsed"].contains(&diagrams) {
        fail(2, USAGE);
    }
    let options = Options {
        base,
        head,
        paths: &paths,
        collapse_diagrams: diagrams == "collapsed",
    };
    let review = run(&options).unwrap_or_else(|e| fail(1, e));
    if !review.changed {
        eprintln!("{}", review.summary);
    }
    let text = match format {
        "json" => serde_json::to_string_pretty(&review).expect("review serializes") + "\n",
        _ => review.markdown.clone(),
    };
    match out {
        Some(path) => {
            std::fs::write(path, text).unwrap_or_else(|e| fail(1, format!("{path}: {e}")))
        }
        None => print!("{text}"),
    }
}
