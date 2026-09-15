//! Os métodos de diagnóstico.
//!
//! A extensão não escreve o arquivo por conta própria quando o core está de pé:
//! manda o evento para cá. Assim a ordem das linhas é a ordem real dos fatos,
//! e há um só dono do arquivo, da rotação e do nível em vigor.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::config::manager::ConfigManager;
use crate::diagnostics::{self, Level};

use super::protocol::ResponseError;

/// Os métodos deste módulo.
pub const METHODS: [&str; 4] = ["log.configure", "log.write", "log.path", "log.clear"];

/// Executa um método de diagnóstico.
///
/// # Errors
/// [`ResponseError`] quando o método não é daqui ou os parâmetros não servem.
pub fn dispatch(method: &str, params: &Value) -> Result<Value, ResponseError> {
    match method {
        // Liga o registro para um projeto. Sem `level`, vale o que estiver na
        // configuração dele — é como a extensão aplica a escolha do usuário.
        "log.configure" => {
            let root = root_of(params)?;
            let level = match params.get("level").and_then(Value::as_str) {
                Some(name) => Level::from_name(name),
                None => level_from_config(&root),
            };
            diagnostics::configure(&root, level);
            if level != Level::Off {
                // Os logs são do diagnóstico de quem roda, não do repositório.
                diagnostics::ignore_logs(&root);
            }
            Ok(json!({ "level": level.label().to_ascii_lowercase() }))
        }
        "log.write" => {
            let level = Level::from_name(
                params
                    .get("level")
                    .and_then(Value::as_str)
                    .unwrap_or("info"),
            );
            let source = params
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("extension");
            let message = params
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| ResponseError::invalid_params("falta `message`"))?;
            diagnostics::write(level, source, message);
            Ok(json!(true))
        }
        "log.path" => {
            let root = root_of(params)?;
            Ok(json!({
                "path": diagnostics::log_path(&root),
                "level": diagnostics::level().label().to_ascii_lowercase(),
            }))
        }
        "log.clear" => {
            let root = root_of(params)?;
            diagnostics::clear(&root).map_err(|e| ResponseError::internal(&e.to_string()))?;
            Ok(json!(true))
        }
        _ => Err(ResponseError::method_not_found(method)),
    }
}

/// A pasta do projeto, que é onde o log vive.
fn root_of(params: &Value) -> Result<PathBuf, ResponseError> {
    params
        .get("workspaceRoot")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| ResponseError::invalid_params("falta `workspaceRoot`"))
}

/// O nível escrito no `config.json` do projeto.
fn level_from_config(root: &Path) -> Level {
    let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    else {
        return Level::Off;
    };
    let config = ConfigManager::new(root, &home);
    Level::from_name(&config.get_all().diagnostics.level)
}
