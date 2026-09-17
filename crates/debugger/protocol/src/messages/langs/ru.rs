//! Русский (ru). Сохранять маркеры `{}` в той же логической позиции, что и в
//! оригинале.

use crate::messages::MsgKey;

#[allow(clippy::match_same_arms)]
#[must_use]
pub const fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "деление на ноль",
        MsgKey::Bounds => "индекс массива вне диапазона",
        MsgKey::StackError => "переполнение стека (столкновение стека и кучи)",
        MsgKey::HeapLow => "переполнение кучи снизу",
        MsgKey::MemAccess => "недопустимый доступ к памяти",
        MsgKey::RuntimeErrorsLabel => "Ошибки времени выполнения",
        MsgKey::PluginVersionMismatch => {
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
        MsgKey::WaitingForPlugin => "Ожидание загрузки отладочного плагина сервером...",
        MsgKey::PluginConnected => "Подключено к отладочному плагину.",
        MsgKey::PluginNotConnected => {
            "Отладочный плагин не подключился за {} с. Проверьте, что сервер запущен, отладочный плагин PawnPro установлен и загружен, и нет другого плагина с тем же именем."
        }
        MsgKey::ServerStartFailed => "Не удалось запустить сервер: {}",
        MsgKey::LaunchWithoutServer => {
            "В конфигурации launch нет команды сервера: отладка PawnPro запускает сервер сама."
        }
        MsgKey::BreakpointNotCompiled => {
            "Эта строка изменилась после компиляции, и в запущенном бинарном файле для неё нет кода. Перезапустите отладку, чтобы перекомпилировать."
        }
        MsgKey::FrameLineChanged => "{} (строка {} изменена после компиляции)",
        MsgKey::PluginConnectFailed => {
            "[pawnpro-dbg] не удалось подключиться к PawnPro по адресу {} (сессия {}): {}"
        }
        MsgKey::AmxWithoutDebugInfo => {
            "В .amx нет отладочной информации ({}): точки останова и переменные недоступны. Скомпилируйте с -d3 или перезапустите отладку для перекомпиляции."
        }
        MsgKey::VariableNotWritten => {
            "не удалось записать '{}': пауза могла закончиться или переменная больше недоступна"
        }
    }
}
