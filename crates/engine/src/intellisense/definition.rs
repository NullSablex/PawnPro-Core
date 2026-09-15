use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::parser::types::SymbolKind;
use crate::text::utf16_col;
use crate::workspace::WorkspaceState;

use super::{Origin, extract_word, locate_symbol};

/// Onde o símbolo sob `position` é declarado, pela mesma busca do hover. Um
/// `forward` só anuncia a função: se a unidade tem o corpo, é para ele que se
/// vai.
pub fn get_definition(state: &WorkspaceState, uri: &str, position: Position) -> Option<Location> {
    let text = state.get_text(uri)?;
    let path = crate::workspace::uri_to_path(uri)?;
    let parsed = state.get_parsed(uri)?;
    let line = text.lines().nth(position.line as usize)?;
    let word = extract_word(line, position.character as usize)?;

    let origin = Origin {
        uri,
        path: &path,
        text: &text,
        parsed: &parsed,
    };
    let found = locate_symbol(state, &origin, &word, |s| {
        !matches!(s.kind, SymbolKind::Forward)
    })
    .or_else(|| locate_symbol(state, &origin, &word, |_| true))?;

    let sym = &found.symbol;
    let target = found.text.lines().nth(sym.line as usize).unwrap_or("");
    let col = sym.col as usize;
    Some(Location {
        uri: Url::parse(&found.uri).ok()?,
        range: Range {
            start: Position {
                line: sym.line,
                character: utf16_col(target, col),
            },
            end: Position {
                line: sym.line,
                character: utf16_col(target, col + sym.name.len()),
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A definição sob `(line, character)` do `main.pwn`, num projeto em que
    /// ele declara um `forward`, o `lib.inc` incluído tem o corpo e um outro
    /// programa, `fs.pwn`, tem uma função de mesmo nome.
    fn definition_in_project(line: u32, character: u32) -> Option<(String, Range)> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-definition-{nanos}"));
        std::fs::create_dir_all(root.join("include")).expect("criar projeto");
        std::fs::write(
            root.join("include").join("lib.inc"),
            "stock Helper(a)\n{\n\treturn a;\n}\n",
        )
        .expect("escrever lib");
        std::fs::write(root.join("fs.pwn"), "stock Helper(a)\n{\n\treturn a;\n}\n")
            .expect("escrever fs");
        let main = root.join("main.pwn");
        let main_text =
            "#include <lib>\nforward Helper(a);\n\nmain()\n{\n\tHelper(1);\n\tUnknown();\n}\n";
        std::fs::write(&main, main_text).expect("escrever main");

        let mut state = WorkspaceState::new();
        state.set_workspace_root(root.clone());
        state.include_paths = vec![root.join("include")];
        let uri = Url::from_file_path(&main).expect("uri").to_string();
        state.open_document(uri.clone(), main_text.into(), 1);
        let found = get_definition(&state, &uri, Position { line, character })
            .map(|l| (l.uri.path().to_string(), l.range));
        let _ = std::fs::remove_dir_all(&root);
        found
    }

    #[test]
    fn a_call_goes_to_the_body_in_the_include_not_the_forward() {
        let (path, range) = definition_in_project(5, 2).expect("definição");
        assert!(path.ends_with("include/lib.inc"), "{path}");
        assert_eq!((range.start.line, range.start.character), (0, 6));
        assert_eq!(range.end.character, 12);
    }

    #[test]
    fn a_name_undeclared_in_the_unit_has_no_definition() {
        assert!(definition_in_project(6, 2).is_none());
    }
}
