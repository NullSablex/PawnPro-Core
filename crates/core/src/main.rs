//! Ponto de entrada do núcleo do PawnPro.
//!
//! Lê requisições JSON-RPC no stdin e responde no stdout, uma mensagem por
//! linha. A lógica está na biblioteca; aqui fica só o que precisa de um
//! processo de verdade.
//!
//! Ver `docs/architecture.md`.

use std::io::{BufReader, stdin, stdout};

use pawnpro_core::rpc::{Sender, Services, serve};

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

    // O soquete só nasce quando a extensão pedir a engine ou o depurador:
    // criá-lo na partida gastaria um em cada janela que nem chega a usá-los.
    let services = Services::new(&sender);

    // O laço termina quando o stdin fecha, que é como a extensão encerra o
    // core: fechar o canal, em vez de matar o processo.
    let result = serve(BufReader::new(stdin().lock()), &sender, &services);

    // Fechar o canal encerra o core; o `Drop` dos serviços para as threads,
    // encerra as sessões de depuração com os servidores delas e apaga o
    // soquete, que senão sobreviveria ao processo.
    drop(services);
    result
}
