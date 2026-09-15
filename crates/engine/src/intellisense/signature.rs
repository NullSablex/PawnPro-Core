use tower_lsp::lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, Position,
    SignatureHelp, SignatureInformation,
};

use crate::workspace::WorkspaceState;

use super::hover::doc_labels;
use super::{Origin, locate_symbol, parse_doc};
use crate::util::to_u32;

pub fn get_signature_help(
    state: &WorkspaceState,
    uri: &str,
    position: Position,
) -> Option<SignatureHelp> {
    let text = state.get_text(uri)?;
    let file_path = crate::workspace::uri_to_path(uri)?;
    let parsed = state.get_parsed(uri)?;

    let lines: Vec<&str> = text.lines().collect();

    let line_idx = position.line as usize;
    if line_idx >= lines.len() {
        return None;
    }
    let line = lines[line_idx];

    // A posição do cliente conta caracteres, não bytes: cortar por byte cairia
    // no meio de uma letra acentuada numa string ou num comentário.
    let prefix: String = line.chars().take(position.character as usize).collect();

    let (func_name, active_param) = find_call_context(&prefix)?;

    let origin = Origin {
        uri,
        path: &file_path,
        text: &text,
        parsed: &parsed,
    };
    let sym = locate_symbol(state, &origin, &func_name, |s| s.signature.is_some())?.symbol;

    let doc = sym.doc.as_deref().map(parse_doc);
    let labels = doc_labels(state.locale);

    let param_infos: Vec<ParameterInformation> = sym
        .params
        .iter()
        .map(|p| {
            let label = if let Some(tag) = &p.tag {
                format!("{}:{}", tag, p.name)
            } else {
                p.name.clone()
            };
            // A doc do parâmetro casa pelo nome, não pela posição: um
            // comentário pode omitir parâmetros ou listá-los fora de ordem.
            let documentation = doc
                .as_ref()
                .and_then(|d| d.param(&p.name))
                .filter(|t| !t.is_empty())
                .map(|t| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: t.to_string(),
                    })
                });
            ParameterInformation {
                label: ParameterLabel::Simple(label),
                documentation,
            }
        })
        .collect();

    let active_idx = to_u32(active_param).min(to_u32(param_infos.len().saturating_sub(1)));

    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: sym.signature.clone().unwrap_or_default(),
            documentation: doc.as_ref().and_then(|d| d.to_markdown(&labels)).map(|v| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: v,
                })
            }),
            parameters: Some(param_infos),
            active_parameter: Some(active_idx),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_idx),
    })
}

fn find_call_context(prefix: &str) -> Option<(String, usize)> {
    let chars: Vec<char> = prefix.chars().collect();
    let mut depth = 0i32;
    let mut active_param = 0usize;
    let mut in_str = false;
    let mut in_char = false;

    // Caminha de trás para frente com índice `usize` (via `checked_sub`),
    // evitando `isize` e seus casts.
    let mut i = chars.len().checked_sub(1);
    while let Some(idx) = i {
        let ch = chars[idx];
        match ch {
            '"' if !in_char => in_str = !in_str,
            '\'' if !in_str => in_char = !in_char,
            _ if in_str || in_char => {}
            ')' | ']' => depth += 1,
            '[' if depth > 0 => {
                depth -= 1;
            }
            '(' => {
                if depth == 0 {
                    let name = name_before(&chars, idx)?;
                    return Some((name, active_param));
                }
                depth -= 1;
            }
            ',' if depth == 0 => active_param += 1,
            _ => {}
        }
        i = idx.checked_sub(1);
    }
    None
}

fn name_before(chars: &[char], paren_pos: usize) -> Option<String> {
    if paren_pos == 0 {
        return None;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut end = paren_pos;
    while end > 0 && chars[end - 1] == ' ' {
        end -= 1;
    }
    if end == 0 || !is_ident(chars[end - 1]) {
        return None;
    }
    let mut start = end;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}
