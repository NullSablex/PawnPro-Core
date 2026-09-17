//! Română (ro). Păstrați marcajele `{}` în aceeași poziție logică ca în original.

use crate::messages::MsgKey;

#[allow(clippy::match_same_arms)]
#[must_use]
pub const fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "împărțire la zero",
        MsgKey::Bounds => "index de matrice în afara limitelor",
        MsgKey::StackError => "depășire de stivă (coliziune stivă/heap)",
        MsgKey::HeapLow => "subdepășire de heap",
        MsgKey::MemAccess => "acces nevalid la memorie",
        MsgKey::RuntimeErrorsLabel => "Erori de runtime",
        MsgKey::PluginVersionMismatch => {
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
        MsgKey::WaitingForPlugin => "Se așteaptă ca serverul să încarce pluginul de depanare...",
        MsgKey::PluginConnected => "Conectat la pluginul de depanare.",
        MsgKey::PluginNotConnected => {
            "Pluginul de depanare nu s-a conectat în {} secunde. Verificați că serverul a pornit, că pluginul de depanare PawnPro este instalat și încărcat și că nu există alt plugin cu același nume."
        }
        MsgKey::ServerStartFailed => "Nu s-a putut porni serverul: {}",
        MsgKey::LaunchWithoutServer => {
            "Configurația de launch nu conține comanda serverului: depanarea PawnPro pornește serverul singură."
        }
        MsgKey::BreakpointNotCompiled => {
            "Această linie s-a modificat de la compilare și nu are cod în binarul care rulează. Reporniți depanarea pentru a recompila."
        }
        MsgKey::FrameLineChanged => "{} (linia {} modificată de la compilare)",
        MsgKey::PluginConnectFailed => {
            "[pawnpro-dbg] nu s-a putut conecta la PawnPro la {} (sesiunea {}): {}"
        }
        MsgKey::AmxWithoutDebugInfo => {
            "Fișierul .amx nu are informații de depanare ({}): breakpoint-urile și variabilele nu sunt disponibile. Compilați cu -d3 sau reporniți depanarea pentru a recompila."
        }
        MsgKey::VariableNotWritten => {
            "nu s-a putut scrie '{}': pauza s-a putut încheia sau variabila nu mai este accesibilă"
        }
    }
}
