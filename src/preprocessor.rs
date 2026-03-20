/// Scribble-like preprocessor: converts markup + @-expressions into Scheme render DSL.
///
/// Rules:
/// - Lines starting with `(` — raw Scheme, passed through as-is
/// - `# text` → `(bold "text")`
/// - `## text` → `(bold "text")` (same, just convention)
/// - `---` → `(hr)`
/// - `@(expr)` inside text → inline Scheme expression
/// - `@name` inside text → Scheme variable reference
/// - `| a | b |` blocks → `(table ...)`
/// - Plain text lines → `(text "...")`  with @-expressions spliced in
/// - Blank lines separate paragraphs (ignored)

pub fn preprocess(source: &str) -> String {
    let mut scheme_lines: Vec<String> = Vec::new();
    let mut render_parts: Vec<String> = Vec::new();
    let mut table_state: Option<TableBuilder> = None;
    let mut in_render = false;

    for line in source.lines() {
        let trimmed = line.trim();

        // Blank line — flush table if any
        if trimmed.is_empty() {
            if let Some(table) = table_state.take() {
                render_parts.push(table.finish());
            }
            continue;
        }

        // Skip (input ...) and (output ...) — parsed statically by Rust
        if trimmed.starts_with("(input ") || trimmed.starts_with("(output ") {
            continue;
        }

        // Raw Scheme lines (start with `(` but not table/heading)
        // We detect Scheme by: starts with ( and is NOT a render line
        if trimmed.starts_with('(') && !in_render {
            // Flush any pending table
            if let Some(table) = table_state.take() {
                render_parts.push(table.finish());
            }
            scheme_lines.push(trimmed.to_string());
            continue;
        }

        // From here on, we're in "render" territory
        if !in_render {
            in_render = true;
        }

        // Horizontal rule
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            if let Some(table) = table_state.take() {
                render_parts.push(table.finish());
            }
            render_parts.push("(hr)".to_string());
            continue;
        }

        // Heading
        if trimmed.starts_with("# ") {
            if let Some(table) = table_state.take() {
                render_parts.push(table.finish());
            }
            let text = &trimmed[2..];
            render_parts.push(format!("(bold {})", process_inline(text)));
            continue;
        }
        if trimmed.starts_with("## ") {
            if let Some(table) = table_state.take() {
                render_parts.push(table.finish());
            }
            let text = &trimmed[3..];
            render_parts.push(format!("(italic {})", process_inline(text)));
            continue;
        }

        // Table row
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            let cells: Vec<&str> = trimmed
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim())
                .collect();

            // Skip separator rows like |---|---|
            if cells.iter().all(|c| c.chars().all(|ch| ch == '-' || ch == ':')) {
                continue;
            }

            if let Some(ref mut table) = table_state {
                table.add_row(&cells);
            } else {
                let mut tb = TableBuilder::new();
                tb.set_headers(&cells);
                table_state = Some(tb);
            }
            continue;
        }

        // If we were building a table but this line is not a table row, flush
        if let Some(table) = table_state.take() {
            render_parts.push(table.finish());
        }

        // Standalone @(expr) on its own line
        if trimmed.starts_with("@(") && trimmed.ends_with(')') {
            let expr = &trimmed[1..]; // keep the parens
            render_parts.push(expr.to_string());
            continue;
        }

        // Plain text with possible @-expressions
        render_parts.push(format!("(text {})", process_inline(trimmed)));
    }

    // Flush remaining table
    if let Some(table) = table_state.take() {
        render_parts.push(table.finish());
    }

    // Build output
    let mut output = String::new();

    // Scheme preamble (defines, inputs, outputs)
    for line in &scheme_lines {
        output.push_str(line);
        output.push('\n');
    }

    // Render block
    if !render_parts.is_empty() {
        output.push_str("(render\n");
        for part in &render_parts {
            output.push_str("  ");
            output.push_str(part);
            output.push('\n');
        }
        output.push(')');
    }

    output
}

/// Process inline text: convert @expr and @(expr) to Scheme splices.
/// Returns a string suitable as arguments to (text ...) or (bold ...).
/// E.g. "The sum is @sum." → "\"The sum is \" sum \".\""
fn process_inline(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '@' && i + 1 < chars.len() {
            // Flush current text
            if !current.is_empty() {
                parts.push(format!("\"{}\"", escape_scheme_string(&current)));
                current.clear();
            }

            i += 1; // skip @

            if chars[i] == '(' {
                // @(expr) — find matching close paren
                let start = i;
                let mut depth = 0;
                while i < chars.len() {
                    if chars[i] == '(' {
                        depth += 1;
                    } else if chars[i] == ')' {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
                let expr: String = chars[start..i].iter().collect();
                parts.push(expr);
            } else {
                // @name — read identifier chars
                let start = i;
                while i < chars.len() && is_scheme_ident(chars[i]) {
                    i += 1;
                }
                if i > start {
                    let name: String = chars[start..i].iter().collect();
                    parts.push(name);
                } else {
                    current.push('@');
                }
            }
        } else {
            current.push(chars[i]);
            i += 1;
        }
    }

    if !current.is_empty() {
        parts.push(format!("\"{}\"", escape_scheme_string(&current)));
    }

    if parts.is_empty() {
        "\"\"".to_string()
    } else {
        parts.join(" ")
    }
}

fn is_scheme_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == '?' || c == '!'
}

fn escape_scheme_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

struct TableBuilder {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl TableBuilder {
    fn new() -> Self {
        Self {
            headers: Vec::new(),
            rows: Vec::new(),
        }
    }

    fn set_headers(&mut self, cells: &[&str]) {
        self.headers = cells.iter().map(|c| c.to_string()).collect();
    }

    fn add_row(&mut self, cells: &[&str]) {
        self.rows
            .push(cells.iter().map(|c| c.to_string()).collect());
    }

    fn finish(&self) -> String {
        let headers_scheme: Vec<String> = self
            .headers
            .iter()
            .map(|h| process_inline(h))
            .collect();

        let rows_scheme: Vec<String> = self
            .rows
            .iter()
            .map(|row| {
                let cells: Vec<String> = row.iter().map(|c| process_inline(c)).collect();
                format!("(list {})", cells.join(" "))
            })
            .collect();

        format!(
            "(table (list {}) (list {}))",
            headers_scheme.join(" "),
            rows_scheme.join(" ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_preprocess() {
        let input = r#"(input x f64)
(output result f64)

(define result (* x 2))

# My Analysis

The result is @result.

---

| name | value |
|------|-------|
| x    | @x    |

@(plot-line '(1 2 3) "test")
"#;
        let output = preprocess(input);
        println!("--- preprocessed ---\n{}\n---", output);

        assert!(!output.contains("(input x f64)"));  // stripped
        assert!(!output.contains("(output result f64)"));  // stripped
        assert!(output.contains("(define result (* x 2))"));
        assert!(output.contains("(render"));
        assert!(output.contains("(bold \"My Analysis\")"));
        assert!(output.contains("(text \"The result is \" result \".\")"));
        assert!(output.contains("(hr)"));
        assert!(output.contains("(table"));
        assert!(output.contains("(plot-line '(1 2 3) \"test\")"));
    }

    #[test]
    fn test_inline_expressions() {
        assert_eq!(
            process_inline("hello @name world"),
            "\"hello \" name \" world\""
        );
        assert_eq!(
            process_inline("sum = @(+ x y)!"),
            "\"sum = \" (+ x y) \"!\""
        );
        assert_eq!(process_inline("no at signs"), "\"no at signs\"");
    }
}
