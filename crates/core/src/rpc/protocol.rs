//! O formato das mensagens trocadas com a extensão.
//!
//! JSON-RPC 2.0, uma mensagem por linha. Sem o `Content-Length` do LSP: não há
//! corpo binário nem streaming, e uma linha por mensagem é mais fácil de
//! depurar.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Número ou texto, devolvido no mesmo tipo que veio: um cliente que manda
/// `"1"` espera `"1"` de volta, não `1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    Text(String),
}

/// Uma chamada vinda da extensão.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    /// Ausente numa notificação — que não espera resposta.
    #[serde(default)]
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Resposta a uma requisição.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

impl Response {
    #[must_use]
    pub fn ok(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn err(id: RequestId, error: ResponseError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Erro devolvido a uma requisição.
#[derive(Debug, Clone, Serialize)]
pub struct ResponseError {
    pub code: i32,
    pub message: String,
}

impl ResponseError {
    /// Método desconhecido — código padrão do JSON-RPC.
    #[must_use]
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("método desconhecido: {method}"),
        }
    }

    /// Parâmetros ausentes ou de tipo errado.
    #[must_use]
    pub fn invalid_params(detail: &str) -> Self {
        Self {
            code: -32602,
            message: format!("parâmetros inválidos: {detail}"),
        }
    }

    /// Falha ao executar o que foi pedido.
    ///
    /// `-32000` é o início da faixa reservada a erros da aplicação.
    #[must_use]
    pub fn internal(detail: &str) -> Self {
        Self {
            code: -32000,
            message: detail.to_string(),
        }
    }
}

/// Aviso enviado sem ninguém ter pedido: é como o supervisor conta que um
/// subsistema caiu.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub jsonrpc: &'static str,
    pub method: String,
    pub params: Value,
}

impl Notification {
    #[must_use]
    pub fn new(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        }
    }
}

/// O que sai pelo stdout.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Outgoing {
    Response(Response),
    Notification(Notification),
}
