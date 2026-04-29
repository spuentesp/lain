//! Decorator: composes parser + enricher into enriched output

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;

use crate::tools::handlers::decoration::parsers::ErrorParser;
use crate::tools::handlers::decoration::types::{EnrichedReport, ParsedError, Severity};
use crate::tools::handlers::decoration::enricher::ErrorEnricher;

/// Decorate command output by parsing errors and enriching with graph context
pub fn decorate_output(
    output: &str,
    parser: &(impl ErrorParser + ?Sized),
    enricher: &impl ErrorEnricher,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
) -> String {
    let errors = parser.parse(output);

    // If no structured errors found, return original output
    if errors.is_empty() {
        return output.to_string();
    }

    // Filter to Error and Warning only (skip Note, Help for enrichment)
    let actionable: Vec<ParsedError> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error || e.severity == Severity::Warning)
        .collect();

    if actionable.is_empty() {
        return output.to_string();
    }

    let report = enricher.enrich(&actionable, graph, overlay);
    render_enriched_report(&report)
}

/// Render the enriched report to a string
fn render_enriched_report(report: &EnrichedReport) -> String {
    let error_count = report
        .errors
        .iter()
        .filter(|e| e.error.severity == Severity::Error)
        .count();
    let warning_count = report
        .errors
        .iter()
        .filter(|e| e.error.severity == Severity::Warning)
        .count();
    let file_count = report.summary.affected_files.len();

    let mut out = String::new();
    out.push_str("\n## Enrichment Report\n");
    out.push_str(&format!(
        "{} error(s), {} warning(s) in {} file(s)\n\n",
        error_count, warning_count, file_count
    ));

    // Render each error with its enrichment
    out.push_str("### Errors\n\n");
    for enriched in &report.errors {
        let marker = match enriched.error.severity {
            Severity::Error => "❌",
            Severity::Warning => "⚠️",
            Severity::Note => "  ",
            Severity::Help => "  ",
        };

        out.push_str(&format!(
            "{}{}:{} [{}] {}\n",
            marker,
            enriched.error.path.display(),
            enriched.error.line,
            enriched.error.code.as_deref().unwrap_or(""),
            enriched.error.message
        ));

        if let Some(ref symbol) = enriched.symbol {
            out.push_str("  → in `");
            out.push_str(symbol);
            if let Some(score) = enriched.anchor_score {
                out.push_str(&format!("` (anchor: {:.3})", score));
            } else {
                out.push('`');
            }
            out.push('\n');
        }
    }

    // Render summary
    if let Some(ref note) = report.summary.architectural_note {
        out.push_str("\n### Architectural Context\n\n");
        out.push_str(note);
        out.push_str("\n\n");
    }

    if !report.summary.affected_symbols.is_empty() {
        let symbols: Vec<String> = report
            .summary
            .affected_symbols
            .iter()
            .take(5)
            .map(|s| format!("`{}`", s))
            .collect();
        out.push_str(&format!(
            "**Affected symbols ({}):** {}\n",
            report.summary.affected_symbols.len(),
            symbols.join(", ")
        ));
    }

    if !report.summary.combined_blast_radius.is_empty() {
        let blast: Vec<String> = report
            .summary
            .combined_blast_radius
            .iter()
            .take(5)
            .map(|s| format!("`{}`", s))
            .collect();
        out.push_str(&format!(
            "**Transitive blast radius ({} nodes):** {}\n",
            report.summary.combined_blast_radius.len(),
            blast.join(", ")
        ));
    }

    if !report.summary.co_change_partners.is_empty() {
        out.push_str("\n**Frequently co-changes with:**\n");
        for (name, count) in &report.summary.co_change_partners {
            out.push_str(&format!("  - {} ({}x)\n", name, count));
        }
    }

    out
}
