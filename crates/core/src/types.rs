//! Tipos que o core compartilha com os subsistemas e com a extensão.
//!
//! Fica separado do `core` para que uma crate possa falar o mesmo vocabulário
//! sem depender do supervisor inteiro.

use serde::{Deserialize, Serialize};

/// Endereço de um servidor Pawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAddr {
    pub host: String,
    pub port: u16,
}

/// Por que um comando não pôde ser enviado, ou o que houve depois do envio.
///
/// É um `enum` e não uma string de erro porque cada variante pede uma reação
/// diferente na interface — e o `match` exaustivo obriga a tratar todas quando
/// uma nova aparecer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RconError {
    /// A porta não respondeu à sondagem: não há servidor para receber o comando.
    ///
    /// Esta variante é a razão de o envio checar antes: na versão em TS o
    /// comando ia para um servidor parado e o painel respondia "enviado".
    ServerDown { addr: ServerAddr },
    /// O RCON está desligado no `config.json` do servidor (`rcon.enable`).
    Disabled,
    /// Sem senha, ou com a senha padrão que o servidor recusa.
    InvalidPassword,
    /// O RCON manda a senha em texto claro: fora do loopback isso a exporia a
    /// quem estiver no caminho.
    RemoteBlocked { host: String },
    /// O servidor recebeu o comando e não respondeu dentro do prazo.
    Timeout { millis: u64 },
    /// Falha de socket.
    Io { message: String },
}

/// Resultado de um comando RCON, correlacionado ao que o originou.
///
/// O `command` volta junto porque a resposta chega depois de um silêncio que
/// fecha a rajada de datagramas: sem essa correlação, dois comandos rápidos
/// tinham suas saídas trocadas no painel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RconReply {
    pub command: String,
    /// Linhas devolvidas pelo servidor. Vazio é resultado legítimo: comandos
    /// como `gmx` executam sem devolver texto.
    pub lines: Vec<String>,
}
