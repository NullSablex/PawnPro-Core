//! Tradução da interface por idioma escolhido pelo usuário.
//!
//! Independente do `l10n` do editor, que fixa o idioma da extensão pelo do
//! editor e não pode ser trocado em runtime. A fonte de tradução são os mesmos
//! bundles `l10n/bundle.l10n.<lang>.json`: a chave é a string em português e o
//! valor é a tradução, então um único conjunto serve às notificações e às
//! páginas.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Idioma da interface.
///
/// `enum` e não string porque o conjunto é fechado — cada variante tem um
/// bundle, e um valor livre viraria um arquivo inexistente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum UiLocale {
    /// Língua-fonte das chaves: não tem bundle, as chaves já estão nela.
    #[serde(rename = "pt-BR")]
    #[default]
    PtBr,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "es")]
    Es,
    #[serde(rename = "ro")]
    Ro,
    #[serde(rename = "ru")]
    Ru,
}

impl UiLocale {
    /// Todos os idiomas suportados.
    pub const ALL: [Self; 5] = [Self::PtBr, Self::En, Self::Es, Self::Ro, Self::Ru];

    /// Tag do idioma, como aparece na configuração e no nome do bundle.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::PtBr => "pt-BR",
            Self::En => "en",
            Self::Es => "es",
            Self::Ro => "ro",
            Self::Ru => "ru",
        }
    }

    /// Reconhece uma tag de idioma pelo prefixo.
    ///
    /// `es-ES`, `es` e `ES` levam todos ao espanhol: o editor entrega a tag
    /// completa do sistema, e exigir correspondência exata deixaria a maioria
    /// dos usuários no idioma errado.
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        let t = tag.trim().to_lowercase();
        // O prefixo de duas letras é o que identifica a língua; `pt-BR` é o
        // único com região, e não há outra variante de português a distinguir.
        Self::ALL.into_iter().find(|l| {
            let prefix = &l.tag()[..2];
            t.starts_with(prefix)
        })
    }
}

/// Resolve o idioma efetivo da interface.
///
/// O escolhido na configuração tem prioridade; vazio ou não suportado cai no
/// idioma do editor; se nem esse for suportado, português — a língua em que as
/// chaves estão escritas.
#[must_use]
pub fn resolve_ui_locale(configured: &str, editor_lang: &str) -> UiLocale {
    UiLocale::from_tag(configured)
        .or_else(|| UiLocale::from_tag(editor_lang))
        .unwrap_or_default()
}

/// Um bundle carregado: chave em português para o texto traduzido.
type Bundle = HashMap<String, String>;

/// Bundles já lidos, por pasta e idioma.
type BundleCache = HashMap<(String, UiLocale), Bundle>;

/// Ler e parsear um bundle custa I/O e o resultado não muda enquanto a extensão
/// vive; cada página recriaria o mesmo mapa sem isto.
fn cache() -> &'static Mutex<BundleCache> {
    static CACHE: OnceLock<Mutex<BundleCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_bundle(extension_dir: &Path, locale: UiLocale) -> Bundle {
    // Português é a própria chave: não há bundle a carregar.
    if locale == UiLocale::PtBr {
        return HashMap::new();
    }
    let key = (extension_dir.to_string_lossy().into_owned(), locale);
    if let Ok(guard) = cache().lock()
        && let Some(hit) = guard.get(&key)
    {
        return hit.clone();
    }

    // Bundle ausente ou malformado devolve mapa vazio: sem tradução, cada
    // chave aparece em português, que é melhor que a página não abrir.
    let path = extension_dir
        .join("l10n")
        .join(format!("bundle.l10n.{}.json", locale.tag()));
    let bundle: Bundle = fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();

    if let Ok(mut guard) = cache().lock() {
        guard.insert(key, bundle.clone());
    }
    bundle
}

/// Traduz chaves em português para o idioma escolhido.
pub struct UiTranslator {
    bundle: Bundle,
}

impl UiTranslator {
    /// Carrega o bundle do idioma.
    #[must_use]
    pub fn new(extension_dir: &Path, locale: UiLocale) -> Self {
        Self {
            bundle: load_bundle(extension_dir, locale),
        }
    }

    /// Traduz, substituindo `{0}`, `{1}`… pelos argumentos.
    ///
    /// Uma chave sem tradução volta como está: é a string em português, que é
    /// exatamente o texto-fonte.
    #[must_use]
    pub fn t(&self, pt_key: &str, args: &[&str]) -> String {
        let template = self.bundle.get(pt_key).map_or(pt_key, String::as_str);
        apply_args(template, args)
    }
}

/// Substitui os marcadores posicionais do template.
///
/// Um índice sem argumento correspondente fica como está — perder o marcador
/// esconderia o erro de quem chamou.
fn apply_args(template: &str, args: &[&str]) -> String {
    if args.is_empty() || !template.contains('{') {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // Chave sem fechamento: daqui em diante é tudo texto literal.
            out.push('{');
            rest = after;
            break;
        };
        let inside = &after[..close];
        if let Some(v) = inside.parse::<usize>().ok().and_then(|i| args.get(i)) {
            out.push_str(v);
        } else {
            // Índice sem argumento, ou texto que não é índice: fica como está —
            // apagá-lo esconderia o erro de quem chamou.
            out.push('{');
            out.push_str(inside);
            out.push('}');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_match_the_bundle_file_names() {
        let tags: Vec<_> = UiLocale::ALL.iter().map(|l| l.tag()).collect();
        assert_eq!(tags, ["pt-BR", "en", "es", "ro", "ru"]);
    }

    #[test]
    fn region_variants_map_to_the_language() {
        // O editor entrega a tag completa do sistema; exigir correspondência
        // exata deixaria a maioria dos usuários no idioma errado.
        assert_eq!(UiLocale::from_tag("es-ES"), Some(UiLocale::Es));
        assert_eq!(UiLocale::from_tag("pt"), Some(UiLocale::PtBr));
        assert_eq!(UiLocale::from_tag("pt-PT"), Some(UiLocale::PtBr));
        assert_eq!(UiLocale::from_tag("en-US"), Some(UiLocale::En));
        assert_eq!(UiLocale::from_tag("EN"), Some(UiLocale::En));
        assert_eq!(UiLocale::from_tag(" ru-RU "), Some(UiLocale::Ru));
    }

    #[test]
    fn unsupported_tags_are_rejected() {
        for tag in ["", "de", "fr-FR", "zh"] {
            assert_eq!(UiLocale::from_tag(tag), None, "{tag}");
        }
    }

    #[test]
    fn configuration_wins_over_the_editor_language() {
        assert_eq!(resolve_ui_locale("ru", "en-US"), UiLocale::Ru);
    }

    #[test]
    fn empty_configuration_falls_back_to_the_editor() {
        assert_eq!(resolve_ui_locale("", "es-ES"), UiLocale::Es);
    }

    #[test]
    fn unsupported_configuration_falls_back_to_the_editor() {
        assert_eq!(resolve_ui_locale("de", "ro"), UiLocale::Ro);
    }

    #[test]
    fn portuguese_is_the_last_resort() {
        // As chaves já estão em português: é o fallback que sempre funciona.
        assert_eq!(resolve_ui_locale("", ""), UiLocale::PtBr);
        assert_eq!(resolve_ui_locale("de", "fr"), UiLocale::PtBr);
    }

    #[test]
    fn portuguese_needs_no_bundle() {
        // A chave é o próprio texto-fonte.
        let dir = Path::new("/nao/existe");
        let t = UiTranslator::new(dir, UiLocale::PtBr);
        assert_eq!(t.t("Servidor iniciado", &[]), "Servidor iniciado");
    }

    #[test]
    fn missing_bundle_falls_back_to_the_key() {
        // Sem tradução, a página abre em português — melhor que não abrir.
        let dir = Path::new("/nao/existe");
        let t = UiTranslator::new(dir, UiLocale::En);
        assert_eq!(t.t("Servidor iniciado", &[]), "Servidor iniciado");
    }

    #[test]
    fn positional_arguments_are_substituted() {
        let t = UiTranslator {
            bundle: HashMap::new(),
        };
        assert_eq!(
            t.t("Porta {0} ocupada por {1}.", &["7777", "1234"]),
            "Porta 7777 ocupada por 1234."
        );
    }

    #[test]
    fn repeated_placeholders_all_get_replaced() {
        let t = UiTranslator {
            bundle: HashMap::new(),
        };
        assert_eq!(t.t("{0} e {0}", &["x"]), "x e x");
    }

    #[test]
    fn a_placeholder_without_an_argument_stays_put() {
        // Apagá-lo esconderia o erro de quem chamou.
        let t = UiTranslator {
            bundle: HashMap::new(),
        };
        assert_eq!(t.t("Porta {0} e {1}", &["7777"]), "Porta 7777 e {1}");
    }

    #[test]
    fn braces_that_are_not_placeholders_are_kept() {
        let t = UiTranslator {
            bundle: HashMap::new(),
        };
        assert_eq!(t.t("Use {chaves} assim", &["x"]), "Use {chaves} assim");
        assert_eq!(t.t("Sem fechar {0", &["x"]), "Sem fechar {0");
    }

    #[test]
    fn translation_replaces_the_key() {
        let mut bundle = HashMap::new();
        bundle.insert(
            "Servidor iniciado".to_string(),
            "Server started".to_string(),
        );
        let t = UiTranslator { bundle };
        assert_eq!(t.t("Servidor iniciado", &[]), "Server started");
    }
}

/// Testes contra os bundles reais da extensão.
///
/// Rodam só quando `PAWNPRO_EXTENSION_DIR` aponta para o repositório dela.
#[cfg(test)]
mod real_files {
    use super::*;

    #[test]
    fn shipped_bundles_load_and_translate() {
        let Ok(dir) = std::env::var("PAWNPRO_EXTENSION_DIR") else {
            return;
        };
        let dir = Path::new(&dir);
        for locale in [UiLocale::En, UiLocale::Es, UiLocale::Ro, UiLocale::Ru] {
            let t = UiTranslator::new(dir, locale);
            assert!(!t.bundle.is_empty(), "{} sem entradas", locale.tag());
            // A tradução tem de sair do bundle, e não da chave. Comparar com o
            // português não serve: "Servidor iniciado" é igual em espanhol.
            let key = "Servidor iniciado";
            let expected = t
                .bundle
                .get(key)
                .expect("chave presente em todos os bundles");
            assert_eq!(&t.t(key, &[]), expected, "{}", locale.tag());
            // E uma chave com marcador precisa interpolar no idioma traduzido.
            let with_arg = "A porta {0} está ocupada por outro programa.                             Configure outra porta ou libere essa.";
            assert!(
                t.t(with_arg, &["7777"]).contains("7777"),
                "{} não interpolou",
                locale.tag()
            );
        }
    }
}
