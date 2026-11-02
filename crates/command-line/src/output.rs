use std::fmt::Write;

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

pub fn print_structured(value: &impl Serialize, format: OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(value)?),
        OutputFormat::Table | OutputFormat::Yaml => print!("{}", serde_yaml_ng::to_string(value)?),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
