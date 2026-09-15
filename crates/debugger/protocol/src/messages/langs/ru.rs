//! Русский (ru). Сохранять маркеры `{}` в той же логической позиции, что и в
//! оригинале.

use crate::messages::MsgKey;

#[allow(clippy::match_same_arms)]
#[must_use]
pub fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "деление на ноль",
        MsgKey::Bounds => "индекс массива вне диапазона",
        MsgKey::StackError => "переполнение стека (столкновение стека и кучи)",
        MsgKey::HeapLow => "переполнение кучи снизу",
        MsgKey::MemAccess => "недопустимый доступ к памяти",
        MsgKey::RuntimeErrorsLabel => "Ошибки времени выполнения",
        MsgKey::PluginVersaoDiferente => {
            "Плагин отладки {}, адаптер {}. Обновите плагин сервера до {}."
        }
        MsgKey::InvalidValue => {
            "недопустимое значение: '{}' (целое, напр. 100/0x64; дробное, напр. 1.5; или true/false)"
        }
        MsgKey::InvalidElement => "недопустимый элемент: '{}'",
        MsgKey::ArrayEditElement => {
            "'{}' — массив; разверните его и измените элемент (напр. {}[0])"
        }
        MsgKey::EmptyExpression => "пустое выражение",
        MsgKey::CannotEvaluate => "не удалось вычислить '{}'",
    }
}
