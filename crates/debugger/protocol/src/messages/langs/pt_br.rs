//! Português (Brasil) (pt-BR). Preservar os marcadores `{}` na mesma posição
//! lógica do original.

use crate::messages::MsgKey;

#[allow(clippy::match_same_arms)]
#[must_use]
pub const fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "divisão por zero",
        MsgKey::Bounds => "índice de array fora do limite",
        MsgKey::StackError => "estouro de pilha (colisão pilha/heap)",
        MsgKey::HeapLow => "underflow de heap",
        MsgKey::MemAccess => "acesso inválido à memória",
        MsgKey::RuntimeErrorsLabel => "Erros de runtime",
        MsgKey::PluginVersionMismatch => {
            "Plugin de depuração {} com adaptador {}. Atualize o plugin do servidor para {}."
        }
        MsgKey::InvalidValue => {
            "valor inválido: '{}' (use inteiro, ex.: 100/0x64; float, ex.: 1.5; ou true/false)"
        }
        MsgKey::InvalidElement => "elemento inválido: '{}'",
        MsgKey::ArrayEditElement => "'{}' é um array; expanda e edite um elemento (ex.: {}[0])",
        MsgKey::EmptyExpression => "expressão vazia",
        MsgKey::CannotEvaluate => "não foi possível avaliar '{}'",
        MsgKey::WaitingForPlugin => "Aguardando o servidor carregar o plugin de depuração...",
        MsgKey::PluginConnected => "Conectado ao plugin de depuração.",
        MsgKey::PluginNotConnected => {
            "O plugin de depuração não conectou em {} segundos. Verifique se o servidor subiu, se o plugin de depuração do PawnPro está instalado e carregou, e se não há outro plugin com o mesmo nome."
        }
        MsgKey::ServerStartFailed => "Falha ao iniciar o servidor: {}",
        MsgKey::LaunchWithoutServer => {
            "A configuração de launch não traz o comando do servidor: a depuração do PawnPro sobe o servidor por conta própria."
        }
        MsgKey::BreakpointNotCompiled => {
            "Esta linha mudou desde a compilação e não tem código no binário em execução. Reinicie a depuração para recompilar."
        }
        MsgKey::FrameLineChanged => "{} (linha {} alterada desde a compilação)",
        MsgKey::PluginConnectFailed => {
            "[pawnpro-dbg] falha ao conectar no PawnPro em {} (sessão {}): {}"
        }
        MsgKey::AmxWithoutDebugInfo => {
            "O .amx está sem informação de depuração ({}): breakpoints e variáveis ficam indisponíveis. Compile com -d3 ou reinicie a depuração para recompilar."
        }
        MsgKey::VariableNotWritten => {
            "não foi possível gravar '{}': a pausa pode ter acabado ou a variável não está mais acessível"
        }
    }
}
