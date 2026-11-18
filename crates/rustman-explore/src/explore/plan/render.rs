//! The plan as `explore plan` prints it: a summary line and one row per
//! probe.

use std::fmt::Write as _;

use super::{Disposition, Plan};

const HEADERS: [&str; 6] = ["ID", "FAMILY", "OPERATION", "METHOD", "EXPECTED", "STATUS"];

/// The plan as a table.
pub fn render(plan: &Plan) -> String {
    let count =
        |f: fn(&Disposition) -> bool| plan.probes.iter().filter(|p| f(&p.disposition)).count();
    let allowed = count(|d| *d == Disposition::Allowed);
    let blocked = count(|d| matches!(d, Disposition::Blocked(_)));
    let skipped = count(|d| matches!(d, Disposition::Skipped(_)));
    let mut out = format!(
        "plan: {} probe{} ({allowed} allowed, {blocked} blocked, {skipped} skipped), seed {}\n",
        plan.probes.len(),
        if plan.probes.len() == 1 { "" } else { "s" },
        plan.seed
    );
    if !plan.scans.is_empty() {
        let scans: Vec<&str> = plan.scans.iter().map(|f| f.as_str()).collect();
        let _ = writeln!(out, "scans of every response: {}", scans.join(", "));
    }
    if plan.probes.is_empty() {
        return out;
    }
    let rows: Vec<[String; 6]> = plan
        .probes
        .iter()
        .map(|p| {
            [
                p.id.clone(),
                p.family.to_string(),
                p.operation.clone(),
                p.method.clone(),
                p.expected.clone(),
                p.disposition.to_string(),
            ]
        })
        .collect();
    let mut widths = HEADERS.map(str::len);
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    out.push('\n');
    line(&mut out, &HEADERS.map(str::to_owned), &widths);
    for row in &rows {
        line(&mut out, row, &widths);
    }
    out
}

fn line(out: &mut String, cells: &[String; 6], widths: &[usize; 6]) {
    let mut text = String::new();
    for (i, (cell, width)) in cells.iter().zip(widths).enumerate() {
        if i + 1 == cells.len() {
            text.push_str(cell);
        } else {
            let _ = write!(text, "{cell:<width$}  ");
        }
    }
    out.push_str(text.trim_end());
    out.push('\n');
}
