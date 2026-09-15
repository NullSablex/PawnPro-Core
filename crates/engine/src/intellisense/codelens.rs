use serde_json::json;
use tower_lsp::lsp_types::{CodeLens, Command, Position, Range};

use crate::messages::{MsgKey, msg};
use crate::parser::types::SymbolKind;
use crate::util::to_u32;
use crate::workspace::WorkspaceState;

pub fn get_code_lens(state: &WorkspaceState, uri: &str) -> Vec<CodeLens> {
    let locale = state.locale;
    let Some(parsed) = state.get_parsed(uri) else {
        return vec![];
    };

    let func_syms: Vec<_> = parsed
        .symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Native
                    | SymbolKind::Public
                    | SymbolKind::Stock
                    | SymbolKind::Static
                    | SymbolKind::Plain
            )
        })
        .collect();

    if func_syms.is_empty() {
        return vec![];
    }

    let Some(file) = crate::workspace::uri_to_path(uri) else {
        return vec![];
    };
    // Conta no que é compilado junto, como a lista de referências que o
    // contador abre, direto do cache de identificadores: nenhum texto é relido
    // a cada pedido. A declaração também aparece escrita como `Nome(`, e é ela
    // que o `- 1` desconta.
    let open = state.open_paths();
    let unit: Vec<_> = state
        .unit_files(&file, &open)
        .iter()
        .filter_map(|path| state.idents_of(path, &open))
        .collect();

    func_syms
        .iter()
        .map(|sym| {
            // Uma `public` também é chamada pelo nome em texto — `SetTimer`.
            let public = matches!(sym.kind, SymbolKind::Public);
            let total: usize = unit
                .iter()
                .map(|f| {
                    f.call_count(&sym.name) + if public { f.quote_count(&sym.name) } else { 0 }
                })
                .sum();

            let refs = total.saturating_sub(1);

            let title = match refs {
                0 => msg(locale, MsgKey::RefsZero).to_string(),
                1 => msg(locale, MsgKey::RefsOne).to_string(),
                n => msg(locale, MsgKey::RefsMany).replace("{n}", &n.to_string()),
            };

            let range = Range {
                start: Position {
                    line: sym.line,
                    character: sym.col,
                },
                end: Position {
                    line: sym.line,
                    character: sym.col + to_u32(sym.name.len()),
                },
            };

            let command = if refs > 0 {
                Some(Command {
                    title,
                    command: "pawnpro.findReferences".to_string(),
                    arguments: Some(vec![json!(uri), json!(sym.line), json!(sym.col)]),
                })
            } else {
                Some(Command {
                    title,
                    command: String::new(),
                    arguments: None,
                })
            };

            CodeLens {
                range,
                command,
                data: None,
            }
        })
        .collect()
}
