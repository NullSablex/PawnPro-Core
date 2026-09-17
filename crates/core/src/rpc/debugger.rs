//! Os métodos que dependem do depurador hospedado.

use serde_json::{Value, json};

use crate::supervisor::debugger::DebuggerService;

use super::Sender;
use super::protocol::ResponseError;

/// Os métodos deste módulo.
///
/// `debug.preflight` não está aqui: é função pura dos parâmetros e fica no
/// `handlers`.
pub const METHODS: [&str; 1] = ["debug.start"];

/// Executa um método do depurador.
///
/// # Errors
/// [`ResponseError`] quando o método não é daqui ou o sistema recusa o soquete.
pub fn dispatch(
    method: &str,
    sender: &Sender,
    debugger: &DebuggerService,
) -> Result<Value, ResponseError> {
    match method {
        // Idempotente: a extensão pede a cada sessão sem coordenar.
        "debug.start" => {
            let address = debugger
                .start(sender)
                .map_err(|e| ResponseError::internal(&e.to_string()))?;
            Ok(json!({ "address": address }))
        }
        _ => Err(ResponseError::method_not_found(method)),
    }
}
