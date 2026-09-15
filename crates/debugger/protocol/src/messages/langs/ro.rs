//! Română (ro). Păstrați marcajele `{}` în aceeași poziție logică ca în original.

use crate::messages::MsgKey;

#[allow(clippy::match_same_arms)]
#[must_use]
pub fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "împărțire la zero",
        MsgKey::Bounds => "index de matrice în afara limitelor",
        MsgKey::StackError => "depășire de stivă (coliziune stivă/heap)",
        MsgKey::HeapLow => "subdepășire de heap",
        MsgKey::MemAccess => "acces nevalid la memorie",
        MsgKey::RuntimeErrorsLabel => "Erori de runtime",
        MsgKey::PluginVersaoDiferente => {
            "Plugin de depanare {} cu adaptor {}. Actualizează pluginul serverului la {}."
        }
        MsgKey::InvalidValue => {
            "valoare invalidă: '{}' (folosiți un întreg, ex.: 100/0x64; un float, ex.: 1.5; sau true/false)"
        }
        MsgKey::InvalidElement => "element invalid: '{}'",
        MsgKey::ArrayEditElement => {
            "'{}' este un array; extindeți-l și editați un element (ex.: {}[0])"
        }
        MsgKey::EmptyExpression => "expresie goală",
        MsgKey::CannotEvaluate => "nu s-a putut evalua '{}'",
    }
}
