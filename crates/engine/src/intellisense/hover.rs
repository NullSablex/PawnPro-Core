use std::fmt::Write as _;
use std::path::Path;

use regex::Regex;
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

use crate::analyzer::includes::resolve_include;
use crate::messages::{MsgKey, msg};
use crate::parser::types::{IncludeDirective, Symbol, SymbolKind};
use crate::workspace::WorkspaceState;

use super::{Origin, extract_word, locate_symbol};

static RX_INCLUDE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r#"#\s*include\s*(?:<([^>]+)>|"([^"]+)")"#).unwrap());

pub fn get_hover(state: &WorkspaceState, uri: &str, position: Position) -> Option<Hover> {
    let locale = state.locale;
    let text = state.get_text(uri)?;
    let file_path = crate::workspace::uri_to_path(uri)?;
    let inc_paths = state.include_paths.clone();
    let parsed = state.get_parsed(uri)?;

    let lines: Vec<&str> = text.lines().collect();
    let line_idx = position.line as usize;
    let col = position.character as usize;

    if line_idx >= lines.len() {
        return None;
    }
    let line = lines[line_idx];

    if let Some(h) = hover_include(line, &file_path, &inc_paths) {
        return Some(h);
    }

    let word = extract_word(line, col)?;
    let origin = Origin {
        uri,
        path: &file_path,
        text: &text,
        parsed: &parsed,
    };
    let found = locate_symbol(state, &origin, &word, |_| true)?;

    Some(format_symbol(&found.symbol, locale))
}

fn hover_include(line: &str, file_path: &Path, inc_paths: &[std::path::PathBuf]) -> Option<Hover> {
    if !line.trim().starts_with('#') {
        return None;
    }
    let cap = RX_INCLUDE.captures(line)?;
    let (token, is_angle) = if let Some(m) = cap.get(1) {
        (m.as_str().to_string(), true)
    } else {
        (cap.get(2)?.as_str().to_string(), false)
    };

    let dir = IncludeDirective {
        token: token.clone(),
        is_angle,
        is_try: false,
        line: 0,
        col: 0,
    };
    let file_dir = file_path.parent().unwrap_or_else(|| Path::new("."));
    resolve_include(&dir, file_dir, inc_paths)?;

    let md = format!("```\n{}\n```\n\n`{}`", line.trim(), token);
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: None,
    })
}

pub(super) fn doc_labels(locale: crate::messages::Locale) -> super::DocLabels {
    super::DocLabels {
        params: msg(locale, MsgKey::HoverParams).to_string(),
        returns: msg(locale, MsgKey::HoverReturns).to_string(),
        remarks: msg(locale, MsgKey::HoverRemarks).to_string(),
    }
}

fn format_symbol(sym: &Symbol, locale: crate::messages::Locale) -> Hover {
    let kw = match sym.kind {
        SymbolKind::Native => "native",
        SymbolKind::Forward => "forward",
        SymbolKind::Public => "public",
        SymbolKind::Stock => "stock",
        SymbolKind::Static => "static",
        SymbolKind::Plain => "",
        SymbolKind::StaticConst | SymbolKind::Const => "const",
        SymbolKind::Enum => "enum",
        SymbolKind::Define => "#define",
        SymbolKind::Variable => "new",
    };

    let declaration = sym.signature.as_deref().unwrap_or(&sym.name);
    let mut md = if kw.is_empty() {
        format!("```pawn\n{declaration}\n```")
    } else {
        format!("```pawn\n{kw} {declaration}\n```")
    };

    if sym.deprecated {
        // Sem blockquote: o editor recua o bloco inteiro, e o `---` seguinte
        // passa a ser lido como continuação dele.
        let _ = write!(md, "\n\n{}", msg(locale, MsgKey::HoverDeprecated));
        // A mensagem da diretiva costuma dizer o que usar no lugar.
        if let Some(m) = sym.deprecated_message.as_deref().filter(|m| !m.is_empty()) {
            let _ = write!(md, " — {m}");
        }
    }

    if let Some(rendered) = sym
        .doc
        .as_deref()
        .map(super::parse_doc)
        .and_then(|d| d.to_markdown(&doc_labels(locale)))
    {
        let _ = write!(md, "\n\n---\n{rendered}");
    }

    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Locale;
    use crate::parser::types::SymbolKind;

    fn sym(deprecated: bool, message: Option<&str>, doc: Option<&str>) -> Symbol {
        Symbol {
            name: "BanirComMotivo".into(),
            kind: SymbolKind::Stock,
            signature: Some("BanirComMotivo(playerid)".into()),
            params: vec![],
            deprecated,
            deprecated_message: message.map(str::to_string),
            doc: doc.map(str::to_string),
            line: 0,
            col: 0,
        }
    }

    /// O hover sobre `(line, character)` num projeto com `lib.inc` (aberto ou
    /// não) e `main.pwn` aberto, que o inclui e chama a função.
    fn hover_in_project(lib_open: bool, line: u32, character: u32, on_lib: bool) -> Option<String> {
        use tower_lsp::lsp_types::Url;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-hover-{nanos}"));
        std::fs::create_dir_all(root.join("include")).expect("criar projeto");
        let lib = root.join("include").join("lib.inc");
        let lib_text = "stock Helper(a)\n{\n\treturn a;\n}\n";
        std::fs::write(&lib, lib_text).expect("escrever lib");
        let main = root.join("main.pwn");
        let main_text = "#include <lib>\n\nmain()\n{\n\tHelper(1);\n}\n";
        std::fs::write(&main, main_text).expect("escrever main");

        let mut state = WorkspaceState::new();
        state.set_workspace_root(root.clone());
        state.include_paths = vec![root.join("include")];
        let lib_uri = Url::from_file_path(&lib).expect("uri").to_string();
        let main_uri = Url::from_file_path(&main).expect("uri").to_string();
        if lib_open {
            state.open_document(lib_uri.clone(), lib_text.into(), 1);
        }
        state.open_document(main_uri.clone(), main_text.into(), 1);
        let uri = if on_lib { &lib_uri } else { &main_uri };
        let hover = get_hover(&state, uri, Position { line, character }).map(|h| markdown(&h));
        let _ = std::fs::remove_dir_all(&root);
        hover
    }

    #[test]
    fn hover_on_a_call_shows_the_included_stock() {
        for lib_open in [false, true] {
            let h = hover_in_project(lib_open, 4, 2, false);
            assert!(
                h.is_some_and(|m| m.contains("Helper")),
                "lib aberta: {lib_open}"
            );
        }
    }

    #[test]
    fn hover_finds_the_stock_of_a_sibling_file() {
        // `a.inc` chama o que `b.inc` declara sem incluí-lo: os dois são
        // compilados juntos pelo `main.pwn`.
        use tower_lsp::lsp_types::Url;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-hover-sibling-{nanos}"));
        std::fs::create_dir_all(&root).expect("criar projeto");
        std::fs::write(root.join("b.inc"), "stock Helper(a)\n{\n\treturn a;\n}\n")
            .expect("escrever b");
        let a = root.join("a.inc");
        let a_text = "Other()\n{\n\tHelper(1);\n}\n";
        std::fs::write(&a, a_text).expect("escrever a");
        std::fs::write(
            root.join("main.pwn"),
            "#include \"a\"\n#include \"b\"\n\nmain()\n{\n}\n",
        )
        .expect("escrever main");

        let mut state = WorkspaceState::new();
        state.set_workspace_root(root.clone());
        let a_uri = Url::from_file_path(&a).expect("uri").to_string();
        state.open_document(a_uri.clone(), a_text.into(), 1);
        let h = get_hover(
            &state,
            &a_uri,
            Position {
                line: 2,
                character: 2,
            },
        )
        .map(|h| markdown(&h));
        let _ = std::fs::remove_dir_all(&root);

        assert!(h.is_some_and(|m| m.contains("Helper(a)")));
    }

    #[test]
    fn hover_does_not_show_a_stock_of_another_program() {
        // O caso real: a função foi renomeada no include, e o nome antigo só
        // existe declarado num `.pwn` que não é compilado junto.
        use tower_lsp::lsp_types::Url;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-hover-other-{nanos}"));
        std::fs::create_dir_all(root.join("include")).expect("criar projeto");
        std::fs::write(
            root.join("include").join("lib.inc"),
            "stock Helper1(a)\n{\n\treturn a;\n}\n",
        )
        .expect("escrever lib");
        std::fs::write(root.join("fs.pwn"), "stock Helper(a)\n{\n\treturn a;\n}\n")
            .expect("escrever fs");
        let main = root.join("main.pwn");
        let main_text = "#include <lib>\n\nmain()\n{\n\tHelper(1);\n}\n";
        std::fs::write(&main, main_text).expect("escrever main");

        let mut state = WorkspaceState::new();
        state.set_workspace_root(root.clone());
        state.include_paths = vec![root.join("include")];
        let main_uri = Url::from_file_path(&main).expect("uri").to_string();
        state.open_document(main_uri.clone(), main_text.into(), 1);
        let h = get_hover(
            &state,
            &main_uri,
            Position {
                line: 4,
                character: 2,
            },
        );
        let _ = std::fs::remove_dir_all(&root);

        assert!(h.is_none());
    }

    #[test]
    fn hover_on_the_declaration_shows_its_stock() {
        let h = hover_in_project(true, 0, 8, true);
        assert!(h.is_some_and(|m| m.contains("Helper")));
    }

    fn markdown(h: &Hover) -> String {
        match &h.contents {
            HoverContents::Markup(m) => m.value.clone(),
            _ => panic!("esperado markup"),
        }
    }

    #[test]
    fn deprecation_is_not_a_blockquote() {
        // `>` faz o editor recuar o bloco e engolir o `---` seguinte como
        // continuação — foi o que deixava o hover torto.
        let md = markdown(&format_symbol(&sym(true, None, None), Locale::PtBr));
        assert!(!md.contains('>'), "{md}");
        assert!(md.contains("Depreciado"), "{md}");
    }

    #[test]
    fn deprecation_message_is_shown_in_the_hover() {
        let md = markdown(&format_symbol(
            &sym(true, Some("Use BanPlayerFor"), None),
            Locale::PtBr,
        ));
        assert!(md.contains("Use BanPlayerFor"), "{md}");
    }

    #[test]
    fn signature_comes_first_and_doc_after_the_rule() {
        let md = markdown(&format_symbol(
            &sym(false, None, Some("/**\n * Bane alguém.\n */")),
            Locale::PtBr,
        ));
        assert!(md.starts_with("```pawn\nstock BanirComMotivo(playerid)\n```"));
        assert!(md.contains("\n---\n"), "{md}");
        assert!(md.contains("Bane alguém."), "{md}");
    }

    #[test]
    fn a_symbol_without_doc_still_shows_its_signature() {
        let md = markdown(&format_symbol(&sym(false, None, None), Locale::PtBr));
        assert!(md.contains("BanirComMotivo(playerid)"));
        assert!(!md.contains("---"), "sem doc não há regra: {md}");
    }
}
