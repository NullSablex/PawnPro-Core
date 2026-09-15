use std::collections::HashSet;
use std::path::Path;

use regex::Regex;

use crate::messages::{Locale, MsgKey, msg};
use crate::parser::lexer::{mask_string_literals, strip_line_comments};
use crate::parser::{ParsedFile, SymbolKind};

use super::includes::ResolvedIncludes;
use super::{codes, diagnostic::PawnDiagnostic};
use crate::util::to_u32;

static RX_CALL: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"\b(?:([A-Za-z_][A-Za-z0-9_]*)::)?([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap()
});

// Limite de caracteres do compilador real (sNAMEMAX = 31)
const SNAME_MAX: usize = 31;

/// Corta em `SNAME_MAX` bytes. Cortar por byte é seguro porque as expressões
/// só capturam identificadores ASCII, como os do próprio Pawn.
fn truncate_name(name: &str) -> &str {
    if name.len() <= SNAME_MAX {
        name
    } else {
        &name[..SNAME_MAX]
    }
}

static RESERVED: std::sync::LazyLock<HashSet<&'static str>> = std::sync::LazyLock::new(|| {
    [
        "if", "else", "for", "while", "do", "switch", "case", "return", "sizeof", "tagof", "state",
        "goto", "assert", "break", "continue", "exit", "sleep", "new", "static", "const", "public",
        "stock", "native", "forward",
    ]
    .into_iter()
    .collect()
});

pub fn analyze_undefined(
    text: &str,
    file_path: &Path,
    parsed: &ParsedFile,
    resolved: &ResolvedIncludes,
    sdk_parsed: Option<&ParsedFile>,
    locale: Locale,
) -> Vec<PawnDiagnostic> {
    // PP0010 só faz sentido em compilation units (.pwn).
    // .inc, .p, .pawn são include files — nunca compilados diretamente.
    let is_include = matches!(
        file_path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("inc" | "p" | "pawn")
    );
    if is_include {
        return vec![];
    }

    let mut known: HashSet<String> = HashSet::new();
    let mut func_prefixes: HashSet<String> = HashSet::new();

    let sources: Vec<&ParsedFile> = {
        let mut v: Vec<&ParsedFile> = Vec::new();
        if let Some(sdk) = sdk_parsed {
            v.push(sdk);
        }
        v.push(parsed);
        v
    };

    for p in &sources {
        for sym in &p.symbols {
            if !matches!(sym.kind, SymbolKind::Variable) {
                known.insert(truncate_name(&sym.name).to_string());
            }
        }
        for name in &p.macro_names {
            known.insert(truncate_name(name).to_string());
        }
        for prefix in &p.func_macro_prefixes {
            func_prefixes.insert(truncate_name(prefix).to_string());
        }
    }
    for fp in &resolved.paths {
        if let Some(entry) = resolved.files.get(fp) {
            for sym in &entry.parsed.symbols {
                if !matches!(sym.kind, SymbolKind::Variable) {
                    known.insert(truncate_name(&sym.name).to_string());
                }
            }
            for name in &entry.parsed.macro_names {
                known.insert(truncate_name(name).to_string());
            }
            for prefix in &entry.parsed.func_macro_prefixes {
                func_prefixes.insert(truncate_name(prefix).to_string());
            }
        }
    }

    let mut diags = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut in_block = false;

    for (line_idx, raw_line) in lines.iter().enumerate() {
        let raw_line = raw_line.trim_end_matches('\r');
        let stripped = strip_line_comments(raw_line, in_block);
        in_block = stripped.in_block;
        let line = mask_string_literals(&stripped.text);
        let line = &line;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }

        for cap in RX_CALL.captures_iter(line) {
            let namespace = cap.get(1).map(|m| m.as_str());
            let name = cap.get(2).map_or("", |m| m.as_str());
            if name.is_empty() {
                continue;
            }

            // Aplica truncagem igual ao compilador real (sNAMEMAX=31)
            let name_trunc = truncate_name(name);

            if RESERVED.contains(name_trunc) || known.contains(name_trunc) {
                continue;
            }

            if let Some(ns) = namespace {
                let ns_trunc = truncate_name(ns);
                let expanded = format!("{ns_trunc}_{name_trunc}");
                if known.contains(expanded.as_str())
                    || known.contains(ns_trunc)
                    || func_prefixes.contains(ns_trunc)
                {
                    continue;
                }
                continue; // unknown namespace — macro may use other patterns
            }

            let col = to_u32(raw_line.find(name).unwrap_or(0));
            diags.push(PawnDiagnostic::warning(
                to_u32(line_idx),
                col,
                col + to_u32(name.len()),
                codes::PP0010,
                msg(locale, MsgKey::SymbolUndeclared).replace("{}", name),
            ));
        }
    }

    diags
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::includes::collect_included_files;
    use crate::parser::parse_file;
    use std::path::PathBuf;

    /// Uma pasta temporária que se apaga sozinha.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("pawnpro-undefined-{tag}-{nanos}"));
            std::fs::create_dir_all(&root).expect("criar pasta");
            Self(root)
        }

        fn write(&self, name: &str, body: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, body).expect("escrever arquivo");
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Analisa o arquivo como a engine: com os includes resolvidos a partir dele.
    fn undefined_in(path: &Path, src: &str) -> Vec<PawnDiagnostic> {
        let parsed = parse_file(src);
        let resolved = collect_included_files(path, &[], &parsed.includes, 8, 100);
        analyze_undefined(src, path, &parsed, &resolved, None, Locale::default())
    }

    fn undefined(src: &str) -> Vec<PawnDiagnostic> {
        undefined_in(Path::new("/nao/existe/main.pwn"), src)
    }

    #[test]
    fn an_undeclared_call_is_flagged() {
        let diags = undefined("main()\n{\n    Foo(1);\n}\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, codes::PP0010);
        assert_eq!(diags[0].line, 2);
        assert_eq!((diags[0].col_start, diags[0].col_end), (4, 7));
    }

    #[test]
    fn declared_in_the_same_file_is_not_flagged() {
        assert!(undefined("stock Foo(a) { return a; }\nmain()\n{\n    Foo(1);\n}\n").is_empty());
    }

    #[test]
    fn declared_in_an_include_is_not_flagged() {
        let dir = Dir::new("include");
        dir.write("lib.inc", "stock Foo(a) { return a; }\n");
        let src = "#include \"lib\"\nmain()\n{\n    Foo(1);\n}\n";
        let main = dir.write("main.pwn", src);
        assert!(undefined_in(&main, src).is_empty());
    }

    #[test]
    fn an_include_file_is_not_checked() {
        let src = "stock Bar()\n{\n    Foo(1);\n}\n";
        let diags = analyze_undefined(
            src,
            Path::new("/x/lib.inc"),
            &parse_file(src),
            &collect_included_files(Path::new("/x/lib.inc"), &[], &[], 8, 100),
            None,
            Locale::default(),
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn names_are_compared_up_to_the_compiler_limit() {
        // O compilador corta em 31 caracteres: a chamada casa com a declaração
        // mesmo diferindo depois disso.
        let decl = format!("{}Tail", "A".repeat(31));
        let call = format!("{}Other", "A".repeat(31));
        let src = format!("stock {decl}() {{ }}\nmain()\n{{\n    {call}();\n}}\n");
        assert!(undefined(&src).is_empty());
    }

    #[test]
    fn accented_text_does_not_crash_the_analysis() {
        // Pawn não aceita acento em identificador; o que importa é não entrar
        // em pane com um nome longo acentuado, cortado no limite de 31 bytes.
        let src = format!("main()\n{{\n    {}ção();\n}}\n", "a".repeat(30));
        let _ = undefined(&src);
    }

    #[test]
    fn strings_comments_and_directives_are_not_calls() {
        let src = "main()\n{\n    print(\"Foo(1)\"); // Bar(2)\n}\n#define X Baz(3)\n";
        let parsed_known = "native print(const s[]);\n".to_string() + src;
        assert!(undefined(&parsed_known).is_empty());
    }

    #[test]
    fn reserved_words_are_not_calls() {
        assert!(undefined("main()\n{\n    if (1) return sizeof(x);\n}\n").is_empty());
    }
}
