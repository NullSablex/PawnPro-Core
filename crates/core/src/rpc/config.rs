//! Os métodos da configuração.
//!
//! O core é o único leitor e o único escritor dos `config.json`: a extensão
//! pergunta e pede. Toda resposta traz a configuração como ficou — quem gravou
//! não precisa reler para saber o que passou a valer.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::config::naming_lists;
use crate::config::service::ConfigService;
use crate::config::{ConfigError, ConfigManager, Scope};

use super::protocol::ResponseError;

/// Os métodos deste módulo.
pub const METHODS: [&str; 8] = [
    "config.open",
    "config.get",
    "config.set",
    "config.delete",
    "config.reload",
    "config.ensureNamingFiles",
    "config.backupNaming",
    "config.migrateNaming",
];

/// Em qual arquivo gravar.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ScopeParam {
    Global,
    Project,
}

impl From<ScopeParam> for Scope {
    fn from(scope: ScopeParam) -> Self {
        match scope {
            ScopeParam::Global => Self::Global,
            ScopeParam::Project => Self::Project,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Entry {
    key: String,
    value: Value,
}

/// Parâmetros de `config.set`: várias chaves, uma escrita.
#[derive(Debug, Deserialize)]
struct SetParams {
    entries: Vec<Entry>,
    scope: ScopeParam,
}

#[derive(Debug, Deserialize)]
struct DeleteParams {
    key: String,
    scope: ScopeParam,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenParams {
    workspace_root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct BackupParams {
    path: PathBuf,
}

/// Executa um método da configuração.
///
/// # Errors
/// [`ResponseError`] quando o método não é daqui, os parâmetros não servem,
/// nenhum projeto foi aberto ou a escrita falha.
pub fn dispatch(
    method: &str,
    params: &Value,
    config: &Arc<ConfigService>,
) -> Result<Value, ResponseError> {
    match method {
        "config.open" => {
            let OpenParams { workspace_root } = parse(params)?;
            config.open(&workspace_root);
            snapshot(config)
        }
        "config.get" => snapshot(config),
        "config.set" => {
            let SetParams { entries, scope } = parse(params)?;
            let entries: Vec<(String, Value)> =
                entries.into_iter().map(|e| (e.key, e.value)).collect();
            write(config, |m| m.set_keys(&entries, scope.into()))
        }
        "config.delete" => {
            let DeleteParams { key, scope } = parse(params)?;
            write(config, |m| m.delete_key(&key, scope.into()))
        }
        // Para quando a extensão mexeu no arquivo por fora e não quer esperar
        // o observador.
        "config.reload" => {
            config.reload();
            snapshot(config)
        }
        "config.ensureNamingFiles" => {
            config
                .read(naming_lists::ensure_naming_files)
                .ok_or_else(not_open)?;
            snapshot(config)
        }
        // O caminho vem da extensão, que decide o nome do arquivo que o
        // usuário vai conferir.
        "config.backupNaming" => {
            let BackupParams { path } = parse(params)?;
            let saved = config
                .read(|m| naming_lists::backup_naming_lists(m, &path))
                .ok_or_else(not_open)?;
            Ok(json!({ "path": saved }))
        }
        "config.migrateNaming" => {
            let moved = config
                .update(naming_lists::migrate_naming_lists)
                .ok_or_else(not_open)?
                .map_err(|e| config_error(&e))?;
            Ok(json!({ "moved": moved, "snapshot": snapshot(config)? }))
        }
        _ => Err(ResponseError::method_not_found(method)),
    }
}

fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, ResponseError> {
    serde_json::from_value(params.clone())
        .map_err(|e| ResponseError::invalid_params(&e.to_string()))
}

fn not_open() -> ResponseError {
    ResponseError::invalid_params("nenhum projeto aberto — chame `config.open` antes")
}

fn config_error(error: &ConfigError) -> ResponseError {
    match error {
        ConfigError::InvalidKey { .. } => ResponseError::invalid_params(&error.to_string()),
        ConfigError::Io(_) => ResponseError::internal(&error.to_string()),
    }
}

fn snapshot(config: &ConfigService) -> Result<Value, ResponseError> {
    let snapshot = config.snapshot().ok_or_else(not_open)?;
    serde_json::to_value(snapshot).map_err(|e| ResponseError::internal(&e.to_string()))
}

/// Aplica uma escrita e devolve a configuração como ficou.
fn write(
    config: &ConfigService,
    f: impl FnOnce(&mut ConfigManager) -> Result<(), ConfigError>,
) -> Result<Value, ResponseError> {
    config
        .update(f)
        .ok_or_else(not_open)?
        .map_err(|e| config_error(&e))?;
    snapshot(config)
}
