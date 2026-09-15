//! Rename de símbolos. Reaproveita `get_references` para localizar todas as
//! ocorrências e as converte num `WorkspaceEdit`. Sem renomear arquivos nem
//! tocar em símbolos de bibliotecas — apenas texto nas ocorrências encontradas.

use std::collections::HashMap;
use std::path::Path;

use tower_lsp::lsp_types::{Position, Range, TextEdit, Url, WorkspaceEdit};

use super::references::{Scope, declared_names, get_references, resolve_callable, scope_of};
use crate::messages::{MsgKey, msg};
use crate::parser::lexer::{mask_string_literals, strip_line_comments, update_brace_depth};
use crate::parser::parse_file;
use crate::text::word_range_at;
use crate::workspace::{WorkspaceState, uri_to_path};

/// Valida que há um identificador na posição e devolve seu intervalo (para o
/// editor destacar o alvo do rename). `None` quando não há palavra.
#[must_use]
pub fn prepare_rename(state: &WorkspaceState, uri: &str, pos: Position) -> Option<Range> {
    let doc = state.open_docs.get(uri)?;
    word_range_at(&doc.text, pos)
}

/// Produz o `WorkspaceEdit` que renomeia todas as ocorrências do símbolo sob o
/// cursor para `new_name`. `Ok(None)` se não houver o que renomear; `Err`, com
/// o motivo, quando renomear por nome atingiria outra variável.
///
/// # Errors
///
/// O motivo da recusa, pronto para o editor mostrar.
pub fn get_rename(
    state: &WorkspaceState,
    uri: &str,
    pos: Position,
    new_name: &str,
) -> Result<Option<WorkspaceEdit>, String> {
    if let Some(reason) = refusal(state, uri, pos) {
        return Err(reason);
    }
    let locations = get_references(state, uri, pos);
    if locations.is_empty() {
        return Ok(None);
    }

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for loc in locations {
        changes.entry(loc.uri).or_default().push(TextEdit {
            range: loc.range,
            new_text: new_name.to_string(),
        });
    }

    Ok(Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    }))
}

/// Por que renomear o nome sob o cursor seria inseguro, se for. É o caso de
/// uma variável global — não de uma função, que só é renomeada onde aparece
/// com `(` — cujo nome também é parâmetro ou local em algum ponto da unidade:
/// sem análise de escopo, renomear por nome alteraria essa outra também.
fn refusal(state: &WorkspaceState, uri: &str, pos: Position) -> Option<String> {
    let text = state.open_docs.get(uri).map(|doc| doc.text.clone())?;
    let word = crate::text::word_at(&text, pos)?;
    let file = uri_to_path(uri)?;
    let global = matches!(
        scope_of(state, uri, &file, &text, &word, pos.line)?,
        Scope::Unit { .. }
    );
    if !global || resolve_callable(state, uri, &text, &word, pos) {
        return None;
    }
    let (where_uri, line) = local_named(state, &file, &word)?;
    let name = uri_to_path(&where_uri)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or(where_uri);
    let place = format!("{name}:{}", line + 1);
    Some(
        msg(state.locale, MsgKey::RenameShadowed)
            .replacen("{}", &word, 1)
            .replacen("{}", &place, 1),
    )
}

/// A URI e a linha onde `word` é parâmetro ou `new`/`static` local, em algum
/// arquivo da unidade de `file`.
fn local_named(state: &WorkspaceState, file: &Path, word: &str) -> Option<(String, usize)> {
    state
        .unit_texts(file, |f| f.mentions_ident(word))
        .into_iter()
        .find_map(|(uri, text)| {
            let param = parse_file(&text)
                .symbols
                .into_iter()
                .find(|s| s.params.iter().any(|p| p.name == word));
            if let Some(sym) = param {
                return Some((uri, sym.line as usize));
            }
            let mut depth = 0;
            let mut in_block = false;
            for (i, raw) in text.lines().enumerate() {
                let stripped = strip_line_comments(raw.trim_end_matches('\r'), in_block);
                in_block = stripped.in_block;
                let code = mask_string_literals(&stripped.text);
                if depth > 0 && declared_names(&code).iter().any(|n| n == word) {
                    return Some((uri, i));
                }
                depth = update_brace_depth(&code, depth);
            }
            None
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "file:///test.pwn";

    fn state_with(text: &str) -> WorkspaceState {
        let st = WorkspaceState::new();
        st.open_document(URI.to_string(), text.to_string(), 1);
        st
    }

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn prepare_returns_word_range() {
        let st = state_with("stock count() {}\n");
        let range = prepare_rename(&st, URI, at(0, 8)).unwrap();
        assert_eq!(range.start.character, 6); // início de "count"
        assert_eq!(range.end.character, 11); // fim de "count"
    }

    #[test]
    fn prepare_none_off_identifier() {
        let st = state_with("stock count() {}\n");
        // coluna 5 = o espaço entre "stock" e "count" não toca identificador? Na
        // verdade encosta no fim de "stock"; uso um ponto claramente vazio.
        assert!(prepare_rename(&st, URI, at(0, 13)).is_none()); // dentro de "()"
    }

    #[test]
    fn rename_renames_all_occurrences() {
        // `n` aparece como parâmetro e no corpo; ambos devem ser renomeados.
        let text = "stock dbl(n) { return n + n; }\n";
        let st = state_with(text);
        let edit = get_rename(&st, URI, at(0, 10), "value")
            .expect("sem recusa")
            .unwrap();
        let changes = edit.changes.unwrap();
        let edits = &changes[&URI.parse::<Url>().unwrap()];
        // 3 ocorrências de `n`: parâmetro + duas no corpo.
        assert_eq!(edits.len(), 3, "esperava 3 edições, got: {edits:?}");
        assert!(edits.iter().all(|e| e.new_text == "value"));
    }

    #[test]
    fn rename_none_when_no_word() {
        let st = state_with("stock f() {}\n");
        // dentro de "()"
        assert!(
            get_rename(&st, URI, at(0, 9), "x")
                .expect("sem recusa")
                .is_none()
        );
    }

    // Fluxo do code action de estilo: dado um nome fora da convenção, a sugestão
    // (camelCase) vira um rename de todas as ocorrências. Une suggestions_for +
    // get_rename como o handler de code action faz.
    #[test]
    fn style_suggestion_drives_rename() {
        use crate::config::{NamingConfig, StyleConfig};

        let text = "stock get_thing() { return get_thing(); }\n";
        let st = state_with(text);
        let cfg = NamingConfig {
            enabled: true,
            style: StyleConfig {
                functions: vec!["camelCase".to_string()],
                ..StyleConfig::default()
            },
            ..NamingConfig::default()
        };

        let suggestions = crate::naming::suggestions_for("get_thing", &cfg);
        assert_eq!(suggestions, vec!["getThing"]);

        // Renomear para a sugestão atinge declaração + chamada.
        let edit = get_rename(&st, URI, at(0, 7), &suggestions[0])
            .expect("sem recusa")
            .unwrap();
        let edits = &edit.changes.unwrap()[&URI.parse::<Url>().unwrap()];
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|e| e.new_text == "getThing"));
    }

    /// As linhas das edições de um rename aceito.
    fn edited_lines(text: &str, pos: Position) -> Vec<u32> {
        let st = state_with(text);
        let edit = get_rename(&st, URI, pos, "novo")
            .expect("sem recusa")
            .expect("há o que renomear");
        let mut lines: Vec<u32> = edit.changes.unwrap()[&URI.parse::<Url>().unwrap()]
            .iter()
            .map(|e| e.range.start.line)
            .collect();
        lines.sort_unstable();
        lines
    }

    #[test]
    fn a_declaration_list_yields_one_name_per_item() {
        assert_eq!(
            declared_names("\tnew a = 1, b[3], Float:c;"),
            vec!["a", "b", "c"]
        );
        assert_eq!(declared_names("\tnew x = Func(1, 2), y;"), vec!["x", "y"]);
        assert_eq!(declared_names("\tfor (new i = 0; i < n; i++)"), vec!["i"]);
        assert_eq!(declared_names("\tnew const k = 3;"), vec!["k"]);
        assert_eq!(declared_names("\tstatic s;"), vec!["s"]);
        assert!(declared_names("\tx = new_value;").is_empty());
    }

    #[test]
    fn a_global_listed_after_an_initializer_is_refused() {
        let st = state_with("new gCount;\n\nmain()\n{\n\tnew a = 1, gCount;\n}\n");
        assert!(get_rename(&st, URI, at(0, 5), "novo").is_err());
    }

    #[test]
    fn a_for_loop_variable_does_not_reach_the_next_loop() {
        let text = "main()\n{\n\tfor (new i = 0; i < 3; i++)\n\t{\n\t\tprint(i);\n\t}\n\tfor (new i = 0; i < 3; i++)\n\t{\n\t\tprint(i);\n\t}\n}\n";
        assert_eq!(edited_lines(text, at(2, 10)), vec![2, 2, 2, 4]);
        assert_eq!(edited_lines(text, at(8, 8)), vec![6, 6, 6, 8]);
    }

    #[test]
    fn a_block_local_does_not_reach_the_sibling_block() {
        let text = "main()\n{\n\tif (1)\n\t{\n\t\tnew x = 1;\n\t\tprint(x);\n\t}\n\telse\n\t{\n\t\tnew x = 2;\n\t\tprint(x);\n\t}\n}\n";
        assert_eq!(edited_lines(text, at(4, 6)), vec![4, 5]);
        assert_eq!(edited_lines(text, at(10, 8)), vec![9, 10]);
    }

    #[test]
    fn a_parameter_is_renamed_only_in_its_function() {
        // O outro `n` é de outra função: renomear por nome no arquivo inteiro o
        // alteraria também.
        let text = "stock a(n)\n{\n\treturn n;\n}\n\nstock b(n)\n{\n\treturn n;\n}\n";
        assert_eq!(edited_lines(text, at(0, 8)), vec![0, 2]);
    }

    #[test]
    fn a_global_with_a_homonym_parameter_is_refused() {
        let st = state_with("new gCount;\n\nstock Use(gCount)\n{\n\treturn gCount;\n}\n");
        let reason = get_rename(&st, URI, at(0, 5), "novo").expect_err("recusa");
        assert!(
            reason.contains("gCount") && reason.contains(":3"),
            "{reason}"
        );
    }

    #[test]
    fn a_global_with_a_homonym_local_is_refused() {
        let st = state_with("new gCount;\n\nmain()\n{\n\tnew gCount = 1;\n}\n");
        let reason = get_rename(&st, URI, at(0, 5), "novo").expect_err("recusa");
        assert!(reason.contains(":5"), "{reason}");
    }

    #[test]
    fn a_global_without_homonyms_is_renamed() {
        let text = "new gCount;\n\nmain()\n{\n\tgCount = 1;\n}\n";
        assert_eq!(edited_lines(text, at(0, 5)), vec![0, 4]);
    }

    #[test]
    fn a_function_with_a_homonym_parameter_is_not_refused() {
        // Uma função só é renomeada onde aparece com `(`: o parâmetro homônimo
        // não é tocado, e não há por que recusar.
        let text = "stock Count() {}\n\nstock Use(Count)\n{\n\tCount();\n}\n";
        assert_eq!(edited_lines(text, at(0, 7)), vec![0, 4]);
    }

    #[test]
    fn a_public_is_also_renamed_in_settimer() {
        // `SetTimer` chama a `public` pelo nome em texto; `print("Tick(")` é só
        // texto, e fica como está.
        let text = "forward Tick();\npublic Tick()\n{\n}\n\nmain()\n{\n\tSetTimer(\"Tick\", 1000, true);\n\tprint(\"Tick(\");\n}\n";
        let st = state_with(text);
        let refs = get_references(&st, URI, at(1, 8));
        let places: Vec<(u32, u32)> = refs
            .iter()
            .map(|l| (l.range.start.line, l.range.start.character))
            .collect();
        assert!(places.contains(&(7, 11)), "{places:?}");
        assert!(!places.iter().any(|(line, _)| *line == 8), "{places:?}");
    }
}
