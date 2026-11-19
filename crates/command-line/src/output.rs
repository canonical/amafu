use std::fmt::Write as _;
use std::io::Write as _;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Yaml,
}

pub struct Table {
    headings: Vec<&'static str>,
    rows: Vec<Vec<String>>,
}

impl Table {
    #[must_use]
    pub fn new(headings: Vec<&'static str>) -> Self {
        Self { headings, rows: Vec::new() }
    }

    pub fn add(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut widths: Vec<usize> = self.headings.iter().map(|heading| heading.len()).collect();
        for row in &self.rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.chars().count());
            }
        }
        let mut rendered = String::new();
        let headings: Vec<String> = self.headings.iter().map(|heading| (*heading).to_owned()).collect();
        for row in std::iter::once(&headings).chain(&self.rows) {
            let cells: Vec<String> =
                row.iter().zip(&widths).map(|(cell, width)| format!("{cell:<width$}")).collect();
            let _ = writeln!(rendered, "{}", cells.join("  ").trim_end());
        }
        rendered
    }
}

pub fn emit(text: &str) -> std::io::Result<()> {
    let mut standard_output = std::io::stdout().lock();
    standard_output.write_all(text.as_bytes())?;
    standard_output.flush()
}

pub fn emit_line(line: impl std::fmt::Display) -> std::io::Result<()> {
    emit(&format!("{line}\n"))
}

#[must_use]
pub fn is_closed_output(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

pub fn print_structured(value: &impl Serialize, format: OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Json => emit_line(serde_json::to_string_pretty(value)?)?,
        OutputFormat::Table | OutputFormat::Yaml => emit(&serde_yaml_ng::to_string(value)?)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_pipe_is_recognised_through_the_error_chain() {
        let closed =
            anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::BrokenPipe)).context("printing");
        assert!(is_closed_output(&closed));
        assert!(!is_closed_output(&anyhow::anyhow!("the API refused the request")));
    }

    #[test]
    fn columns_are_aligned_to_the_widest_cell() {
        let mut table = Table::new(vec!["NAME", "PHASE"]);
        table.add(vec![String::from("web-1"), String::from("running")]);
        table.add(vec![String::from("database-primary"), String::from("pending")]);
        assert_eq!(
            table.render(),
            "NAME              PHASE\nweb-1             running\ndatabase-primary  pending\n"
        );
    }
}
