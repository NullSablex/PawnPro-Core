//! Español (es). Preservar los marcadores `{}` en la misma posición lógica que
//! en el original.

use crate::messages::MsgKey;

#[allow(clippy::match_same_arms)]
#[must_use]
pub const fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "división por cero",
        MsgKey::Bounds => "índice de matriz fuera de límite",
        MsgKey::StackError => "desbordamiento de pila (colisión pila/montículo)",
        MsgKey::HeapLow => "subdesbordamiento del montículo",
        MsgKey::MemAccess => "acceso inválido a memoria",
        MsgKey::RuntimeErrorsLabel => "Errores de runtime",
        MsgKey::PluginVersionMismatch => {
            "Plugin de depuración {} con adaptador {}. Actualiza el plugin del servidor a {}."
        }
        MsgKey::InvalidValue => {
            "valor inválido: '{}' (use un entero, ej.: 100/0x64; un float, ej.: 1.5; o true/false)"
        }
        MsgKey::InvalidElement => "elemento inválido: '{}'",
        MsgKey::ArrayEditElement => "'{}' es un array; expándalo y edite un elemento (ej.: {}[0])",
        MsgKey::EmptyExpression => "expresión vacía",
        MsgKey::CannotEvaluate => "no se pudo evaluar '{}'",
        MsgKey::WaitingForPlugin => "Esperando a que el servidor cargue el plugin de depuración...",
        MsgKey::PluginConnected => "Conectado al plugin de depuración.",
        MsgKey::PluginNotConnected => {
            "El plugin de depuración no se conectó en {} segundos. Verifica que el servidor haya iniciado, que el plugin de depuración de PawnPro esté instalado y cargado, y que no haya otro plugin con el mismo nombre."
        }
        MsgKey::ServerStartFailed => "No se pudo iniciar el servidor: {}",
        MsgKey::LaunchWithoutServer => {
            "La configuración de launch no incluye el comando del servidor: la depuración de PawnPro inicia el servidor por sí misma."
        }
        MsgKey::BreakpointNotCompiled => {
            "Esta línea cambió desde la compilación y no tiene código en el binario en ejecución. Reinicia la depuración para recompilar."
        }
        MsgKey::FrameLineChanged => "{} (línea {} modificada desde la compilación)",
        MsgKey::PluginConnectFailed => {
            "[pawnpro-dbg] no se pudo conectar a PawnPro en {} (sesión {}): {}"
        }
        MsgKey::AmxWithoutDebugInfo => {
            "El .amx no tiene información de depuración ({}): los breakpoints y las variables no están disponibles. Compila con -d3 o reinicia la depuración para recompilar."
        }
        MsgKey::VariableNotWritten => {
            "no se pudo escribir '{}': la pausa pudo haber terminado o la variable ya no es accesible"
        }
    }
}
