//! Os métodos do estado local do projeto: favoritos e histórico do painel do
//! servidor.
//!
//! Sem estado no processo: o arquivo é relido a cada pedido. Outra janela do
//! mesmo projeto também grava, e ele é pequeno demais para valer um cache.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::project::state::{ServerState, StateManager};

use super::protocol::ResponseError;

/// Os métodos deste módulo.
pub const METHODS: [&str; 2] = ["state.get", "state.updateServer"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetParams {
    workspace_root: PathBuf,
}

/// Estrito: um tipo errado aqui é erro de quem chama, não dado do usuário a
/// tolerar.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateServerParams {
    workspace_root: PathBuf,
    server: ServerState,
}

/// Executa um método do estado. Toda resposta traz o estado como ficou.
///
/// # Errors
/// [`ResponseError`] quando o método não é daqui, os parâmetros não servem ou a
/// gravação falha.
pub fn dispatch(method: &str, params: &Value) -> Result<Value, ResponseError> {
    match method {
        "state.get" => {
            let GetParams { workspace_root } = parse(params)?;
            Ok(json!(StateManager::new(&workspace_root).get_all()))
        }
        "state.updateServer" => {
            let UpdateServerParams {
                workspace_root,
                server,
            } = parse(params)?;
            let mut state = StateManager::new(&workspace_root);
            state
                .update_server(server)
                .map_err(|e| ResponseError::internal(&e.to_string()))?;
            Ok(json!(state.get_all()))
        }
        _ => Err(ResponseError::method_not_found(method)),
    }
}

fn parse<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, ResponseError> {
    serde_json::from_value(params.clone())
        .map_err(|e| ResponseError::invalid_params(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_comes_back_as_it_was_stored() {
        let root = std::env::temp_dir().join(format!("pawnpro-rpc-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let answer = dispatch(
            "state.updateServer",
            &json!({ "workspaceRoot": root, "server": { "favorites": ["gmx"], "history": [] } }),
        )
        .expect("gravar");
        let reread = dispatch("state.get", &json!({ "workspaceRoot": root })).expect("ler");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(answer["server"]["favorites"][0], "gmx");
        assert_eq!(answer, reread);
    }

    #[test]
    fn a_malformed_list_from_the_caller_is_refused() {
        // Da extensão, um tipo errado é defeito de quem chama: gravar por cima
        // apagaria o que o usuário tinha.
        let out = dispatch(
            "state.updateServer",
            &json!({ "workspaceRoot": std::env::temp_dir(), "server": { "favorites": 5 } }),
        );
        assert_eq!(out.err().map(|e| e.code), Some(-32602));
    }
}
