use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use regex::Regex;

use crate::messages::{Locale, MsgKey, msg};
use crate::parser::lexer::{mask_string_literals, strip_line_comments};
use crate::parser::{ParsedFile, SymbolKind};

use super::includes::ResolvedIncludes;
use super::{codes, diagnostic::PawnDiagnostic};
use crate::util::to_u32;

static RX_CALL: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap());
static RX_IDENT: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\b").unwrap());

#[derive(Clone, Copy)]
enum CollectMode {
    Calls,
    AllIdents,
    IdentsNoDefineLines,
}

/// O que as referências e o contador consultam, sem os recortes do `unused`,
/// que pulam linhas inteiras.
type Words = (
    HashSet<String>,
    HashMap<String, usize>,
    HashMap<String, usize>,
);

/// Se `s` tem forma de identificador Pawn.
fn is_identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Conta as strings de `line` que são inteiras um identificador — `"Nome"`.
fn count_quoted(line: &str, quoted: &mut HashMap<String, usize>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && bytes[j] != quote {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        if quote == b'"'
            && j < bytes.len()
            && let Some(content) = line.get(start..j)
            && is_identifier(content)
        {
            *quoted.entry(content.to_string()).or_insert(0) += 1;
        }
        i = j + 1;
    }
}

/// Todo identificador fora de comentário e string, quantas vezes cada um
/// aparece chamado — seguido de `(` — e quantas aparece sozinho numa string.
fn scan_words(text: &str) -> Words {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut words = HashSet::new();
    let mut calls: HashMap<String, usize> = HashMap::new();
    let mut quoted: HashMap<String, usize> = HashMap::new();
    let mut in_block = false;
    for raw in text.split('\n') {
        let stripped = strip_line_comments(raw.trim_end_matches('\r'), in_block);
        in_block = stripped.in_block;
        count_quoted(&stripped.text, &mut quoted);
        let line = mask_string_literals(&stripped.text);
        let bytes = line.as_bytes();
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
            // Um número não é identificador, nem com letras no meio (`0x1F`).
            if bytes[start].is_ascii_digit() {
                continue;
            }
            let word = &line[start..i];
            let mut j = i;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'(') {
                *calls.entry(word.to_string()).or_insert(0) += 1;
            }
            words.insert(word.to_string());
        }
    }
    (words, calls, quoted)
}

fn collect_idents(text: &str, mode: CollectMode) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut in_block = false;

    for raw_line in text.split('\n') {
        let raw = raw_line.trim_end_matches('\r');
        let stripped = strip_line_comments(raw, in_block);
        in_block = stripped.in_block;

        let trimmed = stripped.text.trim_start();
        let trimmed_lower = trimmed.to_ascii_lowercase();

        if trimmed_lower.starts_with("#define") || trimmed_lower.starts_with("# define") {
            continue;
        }

        let kw = |k: &str| {
            trimmed_lower.starts_with(&format!("{k} "))
                || trimmed_lower.starts_with(&format!("{k}\t"))
        };
        let skip = match mode {
            CollectMode::Calls => {
                kw("stock") || kw("public") || kw("static") || kw("native") || kw("forward")
            }
            CollectMode::AllIdents => {
                kw("new") || kw("const") || (kw("static") && !stripped.text.contains('('))
            }
            CollectMode::IdentsNoDefineLines => trimmed.starts_with('#'),
        };

        if skip {
            continue;
        }

        let rx = match mode {
            CollectMode::Calls => &*RX_CALL,
            CollectMode::AllIdents | CollectMode::IdentsNoDefineLines => &*RX_IDENT,
        };

        for cap in rx.captures_iter(&stripped.text) {
            out.insert(cap[1].to_string());
        }
    }

    out
}

/// Todo identificador que o texto menciona, fora de comentários.
///
/// Sem pular linha nenhuma, ao contrário do `collect_idents`: para saber se um
/// include é usado, `new Float:x = floatsqroot(2.0);` conta — a declaração usa
/// o `float.inc`. Uma menção dentro de string também conta; isso só pode calar
/// o aviso de include sem uso, nunca inventar um.
fn mentions(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut in_block = false;
    for raw_line in text.split('\n') {
        let stripped = strip_line_comments(raw_line.trim_end_matches('\r'), in_block);
        in_block = stripped.in_block;
        for cap in RX_IDENT.captures_iter(&stripped.text) {
            out.insert(cap[1].to_string());
        }
    }
    out
}

// Sequência de fases: coleta de identificadores (local + includes + workspace)
// seguida de uma verificação de "não usado" por SymbolKind (Variable, Stock,
// Native, Forward, Plain), cada uma com seu código de diagnóstico. As fases são
// lineares e independentes; separá-las exigiria devolver/passar vários conjuntos
// de identificadores, sem ganho real de clareza.
#[allow(clippy::too_many_lines)]
pub fn analyze_unused(
    text: &str,
    file_path: &Path,
    parsed: &ParsedFile,
    resolved: &ResolvedIncludes,
    warn_unused_in_inc: bool,
    others: &[Arc<FileIdents>],
    locale: Locale,
) -> Vec<PawnDiagnostic> {
    let mut diags = Vec::new();
    let is_inc = is_include_file(file_path);

    let local_calls = collect_idents(text, CollectMode::Calls);
    let local_idents = collect_idents(text, CollectMode::AllIdents);

    if is_inc && !warn_unused_in_inc {
        return diags;
    }

    let local_no_directives = collect_idents(text, CollectMode::IdentsNoDefineLines);
    // Os outros arquivos — includes resolvidos e o resto do projeto — entram por
    // consulta, não por cópia: reprocessar o texto de cada include ou juntar os
    // identificadores de todos num conjunto novo a cada análise custaria o que
    // o cache economizou.
    let used_as_call =
        |name: &str| local_calls.contains(name) || others.iter().any(|f| f.calls.contains(name));
    let used_anywhere =
        |name: &str| local_idents.contains(name) || others.iter().any(|f| f.all.contains(name));
    let used_outside_directives = |name: &str| {
        local_no_directives.contains(name) || others.iter().any(|f| f.no_directives.contains(name))
    };

    for sym in parsed
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Variable))
    {
        if sym.name.starts_with('_') {
            continue;
        }
        if !used_anywhere(&sym.name) {
            diags.push(PawnDiagnostic::unnecessary_warning(
                sym.line,
                sym.col,
                sym.col + to_u32(sym.name.len()),
                codes::PP0005,
                msg(locale, MsgKey::VarUnused).replace("{}", &sym.name),
            ));
        }
    }

    for sym in parsed
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Stock | SymbolKind::Static))
    {
        if sym.name.starts_with('_') {
            continue;
        }
        if !used_as_call(&sym.name) {
            diags.push(PawnDiagnostic::unnecessary_warning(
                sym.line,
                sym.col,
                sym.col + to_u32(sym.name.len()),
                codes::PP0006,
                msg(locale, MsgKey::StockUnused).replace("{}", &sym.name),
            ));
        }
    }

    for sym in parsed
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Native))
    {
        if sym.name.starts_with('_') {
            continue;
        }
        if !used_as_call(&sym.name) {
            diags.push(PawnDiagnostic::hint(
                sym.line,
                sym.col,
                sym.col + to_u32(sym.name.len()),
                codes::PP0014,
                msg(locale, MsgKey::NativeNeverCalled).replace("{}", &sym.name),
            ));
        }
    }

    for sym in parsed
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Forward))
    {
        if sym.name.starts_with('_') {
            continue;
        }
        if !used_as_call(&sym.name) {
            diags.push(PawnDiagnostic::hint(
                sym.line,
                sym.col,
                sym.col + to_u32(sym.name.len()),
                codes::PP0015,
                msg(locale, MsgKey::ForwardNeverCalled).replace("{}", &sym.name),
            ));
        }
    }

    for sym in parsed
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Plain))
    {
        if sym.name.starts_with('_') {
            continue;
        }
        if !used_as_call(&sym.name) {
            diags.push(PawnDiagnostic::unnecessary_warning(
                sym.line,
                sym.col,
                sym.col + to_u32(sym.name.len()),
                codes::PP0016,
                msg(locale, MsgKey::FuncNeverCalled).replace("{}", &sym.name),
            ));
        }
    }

    for sym in parsed
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Define))
    {
        if sym.name.starts_with('_') {
            continue;
        }
        if !used_outside_directives(&sym.name) && !used_as_call(&sym.name) {
            diags.push(PawnDiagnostic::hint(
                sym.line,
                sym.col,
                sym.col + to_u32(sym.name.len()),
                codes::PP0011,
                msg(locale, MsgKey::DefineUnused).replace("{}", &sym.name),
            ));
        }
    }

    // "Este arquivo usa algo do include?" se responde olhando só para ele. O
    // `all_idents_ws` inclui o texto do próprio include — a declaração
    // `stock F()` contava como uso de `F`, e o aviso nunca saía.
    let mentioned = mentions(text);
    for inc in &parsed.includes {
        let Some(rp) = find_resolved_path(inc, &resolved.paths) else {
            continue;
        };

        let exported = collect_transitive_exports(rp, resolved);

        if exported.is_empty() {
            continue;
        }

        if !exported.iter().any(|name| mentioned.contains(name)) {
            diags.push(PawnDiagnostic::hint(
                inc.line,
                inc.col,
                inc.col + to_u32(inc.token.len()),
                codes::PP0012,
                msg(locale, MsgKey::IncludeNoSymbolsUsed).replace("{}", &inc.token),
            ));
        }
    }

    diags
}

fn is_include_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("inc" | "p" | "pawn")
    )
}

/// Os identificadores que um arquivo usa, nos três recortes que a análise
/// consulta, e os arquivos que ele inclui — de onde sai a unidade de
/// compilação.
#[derive(Debug, Default)]
pub struct FileIdents {
    calls: HashSet<String>,
    all: HashSet<String>,
    no_directives: HashSet<String>,
    includes: Vec<crate::parser::IncludeDirective>,
    /// O que o texto declara fora de funções — funções, constantes, macros,
    /// variáveis globais —, como o parser dá: o autocomplete e a busca de
    /// declaração leem daqui, sem reparsear o arquivo. O parser não registra
    /// locais.
    symbols: Vec<crate::parser::types::Symbol>,
    /// Todo identificador fora de comentário e string, sem os recortes acima.
    words: HashSet<String>,
    /// Quantas vezes cada identificador aparece seguido de `(`.
    call_counts: HashMap<String, usize>,
    /// Quantas vezes cada identificador aparece sozinho numa string — `"Nome"`,
    /// como uma `public` é chamada por `SetTimer`.
    quoted: HashMap<String, usize>,
}

impl FileIdents {
    /// Coleta os três recortes, as palavras, as chamadas, o que o texto
    /// declara e os includes.
    #[must_use]
    pub fn of(text: &str) -> Self {
        let (words, call_counts, quoted) = scan_words(text);
        let parsed = crate::parser::parse_file(text);
        Self {
            calls: collect_idents(text, CollectMode::Calls),
            all: collect_idents(text, CollectMode::AllIdents),
            no_directives: collect_idents(text, CollectMode::IdentsNoDefineLines),
            includes: parsed.includes,
            symbols: parsed.symbols,
            words,
            call_counts,
            quoted,
        }
    }

    /// Quantas vezes o texto escreve `"name"` — a string inteira é o nome.
    pub(crate) fn quote_count(&self, name: &str) -> usize {
        self.quoted.get(name).copied().unwrap_or(0)
    }

    /// Os símbolos que o texto declara fora de funções.
    pub(crate) fn symbols(&self) -> &[crate::parser::types::Symbol] {
        &self.symbols
    }

    /// Quantas vezes o texto escreve `name(` — a declaração inclusive.
    pub(crate) fn call_count(&self, name: &str) -> usize {
        self.call_counts.get(name).copied().unwrap_or(0)
    }

    /// Os `#include` do texto, ainda por resolver.
    pub(crate) fn includes(&self) -> &[crate::parser::IncludeDirective] {
        &self.includes
    }

    /// Se o texto escreve `name` em algum lugar fora de comentário e string —
    /// numa declaração com `new`, num `#define`, onde for.
    pub(crate) fn mentions_ident(&self, name: &str) -> bool {
        self.words.contains(name)
    }
}

/// Os identificadores de cada arquivo do workspace, lidos agora, sem cache.
///
/// É a coleta de referência dos testes: o cache do `WorkspaceState` tem de
/// chegar ao mesmo resultado, sobre a mesma lista de arquivos.
#[cfg(test)]
#[must_use]
pub fn collect_workspace(root: &Path, exclude: &Path) -> Vec<Arc<FileIdents>> {
    workspace_files(root, exclude)
        .filter_map(|path| std::fs::read(path).ok())
        .map(|bytes| Arc::new(FileIdents::of(&crate::parser::lexer::decode_bytes(&bytes))))
        .collect()
}

/// Os arquivos Pawn do workspace, menos o que está sendo analisado.
pub fn workspace_files<'a>(
    root: &'a Path,
    exclude: &'a Path,
) -> impl Iterator<Item = std::path::PathBuf> + 'a {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(move |e| {
            e.file_type().is_file() && {
                let p = e.path();
                p != exclude
                    && matches!(
                        p.extension().and_then(|x| x.to_str()),
                        Some("pwn" | "inc" | "p" | "pawn")
                    )
            }
        })
        .map(walkdir::DirEntry::into_path)
}

fn collect_transitive_exports(
    root: &std::path::PathBuf,
    resolved: &ResolvedIncludes,
) -> HashSet<String> {
    let mut exported = HashSet::new();
    let mut visited: HashSet<&std::path::PathBuf> = HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(root);

    while let Some(path) = queue.pop_front() {
        if !visited.insert(path) {
            continue;
        }
        let Some(entry) = resolved.files.get(path) else {
            continue;
        };
        exported.extend(entry.parsed.symbols.iter().map(|s| s.name.clone()));
        exported.extend(entry.parsed.macro_names.iter().cloned());

        for nested_inc in &entry.parsed.includes {
            if let Some(nested_path) = find_resolved_path(nested_inc, &resolved.paths) {
                queue.push_back(nested_path);
            }
        }
    }

    exported
}

fn find_resolved_path<'a>(
    inc: &crate::parser::IncludeDirective,
    paths: &'a [std::path::PathBuf],
) -> Option<&'a std::path::PathBuf> {
    paths.iter().find(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let token_stem = std::path::Path::new(&inc.token)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&inc.token);
        stem.eq_ignore_ascii_case(token_stem) || p.to_string_lossy().contains(&*inc.token)
    })
}

// Caracterização: o que o analisador acusa hoje. Otimizar a coleta de
// identificadores não pode mudar nenhum destes resultados.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::includes::collect_included_files;
    use crate::parser::parse_file;
    use std::path::PathBuf;

    /// Um projeto Pawn em disco, que se apaga sozinho.
    struct Project(PathBuf);

    impl Project {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("pawnpro-unused-{tag}-{nanos}"));
            std::fs::create_dir_all(root.join("include")).expect("criar projeto");
            Self(root)
        }

        fn write(&self, relative: &str, body: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::write(&path, body).expect("escrever arquivo");
            path
        }

        /// Os avisos para `file`, analisado como a engine faz: parse, includes
        /// resolvidos e, se pedido, a varredura do workspace.
        fn unused(
            &self,
            file: &Path,
            warn_in_inc: bool,
            scan_workspace: bool,
        ) -> Vec<(&'static str, String)> {
            let text = std::fs::read_to_string(file).expect("ler arquivo");
            let parsed = parse_file(&text);
            let inc_paths = vec![self.0.join("include")];
            let resolved = collect_included_files(file, &inc_paths, &parsed.includes, 16, 1000);
            // Os mesmos "outros arquivos" que a engine entrega: os includes
            // resolvidos e, se pedido, o resto do projeto.
            let mut others: Vec<Arc<FileIdents>> = resolved
                .paths
                .iter()
                .filter_map(|p| resolved.files.get(p))
                .map(|entry| Arc::new(FileIdents::of(&entry.text)))
                .collect();
            if scan_workspace {
                others.extend(collect_workspace(&self.0, file));
            }
            analyze_unused(
                &text,
                file,
                &parsed,
                &resolved,
                warn_in_inc,
                &others,
                Locale::default(),
            )
            .into_iter()
            .map(|d| (d.code, d.message))
            .collect()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn flagged(diags: &[(&str, String)], code: &str, name: &str) -> bool {
        diags.iter().any(|(c, m)| *c == code && m.contains(name))
    }

    #[test]
    fn an_unused_global_is_flagged_and_a_used_one_is_not() {
        let project = Project::new("var");
        let main = project.write(
            "main.pwn",
            "new gUsedValue;\nnew gIdleValue;\n\nmain()\n{\n\tgUsedValue = 1;\n}\n",
        );
        let diags = project.unused(&main, false, false);
        assert!(flagged(&diags, codes::PP0005, "gIdleValue"), "{diags:?}");
        assert!(!flagged(&diags, codes::PP0005, "gUsedValue"), "{diags:?}");
    }

    #[test]
    fn an_uncalled_stock_is_flagged_and_a_called_one_is_not() {
        let project = Project::new("stock");
        let main = project.write(
            "main.pwn",
            "stock IdleHelper()\n{\n}\n\nstock CalledHelper()\n{\n}\n\nmain()\n{\n\tCalledHelper();\n}\n",
        );
        let diags = project.unused(&main, false, false);
        assert!(flagged(&diags, codes::PP0006, "IdleHelper"), "{diags:?}");
        assert!(!flagged(&diags, codes::PP0006, "CalledHelper"), "{diags:?}");
    }

    #[test]
    fn a_stock_called_only_by_another_project_file_is_not_flagged() {
        // É o motivo da varredura do workspace: o include declara, e quem chama
        // é um arquivo que ele não inclui. Sem a varredura, o aviso sairia
        // errado — o contraste é o que prova que ela está funcionando.
        let project = Project::new("workspace");
        let lib = project.write("include/lib.inc", "stock SharedHelper()\n{\n}\n");
        project.write(
            "other.pwn",
            "#include <lib>\n\nmain()\n{\n\tSharedHelper();\n}\n",
        );

        let without_scan = project.unused(&lib, true, false);
        assert!(
            flagged(&without_scan, codes::PP0006, "SharedHelper"),
            "{without_scan:?}"
        );

        let with_scan = project.unused(&lib, true, true);
        assert!(
            !flagged(&with_scan, codes::PP0006, "SharedHelper"),
            "{with_scan:?}"
        );
    }

    #[test]
    fn an_include_whose_symbols_go_unused_is_flagged() {
        let project = Project::new("include");
        project.write("include/lib.inc", "stock LibOnlyFunction()\n{\n}\n");
        let main = project.write("main.pwn", "#include <lib>\n\nmain()\n{\n}\n");
        let diags = project.unused(&main, false, false);
        assert!(flagged(&diags, codes::PP0012, "lib"), "{diags:?}");
    }

    #[test]
    fn an_include_used_only_in_a_declaration_is_not_flagged() {
        // A outra face do aviso: as linhas de `new` ficam de fora da coleta de
        // usos de variável, mas usam o include do mesmo jeito.
        let project = Project::new("include-in-new");
        project.write(
            "include/lib.inc",
            "stock Float:LibSquare(Float:value)\n{\n\treturn value * value;\n}\n",
        );
        let main = project.write(
            "main.pwn",
            "#include <lib>\n\nmain()\n{\n\tnew Float:area = LibSquare(2.0);\n\t#pragma unused area\n}\n",
        );
        let diags = project.unused(&main, false, false);
        assert!(!flagged(&diags, codes::PP0012, "lib"), "{diags:?}");
    }

    #[test]
    fn an_include_file_is_quiet_unless_asked() {
        let project = Project::new("quiet");
        let lib = project.write("include/lib.inc", "stock NobodyCalls()\n{\n}\n");
        assert!(project.unused(&lib, false, false).is_empty());
        assert!(flagged(
            &project.unused(&lib, true, false),
            codes::PP0006,
            "NobodyCalls"
        ));
    }
}
