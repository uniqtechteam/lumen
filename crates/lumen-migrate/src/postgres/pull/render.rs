//! The new schema as text; `lumen format` lays it out afterwards.

use std::fmt::Write as _;

use super::build::Block;
use super::existing::Around;

fn indented(out: &mut String, comments: &str) {
    for line in comments.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "  {line}");
        }
    }
}

fn line(out: &mut String, around: &Around, text: &str) {
    if around.blank_before {
        out.push('\n');
    }
    indented(out, &around.above);
    out.push_str("  ");
    out.push_str(text.trim_end());
    if !around.trailing.is_empty() {
        out.push(' ');
        out.push_str(&around.trailing);
    }
    out.push('\n');
}

/// One block.
#[must_use]
pub fn block(b: &Block) -> String {
    let mut out = String::new();
    for note in &b.notes {
        let _ = writeln!(out, "// lumen: {note}");
    }
    out.push_str(&b.above);
    let _ = writeln!(out, "{} {} {{", b.keyword, b.name);
    for l in &b.lines {
        let text = format!("{} {} {}", l.name, l.ty, l.attributes.join(" "));
        line(&mut out, &l.around, &text);
    }
    if !b.attributes.is_empty() {
        out.push('\n');
        for a in &b.attributes {
            let around = Around {
                blank_before: false,
                ..a.around.clone()
            };
            line(&mut out, &around, &a.text);
        }
    }
    indented(&mut out, &b.tail);
    out.push_str("}\n");
    out
}
