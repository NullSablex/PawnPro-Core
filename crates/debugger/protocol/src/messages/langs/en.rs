//! Inglês (en) — idioma-fonte e fallback dos demais.

use crate::messages::MsgKey;

/// One line per `MsgKey`. `{}` markers are positional (filled by `messages::format`).
#[allow(clippy::match_same_arms)]
#[must_use]
pub fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "division by zero",
        MsgKey::Bounds => "array index out of bounds",
        MsgKey::StackError => "stack overflow (stack/heap collision)",
        MsgKey::HeapLow => "heap underflow",
        MsgKey::MemAccess => "invalid memory access",
        MsgKey::RuntimeErrorsLabel => "Runtime errors",
        MsgKey::PluginVersaoDiferente => {
            "Debug plugin {} with adapter {}. Update the server plugin to {}."
        }
        MsgKey::InvalidValue => {
            "invalid value: '{}' (use an integer, e.g. 100/0x64; a float, e.g. 1.5; or true/false)"
        }
        MsgKey::InvalidElement => "invalid element: '{}'",
        MsgKey::ArrayEditElement => "'{}' is an array; expand it and edit an element (e.g. {}[0])",
        MsgKey::EmptyExpression => "empty expression",
        MsgKey::CannotEvaluate => "could not evaluate '{}'",
    }
}
