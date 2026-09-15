//! Os métodos que a extensão pode chamar.
//!
//! A tradução de parâmetros acontece só aqui: o resto do crate não sabe que
//! existe RPC.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::types::ServerType;
use crate::server;

use super::protocol::ResponseError;

/// Tempo para o servidor salvar e desligar os componentes, sem deixar o
/// usuário esperando quando ele travou.
const KILL_TIMEOUT: Duration = Duration::from_secs(3);

/// Prazo de uma sondagem de porta.
const PING_TIMEOUT: Duration = Duration::from_millis(1200);

/// Extrai um campo obrigatório dos parâmetros.
fn field<'a>(params: &'a Value, name: &str) -> Result<&'a Value, ResponseError> {
    params
        .get(name)
        .ok_or_else(|| ResponseError::invalid_params(&format!("falta `{name}`")))
}

fn path_field(params: &Value, name: &str) -> Result<PathBuf, ResponseError> {
    field(params, name)?
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| ResponseError::invalid_params(&format!("`{name}` deve ser texto")))
}

fn u16_field(params: &Value, name: &str) -> Result<u16, ResponseError> {
    field(params, name)?
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .ok_or_else(|| ResponseError::invalid_params(&format!("`{name}` deve ser uma porta")))
}

fn str_field<'a>(params: &'a Value, name: &str) -> Result<&'a str, ResponseError> {
    field(params, name)?
        .as_str()
        .ok_or_else(|| ResponseError::invalid_params(&format!("`{name}` deve ser texto")))
}

/// Parâmetros de `rcon.send`.
#[derive(Debug, Deserialize)]
struct RconSendParams {
    host: String,
    port: u16,
    password: String,
    #[serde(default = "default_true")]
    enabled: bool,
    command: String,
    #[serde(default = "default_rcon_timeout")]
    timeout_ms: u64,
}

const fn default_true() -> bool {
    true
}

const fn default_rcon_timeout() -> u64 {
    1500
}

/// Executa um método e devolve o resultado em JSON.
///
/// # Errors
/// [`ResponseError`] quando o método não existe, os parâmetros não servem, ou
/// a operação falha.
pub fn dispatch(method: &str, params: &Value) -> Result<Value, ResponseError> {
    match method {
        // --- servidor: configuração ---
        "server.detectType" => {
            let cwd = path_field(params, "cwd")?;
            Ok(json!(server::config::detect_server_type(&cwd)))
        }
        "server.detectExecutable" => {
            let root = path_field(params, "workspaceRoot")?;
            Ok(json!(server::config::detect_server_executable(&root)))
        }
        "server.loadConfig" => {
            let cwd = path_field(params, "cwd")?;
            let server_type: ServerType = params
                .get("type")
                .map_or(Ok(ServerType::Auto), |v| serde_json::from_value(v.clone()))
                .map_err(|e| ResponseError::invalid_params(&e.to_string()))?;
            Ok(json!(server::config::load_server_config(&cwd, server_type)))
        }

        // --- servidor: processos ---
        "server.pidsOnPort" => {
            let port = u16_field(params, "port")?;
            Ok(json!(server::process::pids_on_port(port)))
        }
        "server.projectServersOnPort" => {
            let port = u16_field(params, "port")?;
            let exe = path_field(params, "exe")?;
            Ok(json!(server::process::project_servers_on_port(port, &exe)))
        }
        "server.kill" => {
            let pid = field(params, "pid")?
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| ResponseError::invalid_params("`pid` deve ser um número"))?;
            // O filtro de dono vale também por RPC: a extensão não contorna
            // a política pedindo direto.
            let exe = path_field(params, "exe")?;
            if !server::process::is_project_server(pid, &exe) {
                return Err(ResponseError::internal(
                    "o processo não é o servidor deste projeto",
                ));
            }
            Ok(json!(server::process::kill_process(pid, KILL_TIMEOUT)))
        }

        // --- servidor: RCON ---
        "server.ping" => {
            let host = str_field(params, "host")?;
            let port = u16_field(params, "port")?;
            let addr = crate::server::types::ServerAddr {
                host: host.to_string(),
                port,
            };
            server::rcon::ping(&addr, PING_TIMEOUT)
                .map(|alive| json!(alive))
                .map_err(|e| ResponseError::internal(&format!("{e:?}")))
        }
        "rcon.send" => {
            let p: RconSendParams = serde_json::from_value(params.clone())
                .map_err(|e| ResponseError::invalid_params(&e.to_string()))?;
            let client = server::rcon::RconClient {
                addr: crate::server::types::ServerAddr {
                    host: p.host,
                    port: p.port,
                },
                password: p.password,
                enabled: p.enabled,
            };
            match client.send(&p.command, Duration::from_millis(p.timeout_ms)) {
                Ok(reply) => Ok(json!(reply)),
                // Vai no corpo, não como erro de protocolo: a extensão
                // precisa distinguir as condições para escolher a mensagem.
                Err(e) => Ok(json!({ "error": e })),
            }
        }

        // --- depuração ---
        "debug.preflight" => {
            let cwd = path_field(params, "cwd")?;
            Ok(json!(server::plugin::check_debug_plugin(&cwd)))
        }

        _ => Err(ResponseError::method_not_found(method)),
    }
}

/// Os métodos que este core entende, para a extensão descobrir o que a versão
/// em execução suporta.
#[must_use]
pub fn method_names() -> Vec<&'static str> {
    vec![
        "server.detectType",
        "server.detectExecutable",
        "server.loadConfig",
        "server.pidsOnPort",
        "server.projectServersOnPort",
        "server.kill",
        "server.ping",
        "rcon.send",
        "debug.preflight",
    ]
}
