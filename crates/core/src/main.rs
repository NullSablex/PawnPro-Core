//! Ponto de entrada do núcleo do PawnPro.
//!
//! Lê requisições JSON-RPC no stdin e responde no stdout, uma mensagem por
//! linha. A lógica está na biblioteca; aqui fica só o que precisa de um
//! processo de verdade.
//!
//! Ver `docs/architecture.md`.

use std::io::{BufReader, stdin, stdout};

use pawnpro_core::rpc::{Sender, serve};

fn main() -> std::io::Result<()> {
    let sender = Sender::new(Box::new(stdout()));

    // Um panic no laço principal não pode encerrar em silêncio: a extensão
    // precisa saber por que o core parou de responder. O supervisor faz o mesmo
    // pelos subsistemas; este gancho cobre o que roda aqui.
    let panic_sender = sender.clone();
    std::panic::set_hook(Box::new(move |info| {
        panic_sender.notify(
            "core.panic",
            serde_json::json!({ "detail": info.to_string() }),
        );
    }));

    // O laço termina quando o stdin fecha, que é como a extensão encerra o
    // core: fechar o canal, em vez de matar o processo.
    serve(BufReader::new(stdin().lock()), &sender)
}
