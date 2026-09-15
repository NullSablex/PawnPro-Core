//! Localização das mensagens do debugger voltadas ao usuário (editor), num só
//! lugar compartilhado pelo plugin e pelo adaptador: um [`MsgKey`] por mensagem,
//! um módulo por idioma em [`langs`] com `get(MsgKey) -> &'static str`, e o
//! roteamento por [`Locale`] aqui. Os textos são **templates** com marcadores
//! `{}` (posicionais); [`format`] os preenche. Ver a cobertura em `docs/i18n.md`.

mod langs;

/// Idioma das mensagens. Resolvido por prefixo da tag do editor; desconhecidos
/// caem em inglês (idioma-fonte de fallback do código).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    PtBr,
    Es,
    Ru,
    Ro,
    #[default]
    En,
}

impl Locale {
    /// Resolve o `Locale` de uma tag de idioma (`pt-BR`, `es`, `ru`, …), pelo
    /// prefixo de duas letras. Desconhecido → inglês.
    #[must_use]
    pub fn from_tag(s: &str) -> Self {
        let s = s.to_ascii_lowercase();
        if s.starts_with("pt") {
            Self::PtBr
        } else if s.starts_with("es") {
            Self::Es
        } else if s.starts_with("ru") {
            Self::Ru
        } else if s.starts_with("ro") {
            Self::Ro
        } else {
            Self::En
        }
    }
}

/// Chave de uma mensagem localizável. Uma linha por chave em cada idioma
/// ([`langs`]), para localizar e manter com facilidade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKey {
    // --- Erros de runtime (detectados no plugin) ---
    DivideByZero,
    Bounds,
    StackError,
    HeapLow,
    MemAccess,
    // --- Mensagens do adaptador (respostas ao editor) ---
    RuntimeErrorsLabel,
    /// Plugin do servidor e adaptador em versões diferentes.
    PluginVersaoDiferente,
    InvalidValue,
    InvalidElement,
    ArrayEditElement,
    EmptyExpression,
    CannotEvaluate,
}

/// Template da mensagem `key` no idioma dado (com marcadores `{}` crus).
#[must_use]
pub fn msg(locale: Locale, key: MsgKey) -> &'static str {
    match locale {
        Locale::PtBr => langs::pt_br::get(key),
        Locale::Es => langs::es::get(key),
        Locale::Ru => langs::ru::get(key),
        Locale::Ro => langs::ro::get(key),
        Locale::En => langs::en::get(key),
    }
}

/// Mensagem `key` no idioma dado, com os `{}` preenchidos por `args` (posicional).
/// `{}` sem argumento vira vazio; argumentos sobrando são ignorados.
#[must_use]
pub fn format(locale: Locale, key: MsgKey, args: &[&str]) -> String {
    let template = msg(locale, key);
    let parts: Vec<&str> = template.split("{}").collect();
    let mut out = String::with_capacity(template.len());
    for (i, part) in parts.iter().enumerate() {
        out.push_str(part);
        if i + 1 < parts.len() {
            out.push_str(args.get(i).copied().unwrap_or(""));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_from_str_by_prefix() {
        assert_eq!(Locale::from_tag("pt-BR"), Locale::PtBr);
        assert_eq!(Locale::from_tag("ES"), Locale::Es);
        assert_eq!(Locale::from_tag("ru-RU"), Locale::Ru);
        assert_eq!(Locale::from_tag("ro"), Locale::Ro);
        assert_eq!(Locale::from_tag("en-US"), Locale::En);
        assert_eq!(Locale::from_tag("zh"), Locale::En);
        assert_eq!(Locale::default(), Locale::En);
    }

    #[test]
    fn format_fills_placeholders_positional() {
        // Sem args: template inalterado.
        assert_eq!(
            format(Locale::En, MsgKey::EmptyExpression, &[]),
            "empty expression"
        );
        // Um arg.
        assert_eq!(
            format(Locale::En, MsgKey::CannotEvaluate, &["x+"]),
            "could not evaluate 'x+'"
        );
        // Dois `{}` com o mesmo valor (nome repetido).
        assert_eq!(
            format(Locale::En, MsgKey::ArrayEditElement, &["arr", "arr"]),
            "'arr' is an array; expand it and edit an element (e.g. arr[0])"
        );
    }

    #[test]
    fn runtime_error_messages_localized() {
        assert_eq!(msg(Locale::PtBr, MsgKey::DivideByZero), "divisão por zero");
        assert_eq!(msg(Locale::En, MsgKey::DivideByZero), "division by zero");
        assert_eq!(msg(Locale::En, MsgKey::Bounds), "array index out of bounds");
        assert_eq!(msg(Locale::Ru, MsgKey::HeapLow), "переполнение кучи снизу");
    }
}
