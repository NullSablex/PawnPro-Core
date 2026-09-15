//! Português (Brasil) (pt-BR). Preservar os marcadores `{}` na mesma posição
//! lógica do original.

use crate::messages::MsgKey;

#[allow(clippy::match_same_arms)]
#[must_use]
pub fn get(key: MsgKey) -> &'static str {
    match key {
        MsgKey::DivideByZero => "divisão por zero",
        MsgKey::Bounds => "índice de array fora do limite",
        MsgKey::StackError => "estouro de pilha (colisão pilha/heap)",
        MsgKey::HeapLow => "underflow de heap",
        MsgKey::MemAccess => "acesso inválido à memória",
        MsgKey::RuntimeErrorsLabel => "Erros de runtime",
        MsgKey::PluginVersaoDiferente => {
            "Plugin de depuração {} com adaptador {}. Atualize o plugin do servidor para {}."
        }
        MsgKey::InvalidValue => {
            "valor inválido: '{}' (use inteiro, ex.: 100/0x64; float, ex.: 1.5; ou true/false)"
        }
        MsgKey::InvalidElement => "elemento inválido: '{}'",
        MsgKey::ArrayEditElement => "'{}' é um array; expanda e edite um elemento (ex.: {}[0])",
        MsgKey::EmptyExpression => "expressão vazia",
        MsgKey::CannotEvaluate => "não foi possível avaliar '{}'",
    }
}
