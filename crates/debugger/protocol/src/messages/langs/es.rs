//! Español (es). Preservar los marcadores `{}` en la misma posición lógica que
//! en el original.

use crate::messages::MsgKey;

#[allow(clippy::match_same_arms)]
#[must_use]
pub fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "división por cero",
        MsgKey::Bounds => "índice de matriz fuera de límite",
        MsgKey::StackError => "desbordamiento de pila (colisión pila/montículo)",
        MsgKey::HeapLow => "subdesbordamiento del montículo",
        MsgKey::MemAccess => "acceso inválido a memoria",
        MsgKey::RuntimeErrorsLabel => "Errores de runtime",
        MsgKey::PluginVersaoDiferente => {
            "Plugin de depuración {} con adaptador {}. Actualiza el plugin del servidor a {}."
        }
        MsgKey::InvalidValue => {
            "valor inválido: '{}' (use un entero, ej.: 100/0x64; un float, ej.: 1.5; o true/false)"
        }
        MsgKey::InvalidElement => "elemento inválido: '{}'",
        MsgKey::ArrayEditElement => "'{}' es un array; expándalo y edite un elemento (ej.: {}[0])",
        MsgKey::EmptyExpression => "expresión vacía",
        MsgKey::CannotEvaluate => "no se pudo evaluar '{}'",
    }
}
