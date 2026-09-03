//! JSON-RPC sobre stdio, uma mensagem por linha.
//!
//! A extensão escreve uma requisição no stdin e lê a resposta no stdout. O
//! mesmo canal leva as notificações do supervisor.

pub mod handlers;
pub mod protocol;

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use serde_json::json;

use protocol::{Notification, Outgoing, Request, RequestId, Response, ResponseError};

/// Escreve mensagens no stdout, uma por linha.
///
/// Sem o `Mutex`, as linhas do laço principal e do supervisor se
/// intercalariam.
#[derive(Clone)]
pub struct Sender {
    out: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Sender {
    #[must_use]
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self {
            out: Arc::new(Mutex::new(out)),
        }
    }

    /// Falha de escrita é ignorada: significa que a extensão fechou o canal,
    /// e o laço principal encerra ao ler EOF.
    pub fn send(&self, message: &Outgoing) {
        let Ok(line) = serde_json::to_string(message) else {
            return;
        };
        if let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }

    /// Envia uma notificação.
    pub fn notify(&self, method: &str, params: serde_json::Value) {
        self.send(&Outgoing::Notification(Notification::new(method, params)));
    }
}

/// Roda o laço de mensagens até o stdin fechar.
///
/// Uma linha malformada não derruba o processo.
///
/// # Errors
/// Falha de leitura do stdin.
pub fn serve<R: BufRead>(input: R, sender: &Sender) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(request) = serde_json::from_str::<Request>(trimmed) else {
            // Sem `id` recuperável não há a quem responder.
            continue;
        };

        let Some(id) = request.id.clone() else {
            // Notificação: executa e não responde.
            let _ = handle(&request.method, &request.params);
            continue;
        };

        let response = match handle(&request.method, &request.params) {
            Ok(result) => Response::ok(id, result),
            Err(error) => Response::err(id, error),
        };
        sender.send(&Outgoing::Response(response));
    }
    Ok(())
}

/// Executa um método, incluindo os do próprio core.
fn handle(method: &str, params: &serde_json::Value) -> Result<serde_json::Value, ResponseError> {
    match method {
        // A extensão pergunta o que esta versão suporta, em vez de assumir.
        "core.version" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "methods": handlers::method_names(),
        })),
        other => handlers::dispatch(other, params),
    }
}

/// Um `id` numérico, para quem precisa construir uma resposta à mão.
#[must_use]
pub const fn request_id(n: i64) -> RequestId {
    RequestId::Number(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

    /// Coletor que guarda o que foi escrito, para inspecionar nos testes.
    struct Collector(mpsc::Sender<String>);

    impl Write for Collector {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let text = String::from_utf8_lossy(buf).into_owned();
            if !text.trim().is_empty() {
                let _ = self.0.send(text.trim().to_string());
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Roda o laço com as linhas dadas e devolve o que saiu.
    fn exchange(input: &str) -> Vec<serde_json::Value> {
        let (tx, rx) = mpsc::channel();
        let sender = Sender::new(Box::new(Collector(tx)));
        serve(Cursor::new(input), &sender).expect("laço");
        drop(sender);
        rx.try_iter()
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect()
    }

    #[test]
    fn answers_with_the_same_id_it_received() {
        let out = exchange(r#"{"jsonrpc":"2.0","id":7,"method":"core.version"}"#);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 7);
        assert!(out[0]["result"]["methods"].is_array());
    }

    #[test]
    fn a_text_id_comes_back_as_text() {
        // Um cliente que manda `"1"` espera `"1"`, não `1`.
        let out = exchange(r#"{"jsonrpc":"2.0","id":"abc","method":"core.version"}"#);
        assert_eq!(out[0]["id"], "abc");
    }

    #[test]
    fn an_unknown_method_is_an_error_not_a_crash() {
        let out = exchange(r#"{"jsonrpc":"2.0","id":1,"method":"nao.existe"}"#);
        assert_eq!(out[0]["error"]["code"], -32601);
    }

    #[test]
    fn missing_parameters_are_reported() {
        let out = exchange(r#"{"jsonrpc":"2.0","id":1,"method":"server.detectType"}"#);
        assert_eq!(out[0]["error"]["code"], -32602);
        assert!(out[0]["error"]["message"].as_str().unwrap().contains("cwd"));
    }

    #[test]
    fn a_notification_gets_no_answer() {
        // Sem `id` não há a quem responder.
        let out = exchange(r#"{"jsonrpc":"2.0","method":"core.version"}"#);
        assert!(out.is_empty());
    }

    #[test]
    fn a_malformed_line_does_not_stop_the_loop() {
        // Uma linha quebrada não pode derrubar o core: as seguintes continuam.
        let out = exchange(
            "{ isto não é json\n\
             \n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"core.version\"}\n",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 2);
    }

    #[test]
    fn several_requests_are_answered_in_order() {
        let out = exchange(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"core.version\"}\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"core.version\"}\n",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], 1);
        assert_eq!(out[1]["id"], 2);
    }

    #[test]
    fn killing_a_process_that_is_not_ours_is_refused() {
        // A política de dono vive no core: a extensão não consegue contorná-la
        // pedindo direto por RPC.
        let out = exchange(
            r#"{"jsonrpc":"2.0","id":1,"method":"server.kill","params":{"pid":1,"exe":"/bin/sh"}}"#,
        );
        assert_eq!(out[0]["error"]["code"], -32000);
    }

    #[test]
    fn every_listed_method_is_dispatchable() {
        // Um nome na lista que o `dispatch` não conhece faria a extensão
        // chamar algo inexistente.
        for method in handlers::method_names() {
            let err = handlers::dispatch(method, &json!({})).err();
            let code = err.map_or(0, |e| e.code);
            assert_ne!(code, -32601, "{method} está na lista mas não é despachado");
        }
    }
}
