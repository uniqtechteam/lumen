//! SPEC-SCHEMA-001 AC-001: Prisma's example schemas and schemas from open-source projects, kept
//! under `tests/corpus/` with their licences (see `tests/corpus/README.md`). Each one parses,
//! checks without errors and formats to a fixed point.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use std::path::{Path, PathBuf};

use lumen_schema::{SourceFile, check, format, parse};

/// Every `.prisma` file under `tests/corpus/`, sorted, with its path relative to the corpus.
fn corpus() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "prisma") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // git may check the files out with CRLF on Windows
            let text = std::fs::read_to_string(&p).unwrap().replace("\r\n", "\n");
            (name, text)
        })
        .collect()
}

#[test]
fn ac_001_the_corpus_is_all_there() {
    let names: Vec<String> = corpus().into_iter().map(|(n, _)| n).collect();
    let examples = names
        .iter()
        .filter(|n| n.starts_with("prisma-examples/"))
        .count();
    let projects = names
        .iter()
        .filter(|n| n.ends_with("/schema.prisma"))
        .count();
    assert!(examples >= 50, "{examples} Prisma examples");
    assert!(projects >= 10, "{projects} open-source projects");
}

#[test]
fn ac_001_every_corpus_schema_parses_and_checks() {
    let mut failed = Vec::new();
    for (name, text) in corpus() {
        let file = SourceFile::new(&name, &text);
        let (_, parsed) = parse(&file);
        let (_, checked) = check(&file);
        if parsed.has_errors() || checked.has_errors() {
            failed.push(format!("{name}\n{}", checked.render(&file)));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn ac_001_every_corpus_schema_formats_to_a_fixed_point() {
    let mut failed = Vec::new();
    for (name, text) in corpus() {
        let once = format(&SourceFile::new(&name, &text)).0;
        let twice = format(&SourceFile::new(&name, &once)).0;
        if once != twice {
            failed.push(format!("{name}: formatting twice changed the file"));
        }
        let file = SourceFile::new(&name, &once);
        let (_, d) = check(&file);
        if d.has_errors() {
            failed.push(format!(
                "{name}: formatted, it no longer checks\n{}",
                d.render(&file)
            ));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
