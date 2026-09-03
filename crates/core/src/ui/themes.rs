//! Esquemas de realce de sintaxe do Pawn.
//!
//! Escolhe o esquema, lê o arquivo dele e funde as regras `TextMate` com as que
//! já existem, sem apagar as de outras linguagens.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Tema em uso no editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeKind {
    Dark,
    Light,
    HighContrast,
}

/// Esquema de cores que o usuário pode escolher.
///
/// Fechado: um valor livre viraria um caminho de arquivo inexistente.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    /// Segue o tema do editor.
    Auto,
    ClassicWhite,
    ModernWhite,
    ClassicDark,
    ModernDark,
    /// Sem realce próprio: as regras do Pawn são removidas.
    ///
    /// É o padrão da configuração — a extensão não mexe nas cores do editor
    /// sem o usuário pedir.
    #[default]
    None,
}

/// Uma regra `TextMate`: escopos e as cores que valem neles.
///
/// Sem `Eq`: `settings` guarda o JSON como veio, e as regras que chegam aqui
/// são as da configuração do usuário — de qualquer linguagem, com qualquer
/// valor. Um número ali é `f64`, que não tem igualdade total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenColorRule {
    /// O editor aceita as duas formas; preservá-las evita reescrever regras
    /// de terceiros num formato diferente.
    pub scope: Scopes,
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// O campo `scope` de uma regra, que o editor aceita como texto ou lista.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scopes {
    One(String),
    Many(Vec<String>),
}

impl Scopes {
    /// Os escopos como fatia, seja qual for a forma original.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        match self {
            Self::One(s) => std::slice::from_ref(s),
            Self::Many(v) => v,
        }
    }

    /// Os escopos ordenados, para comparar duas regras sem depender da ordem.
    fn sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.as_slice().iter().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

/// Um arquivo de esquema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenColorScheme {
    pub text_mate_rules: Vec<TokenColorRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_rules: Option<serde_json::Map<String, serde_json::Value>>,
}

impl Scheme {
    /// Todos os esquemas na ordem em que aparecem na página de configurações.
    pub const ALL: [Self; 6] = [
        Self::Auto,
        Self::ClassicWhite,
        Self::ModernWhite,
        Self::ClassicDark,
        Self::ModernDark,
        Self::None,
    ];

    /// Valor gravado na configuração.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ClassicWhite => "classic_white",
            Self::ModernWhite => "modern_white",
            Self::ClassicDark => "classic_dark",
            Self::ModernDark => "modern_dark",
            Self::None => "none",
        }
    }

    /// Rótulo exibido ao usuário.
    ///
    /// Junto da variante, e não numa tabela paralela: separados, um esquema
    /// novo passaria sem rótulo.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automático",
            Self::ClassicWhite => "Clássico (Claro)",
            Self::ModernWhite => "Moderno (Claro)",
            Self::ClassicDark => "Clássico (Escuro)",
            Self::ModernDark => "Moderno (Escuro)",
            Self::None => "Nenhum",
        }
    }

    /// Caminho relativo à pasta da extensão.
    ///
    /// `Auto` resolve para outro esquema e `None` remove as regras: nenhum dos
    /// dois tem arquivo.
    #[must_use]
    pub const fn file(self) -> Option<&'static str> {
        match self {
            Self::Auto | Self::None => None,
            Self::ClassicWhite => Some("syntaxes/themes/classic_white.json"),
            Self::ModernWhite => Some("syntaxes/themes/modern_white.json"),
            Self::ClassicDark => Some("syntaxes/themes/classic_dark.json"),
            Self::ModernDark => Some("syntaxes/themes/modern_dark.json"),
        }
    }

    /// Reconhece o valor gravado na configuração.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// Alto contraste vai com o escuro: os dois fundos pedem as mesmas cores.
#[must_use]
pub const fn pick_auto_scheme(theme: ThemeKind) -> Scheme {
    match theme {
        ThemeKind::Dark | ThemeKind::HighContrast => Scheme::ClassicDark,
        ThemeKind::Light => Scheme::ClassicWhite,
    }
}

/// Lê o arquivo de um esquema.
///
/// `None` se não há arquivo, ou se ele está ausente ou malformado: o realce é
/// acessório e não vale interromper a ativação.
#[must_use]
pub fn read_scheme_from_file(extension_dir: &Path, scheme: Scheme) -> Option<TokenColorScheme> {
    let raw = fs::read_to_string(extension_dir.join(scheme.file()?)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// `true` se um escopo pertence ao realce do Pawn.
fn is_pawn_scope(scope: &str) -> bool {
    // `source.pawn` já contém `.pawn`; basta o segundo teste.
    scope.contains(".pawn")
}

/// Compara ignorando a ordem dos escopos.
///
/// Evita regravar a configuração à toa, o que dispararia os watchers.
#[must_use]
pub fn same_pawn_rules(a: &[TokenColorRule], b: &[TokenColorRule]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.scope.sorted() == y.scope.sorted() && x.settings == y.settings)
}

/// Substitui as regras do Pawn, preservando as demais.
///
/// As regras vêm da configuração global do usuário, que pode ter cores de
/// outras linguagens.
#[must_use]
pub fn merge_token_colors(
    current: &[TokenColorRule],
    update: Option<&TokenColorScheme>,
) -> Vec<TokenColorRule> {
    let mut out: Vec<TokenColorRule> = current
        .iter()
        .filter(|r| !r.scope.as_slice().iter().any(|s| is_pawn_scope(s)))
        .cloned()
        .collect();
    if let Some(scheme) = update {
        out.extend(scheme.text_mate_rules.iter().cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(scopes: &[&str], color: &str) -> TokenColorRule {
        let mut settings = serde_json::Map::new();
        settings.insert("foreground".into(), serde_json::Value::String(color.into()));
        TokenColorRule {
            scope: if scopes.len() == 1 {
                Scopes::One(scopes[0].into())
            } else {
                Scopes::Many(scopes.iter().map(|s| (*s).to_string()).collect())
            },
            settings,
        }
    }

    #[test]
    fn every_scheme_has_key_and_label() {
        for s in Scheme::ALL {
            assert!(!s.key().is_empty(), "{s:?}");
            assert!(!s.label().is_empty(), "{s:?}");
        }
    }

    #[test]
    fn keys_match_the_stored_configuration_values() {
        let keys: Vec<_> = Scheme::ALL.iter().map(|s| s.key()).collect();
        assert_eq!(
            keys,
            [
                "auto",
                "classic_white",
                "modern_white",
                "classic_dark",
                "modern_dark",
                "none"
            ]
        );
    }

    #[test]
    fn keys_round_trip() {
        for s in Scheme::ALL {
            assert_eq!(Scheme::from_key(s.key()), Some(s));
        }
        assert_eq!(Scheme::from_key("inexistente"), None);
    }

    #[test]
    fn only_concrete_schemes_have_a_file() {
        // `auto` resolve para outro esquema e `none` remove as regras: nenhum
        // dos dois tem arquivo próprio.
        assert!(Scheme::Auto.file().is_none());
        assert!(Scheme::None.file().is_none());
        for s in [
            Scheme::ClassicWhite,
            Scheme::ModernWhite,
            Scheme::ClassicDark,
            Scheme::ModernDark,
        ] {
            assert!(s.file().is_some(), "{s:?}");
        }
    }

    #[test]
    fn auto_follows_the_editor_theme() {
        assert_eq!(pick_auto_scheme(ThemeKind::Light), Scheme::ClassicWhite);
        assert_eq!(pick_auto_scheme(ThemeKind::Dark), Scheme::ClassicDark);
        // Alto contraste vai com o escuro: mesmo fundo, mesmas cores de token.
        assert_eq!(
            pick_auto_scheme(ThemeKind::HighContrast),
            Scheme::ClassicDark
        );
    }

    #[test]
    fn merge_keeps_rules_from_other_languages() {
        // As regras vêm da configuração global do usuário, que pode ter cores
        // de outras linguagens: apagá-las seria destrutivo.
        let current = vec![
            rule(&["source.rust"], "#ff0000"),
            rule(&["source.pawn"], "#00ff00"),
            rule(&["keyword.control.pawn"], "#0000ff"),
        ];
        let merged = merge_token_colors(&current, None);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].scope.as_slice(), ["source.rust"]);
    }

    #[test]
    fn merge_drops_pawn_rules_inside_multi_scope_lists() {
        let current = vec![rule(&["source.c", "source.pawn"], "#fff")];
        assert!(merge_token_colors(&current, None).is_empty());
    }

    #[test]
    fn merge_appends_the_new_scheme() {
        let current = vec![rule(&["source.rust"], "#ff0000")];
        let scheme = TokenColorScheme {
            text_mate_rules: vec![rule(&["source.pawn"], "#123456")],
            semantic_rules: None,
        };
        let merged = merge_token_colors(&current, Some(&scheme));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].scope.as_slice(), ["source.pawn"]);
    }

    #[test]
    fn same_rules_ignores_scope_order() {
        // Evita regravar a configuração — e disparar watchers — quando nada
        // mudou de verdade.
        let a = vec![rule(&["b.pawn", "a.pawn"], "#fff")];
        let b = vec![rule(&["a.pawn", "b.pawn"], "#fff")];
        assert!(same_pawn_rules(&a, &b));
    }

    #[test]
    fn same_rules_detects_a_changed_color() {
        let a = vec![rule(&["a.pawn"], "#fff")];
        let b = vec![rule(&["a.pawn"], "#000")];
        assert!(!same_pawn_rules(&a, &b));
    }

    #[test]
    fn same_rules_detects_different_lengths() {
        let a = vec![rule(&["a.pawn"], "#fff")];
        assert!(!same_pawn_rules(&a, &[]));
    }

    #[test]
    fn scheme_json_uses_the_editor_field_names() {
        // O arquivo é lido pelo editor: os nomes precisam bater exatamente.
        // `r##` porque o JSON traz `#` na cor, que fecharia um `r#` no meio.
        let json =
            r##"{"textMateRules":[{"scope":"source.pawn","settings":{"foreground":"#abc"}}]}"##;
        let scheme: TokenColorScheme = serde_json::from_str(json).expect("parse");
        assert_eq!(scheme.text_mate_rules.len(), 1);
        assert_eq!(scheme.text_mate_rules[0].scope.as_slice(), ["source.pawn"]);
    }

    #[test]
    fn scope_accepts_both_shapes() {
        // O editor aceita texto ou lista; preservar a forma original evita
        // reescrever regras de terceiros num formato diferente.
        let one: TokenColorRule =
            serde_json::from_str(r#"{"scope":"a.pawn","settings":{}}"#).expect("parse");
        let many: TokenColorRule =
            serde_json::from_str(r#"{"scope":["a.pawn","b.pawn"],"settings":{}}"#).expect("parse");
        assert_eq!(one.scope.as_slice().len(), 1);
        assert_eq!(many.scope.as_slice().len(), 2);
        assert_eq!(
            serde_json::to_string(&one.scope).expect("ser"),
            "\"a.pawn\""
        );
    }

    #[test]
    fn missing_scheme_file_is_not_an_error() {
        // O realce é acessório: um arquivo ausente não pode impedir a ativação.
        let dir = std::env::temp_dir().join("pawnpro-themes-inexistente");
        assert!(read_scheme_from_file(&dir, Scheme::ClassicDark).is_none());
        assert!(read_scheme_from_file(&dir, Scheme::Auto).is_none());
    }
}

/// Testes contra os arquivos reais da extensão.
///
/// Rodam só quando `PAWNPRO_EXTENSION_DIR` aponta para o repositório da
/// extensão: o core não depende dele para compilar, mas quando está por perto
/// vale conferir que os esquemas de verdade parseiam.
#[cfg(test)]
mod real_files {
    use super::*;

    #[test]
    fn shipped_schemes_parse() {
        let Ok(dir) = std::env::var("PAWNPRO_EXTENSION_DIR") else {
            return;
        };
        let dir = Path::new(&dir);
        for s in [
            Scheme::ClassicWhite,
            Scheme::ModernWhite,
            Scheme::ClassicDark,
            Scheme::ModernDark,
        ] {
            let scheme = read_scheme_from_file(dir, s)
                .unwrap_or_else(|| panic!("{s:?} não parseou em {}", dir.display()));
            assert!(!scheme.text_mate_rules.is_empty(), "{s:?} sem regras");
            // Todo escopo do arquivo precisa ser reconhecido como do Pawn —
            // senão o merge deixaria regras órfãs na configuração do usuário.
            for rule in &scheme.text_mate_rules {
                for scope in rule.scope.as_slice() {
                    assert!(is_pawn_scope(scope), "{s:?}: escopo alheio {scope}");
                }
            }
        }
    }
}
