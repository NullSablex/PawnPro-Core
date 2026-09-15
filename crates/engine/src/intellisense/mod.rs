mod codelens;
mod completion;
mod definition;
mod docs;
mod format_engine;
mod format_indent;
mod format_style;
mod formatter;
mod hover;
mod quickfix;
mod references;
mod rename;
mod semantic_tokens;
mod signature;

pub use codelens::get_code_lens;
pub use completion::{MAX_COMPLETION_ITEMS, get_at_completions, get_completions};
pub use definition::get_definition;
pub use docs::{DocLabels, parse_doc};
pub use format_style::{BracePlacement, FormatStyle, Preset};
pub use formatter::{format_document, format_range};
pub use hover::get_hover;
pub use quickfix::{RemovalKind, removal_kind, removal_range};
pub use references::get_references;
pub use rename::{get_rename, prepare_rename};
pub use semantic_tokens::{get_semantic_tokens, semantic_tokens_legend};
pub use signature::get_signature_help;

use std::path::{Path, PathBuf};

use crate::analyzer::includes::collect_included_files_with;
use crate::parser::ParsedFile;
use crate::parser::types::Symbol;
use crate::workspace::WorkspaceState;

pub(crate) fn extract_word(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';

    let mut start = col.min(chars.len());
    if start == chars.len() || !is_ident(chars[start]) {
        if start == 0 {
            return None;
        }
        start -= 1;
        if !is_ident(chars[start]) {
            return None;
        }
    }
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let mut end = start;
    while end < chars.len() && is_ident(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// O símbolo conhecido mais parecido com `name`, para sugerir em PP0010.
///
/// Considera os símbolos do arquivo e de todos os includes transitivos — é o
/// mesmo universo que o autocomplete oferece, então a sugestão nunca aponta
/// para algo que o arquivo não enxerga.
pub fn suggest_symbol(
    state: &WorkspaceState,
    file_path: &Path,
    inc_paths: &[PathBuf],
    parsed: &ParsedFile,
    name: &str,
) -> Option<String> {
    let all = collect_all_symbols(state, file_path, inc_paths, parsed);
    let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
    crate::similar::closest(name, names).map(str::to_string)
}

/// O arquivo de onde parte a busca por um símbolo.
pub(crate) struct Origin<'a> {
    pub uri: &'a str,
    pub path: &'a Path,
    pub text: &'a str,
    pub parsed: &'a ParsedFile,
}

/// Um símbolo achado, com a URI e o texto do arquivo que o declara: a coluna
/// do símbolo é em bytes, e a posição LSP sai da linha.
pub(crate) struct Located {
    pub uri: String,
    pub text: String,
    pub symbol: Symbol,
}

/// O símbolo `name` aceito por `accept`, como `origin` o enxerga: no próprio
/// arquivo, nos includes e, por fim, noutro arquivo compilado junto com ele —
/// o `.inc` irmão, incluído pelo mesmo `.pwn`. Um programa à parte nunca
/// entra, mesmo com uma função de mesmo nome. Na unidade, só são lidos os
/// arquivos que o cache aponta como declarando o nome.
pub(crate) fn locate_symbol(
    state: &WorkspaceState,
    origin: &Origin<'_>,
    name: &str,
    accept: impl Fn(&Symbol) -> bool,
) -> Option<Located> {
    let wanted = |s: &Symbol| s.name == name && accept(s);
    if let Some(sym) = origin.parsed.symbols.iter().find(|s| wanted(s)) {
        return Some(Located {
            uri: origin.uri.to_string(),
            text: origin.text.to_string(),
            symbol: sym.clone(),
        });
    }
    let open = state.open_paths();
    let resolved = collect_included_files_with(
        origin.path,
        &state.include_paths,
        &origin.parsed.includes,
        16,
        1000,
        &|path| state.text_at(&open, path),
    );
    for path in &resolved.paths {
        let Some(entry) = resolved.files.get(path) else {
            continue;
        };
        if let Some(sym) = entry.parsed.symbols.iter().find(|s| wanted(s)) {
            return Some(Located {
                uri: crate::workspace::uri_for(&open, path)?,
                text: entry.text.clone(),
                symbol: sym.clone(),
            });
        }
    }
    // O resto da unidade, pelos símbolos do cache: só o arquivo que declara o
    // nome tem o texto lido, para a posição.
    let mut unit: Vec<_> = state.unit_files(origin.path, &open).into_iter().collect();
    unit.sort();
    unit.iter().find_map(|path| {
        let idents = state.idents_of(path, &open)?;
        let symbol = idents.symbols().iter().find(|s| wanted(s))?.clone();
        Some(Located {
            uri: crate::workspace::uri_for(&open, path)?,
            text: state.text_at(&open, path)?,
            symbol,
        })
    })
}

pub(crate) fn collect_all_symbols(
    state: &WorkspaceState,
    file_path: &Path,
    inc_paths: &[PathBuf],
    parsed: &ParsedFile,
) -> Vec<Symbol> {
    let mut all = parsed.symbols.clone();
    let open = state.open_paths();
    let resolved =
        collect_included_files_with(file_path, inc_paths, &parsed.includes, 16, 1000, &|path| {
            state.text_at(&open, path)
        });
    for inc_path in &resolved.paths {
        if let Some(entry) = resolved.files.get(inc_path) {
            all.extend(entry.parsed.symbols.clone());
        } else if let Some(inc_parsed) = state.get_parsed_by_path(inc_path) {
            all.extend(inc_parsed.symbols.clone());
        }
    }
    all
}
