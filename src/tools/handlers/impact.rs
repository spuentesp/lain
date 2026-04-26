//! Impact analysis domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;

pub fn get_blast_radius(graph: &GraphDatabase, symbol: &str, include_coupling: bool) -> Result<String, LainError> {
    let node = graph.get_node(symbol)?
        .ok_or_else(|| LainError::NotFound(format!("Symbol not found: {}", symbol)))?;

    let mut output = format!("Blast radius for '{}':\n- {} ({:?})",
        symbol, node.name, node.node_type
    );

    if include_coupling {
        let partners = graph.get_co_change_partners(&node.path)?;
        if !partners.is_empty() {
            output.push_str("\n\nCoupled Files (Git Co-Changes):\n");
            for (p, c) in partners.iter().take(5) {
                output.push_str(&format!("- {} (changed together {} times)\n", p, c));
            }
        }
    }

    Ok(output)
}

pub fn get_coupling_radar(graph: &GraphDatabase, symbol: &str) -> Result<String, LainError> {
    let node = graph.get_node(symbol)?;
    let Some(node) = node else {
        return Ok(format!("Symbol '{}' not found in graph", symbol));
    };

    let partners = graph.get_co_change_partners(&node.path)?;

    if partners.is_empty() {
        return Ok(format!(
            "No co-change coupling found for '{}' ({})",
            symbol, node.path
        ));
    }

    Ok(format!(
        "Files that co-change with '{}' ({}) — top {} partners:\n{}",
        symbol,
        node.path,
        partners.len(),
        partners.iter().take(10).enumerate().map(|(i, (p, c))| {
            format!("{}. {} (changed together {} times)", i + 1, p, c)
        }).collect::<Vec<_>>().join("\n")
    ))
}
