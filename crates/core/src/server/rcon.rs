//! Cliente RCON do protocolo de consulta SA-MP / open.mp.
//!
//! O protocolo é UDP sem retransmissão: cada datagrama pode se perder, e a
//! resposta a um comando chega como uma rajada que só termina no silêncio. Duas
//! consequências moldam este módulo:
//!
//! - **Enviar não é executar.** Sem o servidor no ar, o datagrama some sem erro
//!   nenhum. Por isso [`RconClient::send`] sonda a porta antes: na versão
//!   anterior, em TypeScript, o comando ia para um servidor parado e a interface
//!   respondia "enviado".
//! - **A resposta não se identifica.** O datagrama não diz a que comando
//!   pertence, então quem envia dois seguidos precisa correlacionar pela ordem
//!   de espera — não por qual chega primeiro. [`RconReply`] carrega o comando
//!   de volta por isso.

use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::types::{RconError, RconReply, ServerAddr};

/// Assinatura que abre todo datagrama do protocolo.
const MAGIC: &[u8; 4] = b"SAMP";
/// Cabeçalho: `SAMP` + 4 octetos de IP + porta (u16 LE) + opcode.
const HEADER_LEN: usize = 11;

/// `true` se o host é loopback.
///
/// Decide se a senha do RCON pode sair em texto claro, então erra para o lado
/// seguro: qualquer coisa que não seja comprovadamente loopback é remota.
/// `0.0.0.0` é o curinga "todas as interfaces", **não** loopback — tratá-lo
/// como local mandaria a senha para fora da máquina.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Os quatro octetos que o cabeçalho do protocolo exige.
///
/// Só IPv4 numérico: o campo tem 4 bytes e não há como representar um nome ou
/// um IPv6 ali. `None` diz ao chamador que este host não cabe no protocolo, em
/// vez de inventar um endereço — a versão em TS caía num `127.0.0.1` fixo, o
/// que mandava o pacote com IP errado no cabeçalho para qualquer host nomeado.
fn ipv4_octets(host: &str) -> Option<[u8; 4]> {
    let h = host.trim();
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => Some(v4.octets()),
        // `localhost` resolve para loopback, e o servidor aceita o cabeçalho
        // com 127.0.0.1 porque é para lá que o datagrama vai de fato.
        _ if h.eq_ignore_ascii_case("localhost") => Some([127, 0, 0, 1]),
        _ => None,
    }
}

fn packet(addr: &ServerAddr, opcode: u8, tail: &[u8]) -> Option<Vec<u8>> {
    let octets = ipv4_octets(&addr.host)?;
    let mut pkt = Vec::with_capacity(HEADER_LEN + tail.len());
    pkt.extend_from_slice(MAGIC);
    pkt.extend_from_slice(&octets);
    pkt.extend_from_slice(&addr.port.to_le_bytes());
    pkt.push(opcode);
    pkt.extend_from_slice(tail);
    Some(pkt)
}

fn io_err(e: &io::Error) -> RconError {
    RconError::Io {
        message: e.to_string(),
    }
}

/// Sonda se há servidor vivo em `addr`, pelo opcode `p` (ping).
///
/// O ping **não exige senha** e devolve o mesmo token de 4 bytes, o que o torna
/// o único jeito de saber que a porta responde sem depender de credencial.
///
/// # Errors
/// [`RconError::Io`] se o socket falhar; o timeout devolve `Ok(false)`.
pub fn ping(addr: &ServerAddr, timeout: Duration) -> Result<bool, RconError> {
    let token: [u8; 4] = rand_token();
    let Some(pkt) = packet(addr, b'p', &token) else {
        return Ok(false);
    };

    let socket = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| io_err(&e))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| io_err(&e))?;
    let target: SocketAddr = match format!("{}:{}", addr.host, addr.port).parse() {
        Ok(s) => s,
        // Host nomeado: deixa o resolvedor do sistema decidir.
        Err(_) => match socket.send_to(&pkt, (addr.host.as_str(), addr.port)) {
            Ok(_) => return Ok(await_pong(&socket, token, timeout)),
            Err(e) => return Err(io_err(&e)),
        },
    };
    socket.send_to(&pkt, target).map_err(|e| io_err(&e))?;
    Ok(await_pong(&socket, token, timeout))
}

/// Espera o eco do token dentro do prazo.
///
/// Descarta datagramas que não tenham a assinatura ou o token: a porta efêmera
/// recebe qualquer coisa que chegue nela, e sem esse filtro um pacote alheio
/// passaria por resposta do servidor.
fn await_pong(socket: &UdpSocket, token: [u8; 4], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 1500];
    while Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) if n >= HEADER_LEN + 4 => {
                if &buf[..4] == MAGIC && buf[n - 4..n] == token {
                    return true;
                }
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

/// Token de 4 bytes, sem dependência externa.
///
/// Não é criptográfico e não precisa ser: serve só para distinguir a resposta
/// desta sondagem de um datagrama antigo que ainda esteja no caminho.
fn rand_token() -> [u8; 4] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    nanos.to_le_bytes()
}

/// Configuração necessária para falar RCON com um servidor.
pub struct RconClient {
    pub addr: ServerAddr,
    pub password: String,
    /// `rcon.enable` do `config.json` do servidor.
    pub enabled: bool,
}

impl RconClient {
    /// Envia um comando e devolve o que o servidor respondeu.
    ///
    /// A ordem das checagens é deliberada: as baratas e conclusivas primeiro
    /// (RCON desligado, host remoto, senha ausente), e só então a sondagem, que
    /// custa um datagrama e um prazo de espera.
    ///
    /// # Errors
    /// Ver [`RconError`]. Cada variante corresponde a uma condição distinta que
    /// a interface precisa distinguir — nunca a um "falhou" genérico.
    pub fn send(&self, command: &str, timeout: Duration) -> Result<RconReply, RconError> {
        if !self.enabled {
            return Err(RconError::Disabled);
        }
        if !is_loopback_host(&self.addr.host) {
            return Err(RconError::RemoteBlocked {
                host: self.addr.host.clone(),
            });
        }
        if self.password.is_empty() || self.password.eq_ignore_ascii_case("changename") {
            return Err(RconError::InvalidPassword);
        }
        // Antes de enviar: sem servidor no ar o datagrama some sem erro, e o
        // chamador não teria como distinguir "executou em silêncio" de "não
        // chegou a ninguém".
        if !ping(&self.addr, Duration::from_millis(500))? {
            return Err(RconError::ServerDown {
                addr: self.addr.clone(),
            });
        }

        let mut tail = Vec::new();
        let pw = self.password.as_bytes();
        tail.extend_from_slice(&u16::try_from(pw.len()).unwrap_or(u16::MAX).to_le_bytes());
        tail.extend_from_slice(pw);
        let cmd = command.as_bytes();
        tail.extend_from_slice(&u16::try_from(cmd.len()).unwrap_or(u16::MAX).to_le_bytes());
        tail.extend_from_slice(cmd);

        let Some(pkt) = packet(&self.addr, b'x', &tail) else {
            return Err(RconError::RemoteBlocked {
                host: self.addr.host.clone(),
            });
        };

        let socket = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| io_err(&e))?;
        socket
            .set_read_timeout(Some(timeout))
            .map_err(|e| io_err(&e))?;
        socket
            .send_to(&pkt, (self.addr.host.as_str(), self.addr.port))
            .map_err(|e| io_err(&e))?;

        Ok(RconReply {
            // O comando volta junto: a resposta chega depois de um silêncio, e
            // sem esta correlação dois envios seguidos trocavam de saída.
            command: command.to_string(),
            lines: self.collect_lines(&socket, timeout),
        })
    }

    /// Lê a rajada de resposta até o silêncio que a fecha.
    ///
    /// Não há marcador de fim no protocolo: o servidor manda uma linha por
    /// datagrama e simplesmente para. O prazo de leitura é o que define o fim.
    fn collect_lines(&self, socket: &UdpSocket, timeout: Duration) -> Vec<String> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 1500];
        let mut lines = Vec::new();
        while Instant::now() < deadline {
            let Ok((n, from)) = socket.recv_from(&mut buf) else {
                break;
            };
            // Só o que veio do servidor consultado e traz a assinatura: a porta
            // efêmera recebe qualquer datagrama que chegue nela.
            if from.port() != self.addr.port || n < HEADER_LEN + 2 || &buf[..4] != MAGIC {
                continue;
            }
            let len = u16::from_le_bytes([buf[HEADER_LEN], buf[HEADER_LEN + 1]]) as usize;
            let start = HEADER_LEN + 2;
            let end = (start + len).min(n);
            if start < end {
                lines.push(String::from_utf8_lossy(&buf[start..end]).into_owned());
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_accepts_known_forms() {
        for h in [
            "127.0.0.1",
            "127.1.2.3",
            "localhost",
            "LOCALHOST",
            "::1",
            "[::1]",
            " 127.0.0.1 ",
        ] {
            assert!(is_loopback_host(h), "{h}");
        }
    }

    #[test]
    fn wildcard_is_not_loopback() {
        // `0.0.0.0` é "todas as interfaces": tratá-lo como local mandaria a
        // senha do RCON para fora da máquina.
        assert!(!is_loopback_host("0.0.0.0"));
    }

    #[test]
    fn rejects_external_and_garbage() {
        for h in [
            "10.0.0.1",
            "8.8.8.8",
            "128.0.0.1",
            "",
            "exemplo.com",
            "999.0.0.1",
        ] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }

    #[test]
    fn octets_only_from_ipv4() {
        assert_eq!(ipv4_octets("127.0.0.1"), Some([127, 0, 0, 1]));
        assert_eq!(ipv4_octets("localhost"), Some([127, 0, 0, 1]));
        // Sem inventar endereço: um host que não cabe no cabeçalho devolve
        // `None`, e o chamador recusa em vez de mandar o pacote errado.
        assert_eq!(ipv4_octets("::1"), None);
        assert_eq!(ipv4_octets("exemplo.com"), None);
    }

    #[test]
    fn packet_header() {
        let addr = ServerAddr {
            host: "127.0.0.1".into(),
            port: 7777,
        };
        let pkt = packet(&addr, b'p', &[1, 2, 3, 4]).expect("ipv4");
        assert_eq!(&pkt[..4], MAGIC);
        assert_eq!(&pkt[4..8], &[127, 0, 0, 1]);
        assert_eq!(&pkt[8..10], &7777u16.to_le_bytes());
        assert_eq!(pkt[10], b'p');
        assert_eq!(&pkt[11..], &[1, 2, 3, 4]);
    }

    fn client(host: &str, enabled: bool, password: &str) -> RconClient {
        RconClient {
            addr: ServerAddr {
                host: host.into(),
                port: 7777,
            },
            password: password.into(),
            enabled,
        }
    }

    #[test]
    fn disabled_rcon_is_refused_before_any_io() {
        let c = client("127.0.0.1", false, "senha");
        assert_eq!(
            c.send("gmx", Duration::from_millis(50)),
            Err(RconError::Disabled)
        );
    }

    #[test]
    fn remote_host_is_refused() {
        let c = client("8.8.8.8", true, "senha");
        assert!(matches!(
            c.send("gmx", Duration::from_millis(50)),
            Err(RconError::RemoteBlocked { .. })
        ));
    }

    #[test]
    fn missing_or_default_password_is_refused() {
        for pw in ["", "changename", "CHANGENAME"] {
            let c = client("127.0.0.1", true, pw);
            assert_eq!(
                c.send("gmx", Duration::from_millis(50)),
                Err(RconError::InvalidPassword),
                "{pw}"
            );
        }
    }

    #[test]
    fn stopped_server_errors_instead_of_faking_send() {
        // O bug que motivou trazer isto para o Rust: em TS o comando ia para um
        // servidor parado e o painel respondia "enviado". Aqui o tipo não
        // permite — `send` devolve `ServerDown` e não há `RconReply` nenhum.
        let c = client("127.0.0.1", true, "senha");
        // Porta improvável: nada escutando.
        let c = RconClient {
            addr: ServerAddr {
                host: "127.0.0.1".into(),
                port: 59991,
            },
            ..c
        };
        assert!(matches!(
            c.send("gmx", Duration::from_millis(100)),
            Err(RconError::ServerDown { .. })
        ));
    }
}
