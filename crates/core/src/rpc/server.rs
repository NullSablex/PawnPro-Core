//! Os métodos do servidor que dependem da configuração do projeto aberto.
//!
//! Ficam fora do `handlers`, onde tudo é função pura dos parâmetros: resolver o
//! servidor lê a configuração que o core possui, em vez de recebê-la da
//! extensão — duas cópias da configuração podem discordar.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::config::service::ConfigService;
use crate::server::config::resolve_server_config;

use super::protocol::ResponseError;

/// Os métodos deste módulo.
pub const METHODS: [&str; 1] = ["server.resolve"];

/// Executa um método do servidor.
///
/// # Errors
/// [`ResponseError`] quando o método não é daqui, falta `workspaceRoot` ou
/// nenhum projeto foi aberto.
pub fn dispatch(
    method: &str,
    params: &Value,
    config: &ConfigService,
) -> Result<Value, ResponseError> {
    match method {
        // Executável, pasta, argumentos e log do servidor, com o que a
        // configuração não fixa descoberto no disco.
        "server.resolve" => {
            let root = params
                .get("workspaceRoot")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| ResponseError::invalid_params("falta `workspaceRoot`"))?;
            let server = config.read(|m| m.get_all().server.clone()).ok_or_else(|| {
                ResponseError::invalid_params("nenhum projeto aberto — chame `config.open` antes")
            })?;
            let resolved = resolve_server_config(&server, &root);
            let text = |path: Option<PathBuf>| {
                path.map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            Ok(json!({
                "exe": text(resolved.exe),
                "cwd": resolved.cwd.to_string_lossy(),
                "args": resolved.args,
                "clearOnStart": resolved.clear_on_start,
                "logPath": text(resolved.log_path),
                "logEncoding": resolved.log_encoding,
                "follow": resolved.follow,
            }))
        }
        _ => Err(ResponseError::method_not_found(method)),
    }
}
