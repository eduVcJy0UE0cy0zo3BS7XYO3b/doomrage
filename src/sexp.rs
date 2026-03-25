/// Minimal S-expression parser for canonical hashing.
/// Parses Scheme code into a tree, normalizes whitespace/comments,
/// and produces a canonical string for content-addressed hashing.

use crate::types::content_hash;

/// S-expression tree node.
#[derive(Debug, Clone, PartialEq)]
pub enum Sexp {
    Atom(String),
    Str(String),
    List(Vec<Sexp>),
    Quote(Box<Sexp>),
}

impl std::fmt::Display for Sexp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", canonical(self))
    }
}

/// Parse a string into a list of top-level S-expressions.
pub fn parse_sexp(code: &str) -> Result<Vec<Sexp>, String> {
    let tokens = tokenize(code)?;
    let mut pos = 0;
    let mut results = Vec::new();
    while pos < tokens.len() {
        let (sexp, next) = parse_expr(&tokens, pos)?;
        results.push(sexp);
        pos = next;
    }
    Ok(results)
}

/// Parse a single sexp from code (convenience wrapper).
pub fn parse_one(code: &str) -> Result<Sexp, String> {
    let sexps = parse_sexp(code)?;
    sexps.into_iter().next().ok_or_else(|| "empty input".to_string())
}

/// Canonical serialization: single spaces, no comments, deterministic.
pub fn canonical(sexp: &Sexp) -> String {
    match sexp {
        Sexp::Atom(a) => a.clone(),
        Sexp::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Sexp::List(items) => {
            let inner: Vec<String> = items.iter().map(canonical).collect();
            format!("({})", inner.join(" "))
        }
        Sexp::Quote(inner) => format!("'{}", canonical(inner)),
    }
}

/// Hash the canonical form of an S-expression.
pub fn canonical_hash(sexp: &Sexp) -> u64 {
    content_hash(&canonical(sexp))
}

/// Hash a code string via canonical sexp normalization.
/// Falls back to raw content_hash if parsing fails.
pub fn canonical_hash_str(code: &str) -> u64 {
    match parse_sexp(code.trim()) {
        Ok(sexps) if sexps.len() == 1 => canonical_hash(&sexps[0]),
        Ok(sexps) if sexps.len() > 1 => {
            // Multiple forms: hash canonical of each, joined
            let c: String = sexps.iter().map(canonical).collect::<Vec<_>>().join(" ");
            content_hash(&c)
        }
        _ => content_hash(code),
    }
}

// --- Structural diff ---

/// A single change between two S-expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffOp {
    /// Unchanged subtree
    Same(String),
    /// Replaced: old → new
    Changed { old: String, new: String },
    /// Added in new
    Added(String),
    /// Removed from old
    Removed(String),
}

/// Structural diff between two S-expressions.
/// Returns a list of DiffOps describing the changes.
pub fn sexp_diff(old: &Sexp, new: &Sexp) -> Vec<DiffOp> {
    if old == new {
        return vec![DiffOp::Same(canonical(old))];
    }
    match (old, new) {
        (Sexp::List(old_items), Sexp::List(new_items)) => {
            diff_lists(old_items, new_items)
        }
        _ => {
            vec![DiffOp::Changed {
                old: canonical(old),
                new: canonical(new),
            }]
        }
    }
}

/// Diff two lists element-by-element (simple LCS-like).
fn diff_lists(old: &[Sexp], new: &[Sexp]) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let mut oi = 0;
    let mut ni = 0;

    while oi < old.len() && ni < new.len() {
        if old[oi] == new[ni] {
            ops.push(DiffOp::Same(canonical(&old[oi])));
            oi += 1;
            ni += 1;
        } else {
            // Look ahead: is old[oi] somewhere in new[ni..]?
            let old_in_new = new[ni..].iter().position(|x| x == &old[oi]);
            // Is new[ni] somewhere in old[oi..]?
            let new_in_old = old[oi..].iter().position(|x| x == &new[ni]);

            match (old_in_new, new_in_old) {
                (Some(0), _) => unreachable!(), // covered by == check
                (_, Some(0)) => unreachable!(),
                (Some(skip), None) | (Some(skip), Some(_)) if skip <= 2 => {
                    // Items were added before old[oi]
                    for j in 0..skip {
                        ops.push(DiffOp::Added(canonical(&new[ni + j])));
                    }
                    ni += skip;
                }
                (None, Some(skip)) if skip <= 2 => {
                    // Items were removed
                    for j in 0..skip {
                        ops.push(DiffOp::Removed(canonical(&old[oi + j])));
                    }
                    oi += skip;
                }
                _ => {
                    // Element changed in place — recurse if both are lists
                    let sub = sexp_diff(&old[oi], &new[ni]);
                    ops.extend(sub);
                    oi += 1;
                    ni += 1;
                }
            }
        }
    }
    // Remaining
    while oi < old.len() {
        ops.push(DiffOp::Removed(canonical(&old[oi])));
        oi += 1;
    }
    while ni < new.len() {
        ops.push(DiffOp::Added(canonical(&new[ni])));
        ni += 1;
    }
    ops
}

/// Format diff ops as a human-readable string.
pub fn format_diff(ops: &[DiffOp]) -> String {
    let mut out = String::new();
    for op in ops {
        match op {
            DiffOp::Same(s) => out.push_str(&format!("  {}\n", s)),
            DiffOp::Changed { old, new } => {
                out.push_str(&format!("- {}\n+ {}\n", old, new));
            }
            DiffOp::Added(s) => out.push_str(&format!("+ {}\n", s)),
            DiffOp::Removed(s) => out.push_str(&format!("- {}\n", s)),
        }
    }
    out
}

/// Diff two definition bodies by their content hash strings.
/// Loads canonical bodies from content-addressed storage, parses, diffs.
pub fn diff_by_hash(hash_a: u64, hash_b: u64) -> Option<String> {
    let body_a = crate::persistence::load_definition(hash_a)?;
    let body_b = crate::persistence::load_definition(hash_b)?;
    let sexp_a = parse_one(&body_a).ok()?;
    let sexp_b = parse_one(&body_b).ok()?;
    let ops = sexp_diff(&sexp_a, &sexp_b);
    Some(format_diff(&ops))
}

// --- Tokenizer ---

#[derive(Debug, Clone, PartialEq)]
enum Token {
    LParen,
    RParen,
    Quote,
    Atom(String),
    Str(String),
}

fn tokenize(code: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Skip whitespace
        if c.is_whitespace() { i += 1; continue; }
        // Line comment
        if c == ';' {
            while i < chars.len() && chars[i] != '\n' { i += 1; }
            continue;
        }
        // Block comment #| ... |#
        if c == '#' && i + 1 < chars.len() && chars[i + 1] == '|' {
            i += 2;
            let mut depth = 1;
            while i + 1 < chars.len() && depth > 0 {
                if chars[i] == '#' && chars[i + 1] == '|' { depth += 1; i += 2; }
                else if chars[i] == '|' && chars[i + 1] == '#' { depth -= 1; i += 2; }
                else { i += 1; }
            }
            continue;
        }
        match c {
            '(' => { tokens.push(Token::LParen); i += 1; }
            ')' => { tokens.push(Token::RParen); i += 1; }
            '\'' => { tokens.push(Token::Quote); i += 1; }
            '"' => {
                // String literal
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        match chars[i] {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            '\\' => s.push('\\'),
                            '"' => s.push('"'),
                            other => { s.push('\\'); s.push(other); }
                        }
                    } else {
                        s.push(chars[i]);
                    }
                    i += 1;
                }
                if i < chars.len() { i += 1; } // skip closing "
                tokens.push(Token::Str(s));
            }
            _ => {
                // Atom: symbol, number, boolean, etc.
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace()
                    && chars[i] != '(' && chars[i] != ')' && chars[i] != '"' && chars[i] != ';'
                {
                    i += 1;
                }
                let atom = chars[start..i].iter().collect::<String>();
                tokens.push(Token::Atom(atom));
            }
        }
    }
    Ok(tokens)
}

// --- Parser ---

fn parse_expr(tokens: &[Token], pos: usize) -> Result<(Sexp, usize), String> {
    if pos >= tokens.len() {
        return Err("unexpected end of input".to_string());
    }
    match &tokens[pos] {
        Token::LParen => {
            let mut items = Vec::new();
            let mut i = pos + 1;
            while i < tokens.len() {
                if tokens[i] == Token::RParen {
                    return Ok((Sexp::List(items), i + 1));
                }
                let (expr, next) = parse_expr(tokens, i)?;
                items.push(expr);
                i = next;
            }
            Err("unclosed parenthesis".to_string())
        }
        Token::RParen => Err("unexpected ')'".to_string()),
        Token::Quote => {
            let (inner, next) = parse_expr(tokens, pos + 1)?;
            Ok((Sexp::Quote(Box::new(inner)), next))
        }
        Token::Atom(a) => Ok((Sexp::Atom(a.clone()), pos + 1)),
        Token::Str(s) => Ok((Sexp::Str(s.clone()), pos + 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_atom() {
        assert_eq!(parse_one("foo").unwrap(), Sexp::Atom("foo".into()));
    }

    #[test]
    fn test_parse_number() {
        assert_eq!(parse_one("42").unwrap(), Sexp::Atom("42".into()));
    }

    #[test]
    fn test_parse_string() {
        assert_eq!(parse_one("\"hello\"").unwrap(), Sexp::Str("hello".into()));
    }

    #[test]
    fn test_parse_list() {
        let sexp = parse_one("(+ 1 2)").unwrap();
        assert_eq!(sexp, Sexp::List(vec![
            Sexp::Atom("+".into()),
            Sexp::Atom("1".into()),
            Sexp::Atom("2".into()),
        ]));
    }

    #[test]
    fn test_parse_nested() {
        let sexp = parse_one("(define (f x) (* x x))").unwrap();
        assert_eq!(canonical(&sexp), "(define (f x) (* x x))");
    }

    #[test]
    fn test_parse_quote() {
        let sexp = parse_one("'foo").unwrap();
        assert_eq!(sexp, Sexp::Quote(Box::new(Sexp::Atom("foo".into()))));
        assert_eq!(canonical(&sexp), "'foo");
    }

    #[test]
    fn test_canonical_normalizes_whitespace() {
        let a = parse_one("(+  1   2)").unwrap();
        let b = parse_one("(+ 1 2)").unwrap();
        assert_eq!(canonical(&a), canonical(&b));
        assert_eq!(canonical(&a), "(+ 1 2)");
    }

    #[test]
    fn test_canonical_strips_comments() {
        let code = "(define x ; this is a comment\n  42)";
        let sexp = parse_one(code).unwrap();
        assert_eq!(canonical(&sexp), "(define x 42)");
    }

    #[test]
    fn test_canonical_hash_whitespace_invariant() {
        let h1 = canonical_hash_str("(+ 1  2)");
        let h2 = canonical_hash_str("(+ 1 2)");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_canonical_hash_comment_invariant() {
        let h1 = canonical_hash_str("(define x ; comment\n  42)");
        let h2 = canonical_hash_str("(define x 42)");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_canonical_hash_different_code() {
        let h1 = canonical_hash_str("(+ 1 2)");
        let h2 = canonical_hash_str("(+ 1 3)");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_parse_block_comment() {
        let code = "(define x #| block comment |# 42)";
        let sexp = parse_one(code).unwrap();
        assert_eq!(canonical(&sexp), "(define x 42)");
    }

    #[test]
    fn test_parse_string_with_escapes() {
        let sexp = parse_one("\"hello\\nworld\"").unwrap();
        assert_eq!(sexp, Sexp::Str("hello\nworld".into()));
    }

    #[test]
    fn test_parse_empty_list() {
        let sexp = parse_one("()").unwrap();
        assert_eq!(sexp, Sexp::List(vec![]));
    }

    #[test]
    fn test_parse_multiple_toplevel() {
        let sexps = parse_sexp("(define a 1) (define b 2)").unwrap();
        assert_eq!(sexps.len(), 2);
    }

    #[test]
    fn test_boolean_atoms() {
        let sexp = parse_one("#t").unwrap();
        assert_eq!(sexp, Sexp::Atom("#t".into()));
    }

    #[test]
    fn test_real_scheme_code() {
        let code = r#"(define (wave-points i acc)
  (if (>= i 300.0)
      acc
      (wave-points (+ i 1.0)
                   (cons (list i (* 100.0 (sin (* i 0.05)))) acc))))"#;
        let sexp = parse_one(code).unwrap();
        let c = canonical(&sexp);
        assert!(c.starts_with("(define (wave-points i acc)"));
        // Re-parse canonical form should be identical
        let sexp2 = parse_one(&c).unwrap();
        assert_eq!(canonical(&sexp), canonical(&sexp2));
    }

    // --- Diff tests ---

    #[test]
    fn test_diff_identical() {
        let a = parse_one("(+ 1 2)").unwrap();
        let ops = sexp_diff(&a, &a);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::Same(s) if s == "(+ 1 2)"));
    }

    #[test]
    fn test_diff_atom_change() {
        let a = parse_one("42").unwrap();
        let b = parse_one("99").unwrap();
        let ops = sexp_diff(&a, &b);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::Changed { old, new } if old == "42" && new == "99"));
    }

    #[test]
    fn test_diff_list_element_changed() {
        let a = parse_one("(+ 1 2)").unwrap();
        let b = parse_one("(+ 1 3)").unwrap();
        let ops = sexp_diff(&a, &b);
        // + and 1 are same, 2→3 changed
        assert!(ops.iter().any(|op| matches!(op, DiffOp::Same(s) if s == "+")));
        assert!(ops.iter().any(|op| matches!(op, DiffOp::Changed { old, new } if old == "2" && new == "3")));
    }

    #[test]
    fn test_diff_element_added() {
        let a = parse_one("(+ 1)").unwrap();
        let b = parse_one("(+ 1 2)").unwrap();
        let ops = sexp_diff(&a, &b);
        assert!(ops.iter().any(|op| matches!(op, DiffOp::Added(s) if s == "2")));
    }

    #[test]
    fn test_diff_element_removed() {
        let a = parse_one("(+ 1 2)").unwrap();
        let b = parse_one("(+ 1)").unwrap();
        let ops = sexp_diff(&a, &b);
        assert!(ops.iter().any(|op| matches!(op, DiffOp::Removed(s) if s == "2")));
    }

    #[test]
    fn test_diff_nested_change() {
        let a = parse_one("(define (f x) (* x 2))").unwrap();
        let b = parse_one("(define (f x) (* x 3))").unwrap();
        let ops = sexp_diff(&a, &b);
        let text = format_diff(&ops);
        assert!(text.contains("- 2"));
        assert!(text.contains("+ 3"));
        assert!(text.contains("  define"));
    }

    #[test]
    fn test_format_diff() {
        let a = parse_one("(+ 1 2)").unwrap();
        let b = parse_one("(+ 1 3)").unwrap();
        let ops = sexp_diff(&a, &b);
        let text = format_diff(&ops);
        assert!(text.contains("  +\n"));   // operator unchanged
        assert!(text.contains("  1\n"));   // first arg unchanged
        assert!(text.contains("- 2\n"));   // old
        assert!(text.contains("+ 3\n"));   // new
    }
}
