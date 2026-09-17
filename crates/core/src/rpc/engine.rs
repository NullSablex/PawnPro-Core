//! Os métodos que dependem da engine hospedada.
//!
//! Ficam fora do `handlers` porque precisam do estado do processo — o endereço
//! reservado, a thread supervisionada e o canal de configuração —, enquanto lá
//! tudo é função pura de parâmetros.
//!
//! A extensão não recebe mais configuração para repassar à engine: ela diz qual
//! projeto abriu, e o core entrega o resto direto.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::supervisor::engine::EngineService;

use super::Sender;
use super::protocol::ResponseError;

/// Os métodos deste módulo, na ordem em que a extensão costuma usá-los.
pub const METHODS: [&str; 1] = ["engine.start"];

/// Executa um método da engine.
///
/// # Errors
/// [`ResponseError`] quando o método não é daqui, os parâmetros não servem ou
/// o sistema recusa a thread.
pub fn dispatch(
    method: &str,
    params: &Value,
    sender: &Sender,
    engine: &EngineService,
) -> Result<Value, ResponseError> {
    match method {
        // Idempotente: a extensão pode pedir a cada janela sem coordenar.
        "engine.start" => {
            let root = params
                .get("workspaceRoot")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| ResponseError::invalid_params("falta `workspaceRoot`"))?;
            // Só a extensão conhece o idioma do editor; ausente, vale o que
            // estiver na configuração.
            let editor_language = params
                .get("editorLanguage")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let address = engine
                .start(sender, &root, editor_language)
                .map_err(|e| ResponseError::internal(&e.to_string()))?;
            Ok(json!({ "address": address }))
        }
        _ => Err(ResponseError::method_not_found(method)),
    }
}
