//! Inglês (en) — idioma-fonte e fallback dos demais.

use crate::messages::MsgKey;

/// One line per `MsgKey`. `{}` markers are positional (filled by `messages::format`).
#[allow(clippy::match_same_arms)]
#[must_use]
pub const fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "division by zero",
        MsgKey::Bounds => "array index out of bounds",
        MsgKey::StackError => "stack overflow (stack/heap collision)",
        MsgKey::HeapLow => "heap underflow",
        MsgKey::MemAccess => "invalid memory access",
        MsgKey::RuntimeErrorsLabel => "Runtime errors",
        MsgKey::PluginVersionMismatch => {
            "Debug plugin {} with adapter {}. Update the server plugin to {}."
        }
        MsgKey::InvalidValue => {
            "invalid value: '{}' (use an integer, e.g. 100/0x64; a float, e.g. 1.5; or true/false)"
        }
        MsgKey::InvalidElement => "invalid element: '{}'",
        MsgKey::ArrayEditElement => "'{}' is an array; expand it and edit an element (e.g. {}[0])",
        MsgKey::EmptyExpression => "empty expression",
        MsgKey::CannotEvaluate => "could not evaluate '{}'",
        MsgKey::WaitingForPlugin => "Waiting for the server to load the debug plugin...",
        MsgKey::PluginConnected => "Connected to the debug plugin.",
        MsgKey::PluginNotConnected => {
            "The debug plugin did not connect within {} seconds. Check that the server started, that the PawnPro debug plugin is installed and loaded, and that no other plugin has the same name."
        }
        MsgKey::ServerStartFailed => "Failed to start the server: {}",
        MsgKey::LaunchWithoutServer => {
            "The launch configuration has no server command: PawnPro debugging starts the server itself."
        }
        MsgKey::BreakpointNotCompiled => {
            "This line changed since the last build and has no code in the running binary. Restart debugging to rebuild."
        }
        MsgKey::FrameLineChanged => "{} (line {} changed since the last build)",
        MsgKey::PluginConnectFailed => {
            "[pawnpro-dbg] could not connect to PawnPro at {} (session {}): {}"
        }
        MsgKey::AmxWithoutDebugInfo => {
            "The .amx has no debug information ({}): breakpoints and variables are unavailable. Compile with -d3, or restart debugging to rebuild."
        }
        MsgKey::VariableNotWritten => {
            "could not write '{}': the pause may have ended or the variable is no longer reachable"
        }
    }
}
