use std::ops::RangeInclusive;
use std::path::Path;

use tower_lsp::lsp_types::{Location, Position, Range, Url};

use super::{Origin, locate_symbol};
use crate::parser::lexer::{mask_string_literals, strip_line_comments, update_brace_depth};
use crate::parser::types::SymbolKind;
use crate::text::utf16_col;
use crate::util::to_u32;
use crate::workspace::WorkspaceState;

/// Até onde vale um nome, pelo lugar onde ele é declarado.
pub(super) enum Scope {
    /// Local ou parâmetro — sem declaração fora de funções na unidade: vale nas
    /// linhas `first..=last` do arquivo atual. Um local, do `new` que o declara
    /// até o fim do bloco dele; um parâmetro, no corpo da função.
    Local { first: usize, last: usize },
    /// Declarado fora de funções: vale na unidade de compilação inteira. Uma
    /// `public` também é chamada pelo nome em texto — `SetTimer("Nome", ...)`.
    Unit { public: bool },
}

pub fn get_references(state: &WorkspaceState, uri: &str, pos: Position) -> Vec<Location> {
    let Some(text) = state.open_docs.get(uri).map(|doc| doc.text.clone()) else {
        return vec![];
    };
    let Some(word) = word_at(&text, pos.line, pos.character) else {
        return vec![];
    };
    let Some(file) = crate::workspace::uri_to_path(uri) else {
        return vec![];
    };
    let is_callable = resolve_callable(state, uri, &text, &word, pos);

    match scope_of(state, uri, &file, &text, &word, pos.line) {
        Some(Scope::Local { first, last }) => {
            occurrences(uri, &text, &word, false, false, first..=last)
        }
        // Tudo o que é compilado junto, aberto ou não: uma chamada num arquivo
        // fechado também é referência; uma num programa à parte, não.
        Some(Scope::Unit { public }) => state
            .unit_texts(&file, |f| {
                f.mentions_ident(&word) || (public && f.quote_count(&word) > 0)
            })
            .iter()
            .flat_map(|(doc_uri, doc_text)| {
                occurrences(
                    doc_uri,
                    doc_text,
                    &word,
                    is_callable,
                    public,
                    0..=usize::MAX,
                )
            })
            .collect(),
        None => vec![],
    }
}

/// O alcance de `word` visto de `uri`: global se há declaração dele fora de
/// funções na unidade; senão, o corpo da função em volta da linha `line`.
pub(super) fn scope_of(
    state: &WorkspaceState,
    uri: &str,
    file: &Path,
    text: &str,
    word: &str,
    line: u32,
) -> Option<Scope> {
    let parsed = state.get_parsed(uri)?;
    let origin = Origin {
        uri,
        path: file,
        text,
        parsed: &parsed,
    };
    if locate_symbol(state, &origin, word, |_| true).is_some() {
        let public = locate_symbol(state, &origin, word, |s| {
            matches!(s.kind, SymbolKind::Public)
        })
        .is_some();
        return Some(Scope::Unit { public });
    }
    let (body_first, body_last) = enclosing_body(text, line as usize)?;
    let (first, last) = local_scope(text, word, body_first, body_last, line as usize);
    Some(Scope::Local { first, last })
}

/// As linhas onde vale o local `word` visto da linha `line`, na função
/// `body_first..=body_last`: da declaração mais próxima que ainda vale ali até
/// o fim do bloco dela — num `for (new i ...)`, até o fim do próprio `for`.
/// Sem declaração visível, é um parâmetro, e vale a função inteira.
fn local_scope(
    text: &str,
    word: &str,
    body_first: usize,
    body_last: usize,
    line: usize,
) -> (usize, usize) {
    // A profundidade antes e depois de cada linha da função, e o código dela.
    let mut rows: Vec<(i32, i32, String)> = Vec::new();
    let mut depth = 0;
    let mut in_block = false;
    for (i, raw) in text.lines().enumerate().take(body_last + 1) {
        let stripped = strip_line_comments(raw.trim_end_matches('\r'), in_block);
        in_block = stripped.in_block;
        let code = mask_string_literals(&stripped.text);
        let before = depth;
        depth = update_brace_depth(&code, depth);
        if i >= body_first {
            rows.push((before, depth, code));
        }
    }
    // A primeira linha, a partir da linha `from` da função, em que a
    // profundidade cai abaixo de `level`: a da `}` que fecha o bloco.
    let closes = |from: usize, level: i32| {
        rows.iter()
            .enumerate()
            .skip(from)
            .find(|(_, (_, after, _))| *after < level)
            .map_or(body_last, |(k, _)| body_first + k)
    };

    let mut best = None;
    for (k, (before, after, code)) in rows.iter().enumerate() {
        let decl = body_first + k;
        if decl > line || !declared_names(code).iter().any(|n| n == word) {
            continue;
        }
        let end = if code.trim_start().starts_with("for") {
            if after > before {
                closes(k, *after)
            } else if code.contains('{') {
                // Corpo na mesma linha.
                decl
            } else if let Some((_, next_after, next)) = rows.get(k + 1)
                && next.trim_start().starts_with('{')
            {
                closes(k + 1, *next_after)
            } else {
                // Um só comando, na linha seguinte.
                (decl + 1).min(body_last)
            }
        } else {
            closes(k, *before)
        };
        // A declaração mais adiante que ainda vale na linha é a que manda: um
        // bloco interno pode declarar de novo o mesmo nome.
        if line <= end {
            best = Some((decl, end));
        }
    }
    best.unwrap_or((body_first, body_last))
}

/// O nome que um item de uma lista de declaração declara — `b[3]`, `Tag:c`,
/// `const d = 1` —, se tiver um.
fn item_name(item: &str) -> Option<String> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut rest = item.trim_start();
    if let Some(after) = rest.strip_prefix("const")
        && after.starts_with(char::is_whitespace)
    {
        rest = after.trim_start();
    }
    let end = rest.find(|c| !is_ident(c)).unwrap_or(rest.len());
    let (first, after) = rest.split_at(end);
    // Uma tag — `Float:x` — é o `:` simples, não o `::` de namespace.
    let name = if after.starts_with(':') && !after.starts_with("::") {
        let tagged = after[1..].trim_start();
        &tagged[..tagged.find(|c| !is_ident(c)).unwrap_or(tagged.len())]
    } else {
        first
    };
    let starts_well = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    starts_well.then(|| name.to_string())
}

/// Os nomes que uma linha declara com `new` ou `static`, um por item da lista:
/// `new a = 1, b[3], Float:c;` declara `a`, `b` e `c`. A lista vai até o `;`,
/// ou até o `)` que fecha um `for (new i ...)`. A linha já vem sem comentários
/// e sem o conteúdo das strings.
pub(super) fn declared_names(code: &str) -> Vec<String> {
    let bytes = code.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut names = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !is_ident(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_ident(bytes[i]) {
            i += 1;
        }
        if !matches!(&code[start..i], "new" | "static") {
            continue;
        }
        let mut depth = 0;
        let mut item_start = i;
        let mut j = i;
        while j < bytes.len() {
            match bytes[j] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => {
                    names.extend(item_name(&code[item_start..j]));
                    item_start = j + 1;
                }
                b';' if depth == 0 => break,
                _ => {}
            }
            j += 1;
        }
        names.extend(item_name(&code[item_start..j]));
        i = j;
    }
    names
}

/// As linhas `(primeira, última)` da função que contém `line`: do cabeçalho,
/// onde estão os parâmetros, até a `}` que a fecha.
pub(super) fn enclosing_body(text: &str, line: usize) -> Option<(usize, usize)> {
    let mut depth = 0;
    let mut in_block = false;
    let mut header = 0;
    let mut start = None;
    for (i, raw) in text.lines().enumerate() {
        let stripped = strip_line_comments(raw.trim_end_matches('\r'), in_block);
        in_block = stripped.in_block;
        let code = mask_string_literals(&stripped.text);
        let before = depth;
        let trimmed = code.trim();
        if before == 0 && !trimmed.is_empty() && !trimmed.starts_with('{') {
            header = i;
        }
        depth = update_brace_depth(&code, depth);
        if before == 0 && depth > 0 {
            start = Some(header);
        } else if before == 0 && code.contains('{') {
            // Corpo aberto e fechado na mesma linha.
            if header <= line && line <= i {
                return Some((header, i));
            }
        } else if before > 0
            && depth == 0
            // O `take` roda sempre que um bloco fecha, contenha ele a linha ou não.
            && let Some(first) = start.take()
            && (first..=i).contains(&line)
        {
            return Some((first, i));
        }
        if i > line && depth == 0 && start.is_none() {
            break;
        }
    }
    None
}

/// As ocorrências de `word` nas linhas `lines` de `text`, fora de comentário
/// e string. Com `callable`, só as seguidas de `(`; com `quoted`, também as
/// strings que são exatamente o nome.
fn occurrences(
    uri: &str,
    text: &str,
    word: &str,
    callable: bool,
    quoted: bool,
    lines: RangeInclusive<usize>,
) -> Vec<Location> {
    let Ok(loc_uri) = uri.parse::<Url>() else {
        return vec![];
    };
    let wb = word.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let needle = format!("\"{word}\"");
    let mut out = Vec::new();
    let mut in_block = false;
    for (line_idx, raw_line) in text.lines().enumerate() {
        let raw_line = raw_line.trim_end_matches('\r');
        let stripped = strip_line_comments(raw_line, in_block);
        in_block = stripped.in_block;
        if !lines.contains(&line_idx) {
            continue;
        }
        // A coluna LSP conta UTF-16, não bytes: um acento antes do nome
        // deslocaria a marcação.
        let mut push = |col: usize| {
            out.push(Location {
                uri: loc_uri.clone(),
                range: Range {
                    start: Position {
                        line: to_u32(line_idx),
                        character: utf16_col(raw_line, col),
                    },
                    end: Position {
                        line: to_u32(line_idx),
                        character: utf16_col(raw_line, col + wb.len()),
                    },
                },
            });
        };

        // Sem o conteúdo das strings: `"Nome("` num texto não é referência.
        let line = mask_string_literals(&stripped.text);
        let bytes = line.as_bytes();
        let mut col = 0usize;
        while col + wb.len() <= bytes.len() {
            let end = col + wb.len();
            if &bytes[col..end] == wb
                && (col == 0 || !is_ident(bytes[col - 1]))
                && (end >= bytes.len() || !is_ident(bytes[end]))
                && (!callable || {
                    let mut j = end;
                    while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
                        j += 1;
                    }
                    bytes.get(j) == Some(&b'(')
                })
            {
                push(col);
            }
            col += 1;
        }
        if quoted {
            for (at, _) in stripped.text.match_indices(&needle) {
                push(at + 1);
            }
        }
    }
    out
}

pub(super) fn resolve_callable(
    state: &WorkspaceState,
    uri: &str,
    text: &str,
    name: &str,
    pos: Position,
) -> bool {
    if let Some(parsed) = state.get_parsed(uri) {
        for sym in &parsed.symbols {
            if sym.name == name && sym.line == pos.line {
                return is_func_kind(&sym.kind);
            }
        }
    }

    if let Some(line_str) = text.lines().nth(pos.line as usize) {
        let bytes = line_str.as_bytes();
        let col = pos.character as usize;
        let mut end = col;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        let mut j = end;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'(' {
            return true;
        }
    }

    let mut found_as_func = false;
    let mut found_as_non_func = false;

    for entry in &state.open_docs {
        if let Some(parsed) = state.get_parsed(entry.key().as_str()) {
            for sym in &parsed.symbols {
                if sym.name == name {
                    if is_func_kind(&sym.kind) {
                        found_as_func = true;
                    } else {
                        found_as_non_func = true;
                    }
                }
            }
        }
    }

    found_as_func && !found_as_non_func
}

#[inline]
const fn is_func_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Native
            | SymbolKind::Public
            | SymbolKind::Stock
            | SymbolKind::Static
            | SymbolKind::Forward
            | SymbolKind::Plain
    )
}

fn word_at(text: &str, line: u32, col: u32) -> Option<String> {
    crate::text::word_at(
        text,
        Position {
            line,
            character: col,
        },
    )
}
