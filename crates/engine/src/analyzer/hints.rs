use std::collections::HashSet;

use regex::Regex;

use crate::messages::{Locale, MsgKey, msg};
use crate::parser::lexer::strip_line_comments;
use crate::parser::types::{Symbol, SymbolKind};

use super::{codes, diagnostic::PawnDiagnostic};
use crate::util::to_u32;

static RX_WORD: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\b").unwrap());

pub fn analyze_hints(text: &str, symbols: &[Symbol], locale: Locale) -> Vec<PawnDiagnostic> {
    let mut diags = Vec::new();

    let funcs: Vec<&Symbol> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Public | SymbolKind::Stock | SymbolKind::Static | SymbolKind::Plain
            ) && !s.params.is_empty()
        })
        .collect();

    if funcs.is_empty() {
        return diags;
    }

    let raw_lines: Vec<&str> = text.split('\n').collect();

    for sym in funcs {
        let body_lines = extract_body_lines(&raw_lines, sym.line as usize);
        if body_lines.is_empty() {
            continue;
        }

        let used = collect_idents(&body_lines);

        for param in &sym.params {
            if param.is_variadic || param.name.starts_with('_') || param.name == "..." {
                continue;
            }
            if !used.contains(&param.name) {
                let col = find_param_col(&raw_lines, sym.line as usize, &param.name);
                diags.push(PawnDiagnostic::hint(
                    sym.line,
                    col,
                    col + to_u32(param.name.len()),
                    codes::PP0009,
                    msg(locale, MsgKey::ParamUnused).replace("{}", &param.name),
                ));
            }
        }
    }

    diags
}

fn extract_body_lines<'t>(raw_lines: &[&'t str], decl_line: usize) -> Vec<&'t str> {
    let mut result = Vec::new();
    let mut depth: i32 = 0;
    let mut found_open = false;
    let mut in_block = false;

    for raw in raw_lines.iter().skip(decl_line) {
        let stripped = strip_line_comments(raw.trim_end_matches('\r'), in_block);
        in_block = stripped.in_block;

        for ch in stripped.text.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    found_open = true;
                }
                '}' => {
                    depth = (depth - 1).max(0);
                }
                _ => {}
            }
        }

        result.push(*raw);

        if found_open && depth == 0 {
            break;
        }
    }

    if !found_open {
        result.clear();
    }
    result
}

fn collect_idents(body_lines: &[&str]) -> HashSet<String> {
    let mut idents = HashSet::new();
    let mut in_block = false;

    for (i, raw) in body_lines.iter().enumerate() {
        let stripped = strip_line_comments(raw.trim_end_matches('\r'), in_block);
        in_block = stripped.in_block;

        if i == 0 {
            continue;
        }

        for cap in RX_WORD.captures_iter(&stripped.text) {
            idents.insert(cap[1].to_string());
        }
    }

    idents
}

fn find_param_col(raw_lines: &[&str], decl_line: usize, param_name: &str) -> u32 {
    for raw in raw_lines.iter().skip(decl_line).take(8) {
        if let Some(col) = word_col_in_line(raw, param_name) {
            return col;
        }
        if raw.contains('{') {
            break;
        }
    }
    0
}

fn word_col_in_line(line: &str, word: &str) -> Option<u32> {
    let bytes = line.as_bytes();
    let wbytes = word.as_bytes();
    let wlen = wbytes.len();
    if wlen == 0 || wlen > bytes.len() {
        return None;
    }
    for i in 0..=(bytes.len() - wlen) {
        if &bytes[i..i + wlen] != wbytes {
            continue;
        }
        let is_ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
        let after_ok = i + wlen == bytes.len() || !is_ident_byte(bytes[i + wlen]);
        if before_ok && after_ok {
            return Some(to_u32(i));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;

    fn hints(src: &str) -> Vec<PawnDiagnostic> {
        analyze_hints(src, &parse_file(src).symbols, Locale::default())
    }

    #[test]
    fn an_unused_parameter_is_flagged_on_its_name() {
        let diags = hints("stock Foo(playerid, amount)\n{\n    return amount;\n}\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, codes::PP0009);
        assert_eq!(diags[0].line, 0);
        assert_eq!((diags[0].col_start, diags[0].col_end), (10, 18));
    }

    #[test]
    fn a_used_parameter_is_not_flagged() {
        assert!(hints("stock Foo(a)\n{\n    return a;\n}\n").is_empty());
    }

    #[test]
    fn a_leading_underscore_silences() {
        assert!(hints("stock Foo(_unused)\n{\n    return 0;\n}\n").is_empty());
    }

    #[test]
    fn a_mention_in_a_comment_is_not_a_use() {
        let diags = hints("stock Foo(a)\n{\n    // a\n    return 0;\n}\n");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn a_similar_name_is_not_a_use() {
        // `ab` não é `a`: a busca é por palavra inteira.
        let diags = hints("stock Foo(a)\n{\n    new ab;\n    return ab;\n}\n");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn a_bodyless_declaration_is_not_flagged() {
        assert!(hints("forward Foo(a);\nnative Bar(b);\n").is_empty());
    }
}
