//! Testing domain handlers - test coverage and generation utilities

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::NodeType;

pub fn find_untested_functions(
    graph: &GraphDatabase,
    _overlay: &VolatileOverlay,
    limit: Option<usize>,
) -> Result<String, LainError> {
    let all_functions = graph.get_nodes_by_type(NodeType::Function)?;

    let untested: Vec<_> = all_functions
        .into_iter()
        .filter(|f| f.fan_in.unwrap_or(0) == 0)
        .collect();

    if untested.is_empty() {
        return Ok("All functions appear to have callers or tests. No obvious untested functions found.".to_string());
    }

    let max = limit.unwrap_or(20);
    let mut result = format!("## Potentially Untested Functions ({} found)\n\n", untested.len());
    result.push_str("These functions have no incoming call edges - they may be untested or dead code:\n\n");

    for (i, func) in untested.iter().take(max).enumerate() {
        let sig = func.signature.as_deref().unwrap_or("(no signature)");
        result.push_str(&format!("{}. **{}**\n   Path: {}\n   Signature: `{}`\n\n", i + 1, func.name, func.path, sig));
    }

    if untested.len() > max {
        result.push_str(&format!("... and {} more\n", untested.len() - max));
    }

    Ok(result)
}

pub fn get_test_template(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    function_name: &str,
) -> Result<String, LainError> {
    let node = crate::tools::utils::resolve_node(graph, overlay, function_name)?;

    let sig = node.signature.as_deref().unwrap_or("");
    let return_type = extract_return_type(sig);

    let struct_name = node.path.split('/').next_back()
        .unwrap_or("module")
        .replace(".rs", "");

    let mut template = String::new();
    template.push_str(&format!("# Test template for {} (auto-generated)\n\n", function_name));
    template.push_str("```rust\n");
    template.push_str("#[cfg(test)]\n");
    template.push_str(&format!("mod {} {{\n", struct_name.replace("-", "_")));
    template.push_str("    use super::*;\n\n");

    // Generate test based on function type
    match node.node_type {
        NodeType::Function => {
            template.push_str("    #[test]\n");
            template.push_str(&format!("    fn test_{} {{\n", function_name.replace("::", "_")));
            template.push_str("        // TODO: Set up test fixtures\n\n");
            if return_type != "()" {
                template.push_str("        // TODO: Assert expected results\n");
            }
            template.push_str("    }\n");
        }
        NodeType::Struct => {
            template.push_str("    #[test]\n");
            template.push_str(&format!("    fn test_{}_new {{\n", struct_name.replace("-", "_")));
            template.push_str("        // TODO: Test constructor\n");
            template.push_str("    }\n\n");
            template.push_str("    #[test]\n");
            template.push_str(&format!("    fn test_{}_default {{\n", struct_name.replace("-", "_")));
            template.push_str("        // TODO: Test Default impl\n");
            template.push_str("    }\n");
        }
        NodeType::Enum => {
            template.push_str("    #[test]\n");
            template.push_str(&format!("    fn test_{}_variants {{\n", struct_name.replace("-", "_")));
            template.push_str("        // TODO: Test each enum variant\n");
            template.push_str("    }\n");
        }
        _ => {
            template.push_str("    #[test]\n");
            template.push_str(&format!("    fn test_{} {{\n", function_name.replace("::", "_")));
            template.push_str("        // TODO: Implement test\n");
            template.push_str("    }\n");
        }
    }

    template.push_str("}\n");
    template.push_str("```\n");
    template.push_str("\n**Note:** This is a basic scaffold. Adjust based on actual function behavior.\n");

    Ok(template)
}

pub fn find_test_file(
    _graph: &GraphDatabase,
    module_path: &str,
) -> Result<String, LainError> {
    // Strip file extension and common suffixes
    let base_path = module_path
        .replace(".rs", "")
        .replace("/src/", "/tests/");

    // Common test file patterns
    let patterns = vec![
        format!("{}_test.rs", base_path),
        format!("{}_tests.rs", base_path),
        format!("{}/mod.rs", base_path),
        base_path.replace(".rs", "/tests.rs"),
    ];

    // Check for inline tests module
    let inline_test_path = module_path.to_string();
    if inline_test_path.contains("/src/") {
        // Check if original file has #[cfg(test)] module
        let file_name = module_path.split('/').next_back().unwrap_or("module");
        return Ok(format!(
            "Inline tests may exist in `{0}`\n\nTo find tests, search for:\n- `mod tests` or `#[cfg(test)]` in {0}\n- Test files: tests/{1}",
            module_path,
            file_name
        ));
    }

    // Search in tests directory
    for pattern in patterns {
        if std::path::Path::new(&pattern).exists() {
            return Ok(format!("Found test file: {}", pattern));
        }
    }

    Ok(format!(
        "No dedicated test file found for `{}`.\nSearch for `mod tests` or `#[cfg(test)]` within the source file itself.",
        module_path
    ))
}

pub fn get_coverage_summary(
    graph: &GraphDatabase,
    _overlay: &VolatileOverlay,
    module_path: Option<&str>,
) -> Result<String, LainError> {
    // Note: Real coverage would require running `cargo llvm-cov` or similar
    // Here we provide a structural estimate based on graph connectivity

    let all_nodes = graph.get_all_nodes();

    let (total_functions, untested_functions) = if let Some(path) = module_path {
        let module_funcs: Vec<_> = all_nodes.iter()
            .filter(|n| n.path.contains(path) && n.node_type == NodeType::Function)
            .collect();
        let untested = module_funcs.iter().filter(|f| f.fan_in.unwrap_or(0) == 0).count();
        (module_funcs.len(), untested)
    } else {
        let funcs: Vec<_> = all_nodes.iter()
            .filter(|n| n.node_type == NodeType::Function)
            .collect();
        let untested = funcs.iter().filter(|f| f.fan_in.unwrap_or(0) == 0).count();
        (funcs.len(), untested)
    };

    let coverage_pct = if total_functions > 0 {
        ((total_functions - untested_functions) as f64 / total_functions as f64) * 100.0
    } else {
        100.0
    };

    let mut result = String::from("## Code Coverage Estimate\n\n");
    result.push_str(&format!("**Total functions:** {}\n", total_functions));
    result.push_str(&format!("**Potentially untested:** {}\n", untested_functions));
    result.push_str(&format!("**Estimated coverage:** {:.1}%\n\n", coverage_pct));

    if coverage_pct < 70.0 {
        result.push_str("⚠️ Coverage is below recommended levels. Consider adding more tests.\n");
    } else if coverage_pct >= 90.0 {
        result.push_str("✅ Excellent coverage!\n");
    } else {
        result.push_str("ℹ️ Consider adding tests for untested functions.\n");
    }

    result.push_str("\n*Note: This is a structural estimate, not actual line-level coverage.*\n");

    Ok(result)
}

fn extract_return_type(signature: &str) -> &str {
    // Simple heuristics for common patterns
    if let Some(pos) = signature.rfind("->") {
        let ret = signature[pos + 2..].trim();
        // Remove where clause
        if let Some(pos) = ret.find('<') {
            return &ret[..pos];
        }
        if let Some(pos) = ret.find(';') {
            return &ret[..pos];
        }
        return ret;
    }
    "()"
}