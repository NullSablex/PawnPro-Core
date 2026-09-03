//! Leitura do `CHANGELOG.md`.
//!
//! Extrai a seção da versão instalada. Converter o Markdown em HTML fica na
//! extensão: o resultado só existe dentro de uma `WebView`.

use std::path::Path;

/// Extrai a seção de uma versão.
///
/// Formato Keep a Changelog: cada versão abre com `## [1.2.3] - data`. Vazio
/// quando o arquivo não existe ou a versão não está lá.
#[must_use]
pub fn extract_section(changelog_path: &Path, version: &str) -> String {
    let Ok(raw) = std::fs::read_to_string(changelog_path) else {
        return String::new();
    };
    extract_section_from(&raw, version)
}

/// A mesma extração, sobre o texto já lido.
#[must_use]
pub fn extract_section_from(raw: &str, version: &str) -> String {
    let mut section = Vec::new();
    let mut inside = false;

    for line in raw.lines() {
        let Some(bracket) = version_heading(line) else {
            if inside {
                section.push(line);
            }
            continue;
        };
        // Chegou ao próximo cabeçalho de versão: a seção acabou.
        if inside {
            break;
        }
        inside = matches_version(bracket, version);
    }

    section.join("\n").trim().to_string()
}

/// O texto entre colchetes de um cabeçalho `## [versão]`, se a linha for um.
fn version_heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("##")?.trim_start();
    let rest = rest.strip_prefix('[')?;
    rest.split(']').next()
}

/// Aceita sufixos: a versão instalada pode ser `3.5.0` e o changelog trazer
/// `3.5.0-beta.1`. Exigir igualdade exata esvaziaria a página.
fn matches_version(heading: &str, version: &str) -> bool {
    heading == version
        || heading
            .strip_prefix(version)
            .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGELOG: &str = "\
# Changelog

## [3.5.0] - 2026-09-01

### Adicionado
- Recurso novo
- Outro recurso

### Corrigido
- Um bug

## [3.4.0] - 2026-08-01

### Adicionado
- Recurso antigo
";

    #[test]
    fn extracts_only_the_requested_version() {
        let section = extract_section_from(CHANGELOG, "3.5.0");
        assert!(section.contains("Recurso novo"));
        assert!(section.contains("Um bug"));
        // A seção termina no próximo cabeçalho de versão.
        assert!(!section.contains("Recurso antigo"));
        assert!(!section.contains("3.4.0"));
    }

    #[test]
    fn the_heading_itself_is_not_part_of_the_section() {
        // A página mostra o título por conta própria.
        let section = extract_section_from(CHANGELOG, "3.5.0");
        assert!(!section.contains("## [3.5.0]"));
        assert!(section.starts_with("### Adicionado"));
    }

    #[test]
    fn a_prerelease_suffix_matches_the_same_section() {
        // A versão instalada é `3.5.0`, o changelog traz `3.5.0-beta.1`:
        // exigir igualdade exata deixaria a página vazia.
        let raw = "## [3.5.0-beta.1] - 2026-09-01\n\n- Novidade\n";
        assert!(extract_section_from(raw, "3.5.0").contains("Novidade"));
    }

    #[test]
    fn a_patch_suffix_also_matches() {
        let raw = "## [3.5.0.1] - 2026-09-01\n\n- Correção\n";
        assert!(extract_section_from(raw, "3.5.0").contains("Correção"));
    }

    #[test]
    fn a_different_version_does_not_match_by_prefix() {
        // `3.5.0` não pode casar com `3.5.01`: são versões distintas.
        let raw = "## [3.50.0] - 2026-09-01\n\n- Outra\n";
        assert!(extract_section_from(raw, "3.5.0").is_empty());
    }

    #[test]
    fn an_absent_version_yields_nothing() {
        assert!(extract_section_from(CHANGELOG, "9.9.9").is_empty());
    }

    #[test]
    fn the_last_section_runs_to_the_end_of_the_file() {
        let section = extract_section_from(CHANGELOG, "3.4.0");
        assert!(section.contains("Recurso antigo"));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        // Não ter changelog não vale interromper a extensão.
        assert!(extract_section(Path::new("/nao/existe/CHANGELOG.md"), "1.0.0").is_empty());
    }

    #[test]
    fn other_heading_levels_do_not_close_the_section() {
        // `###` é subtítulo dentro da versão, não o fim dela.
        let section = extract_section_from(CHANGELOG, "3.5.0");
        assert!(section.contains("### Adicionado"));
        assert!(section.contains("### Corrigido"));
    }
}

/// Teste contra o `CHANGELOG.md` real da extensão.
///
/// Roda só com `PAWNPRO_EXTENSION_DIR` apontando para o repositório dela.
#[cfg(test)]
mod real_files {
    use super::*;

    #[test]
    fn the_shipped_changelog_has_the_current_version() {
        let Ok(dir) = std::env::var("PAWNPRO_EXTENSION_DIR") else {
            return;
        };
        let path = Path::new(&dir).join("CHANGELOG.md");
        let raw = std::fs::read_to_string(&path).expect("ler changelog");

        // A primeira versão listada é a mais recente: se ela não extrai, o
        // formato do arquivo mudou e a página ficaria vazia.
        let first = raw
            .lines()
            .find_map(version_heading)
            .expect("changelog tem ao menos uma versão");
        let section = extract_section_from(&raw, first);
        assert!(!section.is_empty(), "a seção de {first} saiu vazia");
    }
}
