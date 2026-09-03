//! Cliente RCON do protocolo de consulta SA-MP / open.mp.
//!
//! UDP sem retransmissão, e duas consequências moldam o módulo:
//!
//! - **Enviar não é executar.** Sem servidor no ar o datagrama some sem erro,
//!   por isso [`RconClient::send`] sonda a porta antes.
//! - **A resposta não se identifica.** O datagrama não diz a que comando
//!   pertence; [`RconReply`] carrega o comando de volta por isso.

use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::server::types::{RconError, RconReply, ServerAddr};

/// Assinatura que abre todo datagrama do protocolo.
const MAGIC: &[u8; 4] = b"SAMP";
/// Cabeçalho: `SAMP` + 4 octetos de IP + porta (u16 LE) + opcode.
const HEADER_LEN: usize = 11;

/// `true` se o host é loopback.
///
/// Decide se a senha pode sair em texto claro, então erra para o lado seguro.
/// `0.0.0.0` é o curinga "todas as interfaces" e **não** conta como local.
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
/// Só IPv4 numérico: o campo tem 4 bytes e não cabe nome nem IPv6. `None` em
/// vez de um endereço inventado, que mandaria o pacote com IP errado.
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

/// Sonda se há servidor vivo, pelo opcode `p`.
///
/// O ping não exige senha: é o único jeito de saber que a porta responde sem
/// depender de credencial.
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

/// Descarta datagramas sem a assinatura ou o token: a porta efêmera recebe
/// qualquer coisa, e um pacote alheio passaria por resposta do servidor.
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

/// Não é criptográfico e não precisa ser: só distingue esta resposta de um
/// datagrama antigo ainda no caminho.
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
    /// As checagens vão da mais barata à mais cara: a sondagem custa um
    /// datagrama e um prazo, e vem por último.
    ///
    /// # Errors
    /// Ver [`RconError`] — uma variante por condição, nunca um "falhou"
    /// genérico.
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
        // Sem isto, "executou em silêncio" e "não chegou a ninguém" seriam
        // indistinguíveis.
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
            // Sem esta correlação dois envios seguidos trocam de saída.
            command: command.to_string(),
            lines: self.collect_lines(&socket, timeout),
        })
    }

    /// Lê a rajada até o silêncio.
    ///
    /// O protocolo não tem marcador de fim: o servidor manda uma linha por
    /// datagrama e para. O prazo define o fim.
    fn collect_lines(&self, socket: &UdpSocket, timeout: Duration) -> Vec<String> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 1500];
        let mut lines = Vec::new();
        while Instant::now() < deadline {
            let Ok((n, from)) = socket.recv_from(&mut buf) else {
                break;
            };
            // A porta efêmera recebe qualquer datagrama que chegue nela.
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

    /// Servidor de mentira que responde ao ping e a um comando.
    ///
    /// Sem ele não dá para testar o caminho de sucesso — e era justamente ali
    /// que estava a segunda metade do bug: a resposta chegava sem dizer a que
    /// comando pertencia.
    fn fake_server() -> (u16, std::thread::JoinHandle<()>) {
        let socket = UdpSocket::bind(("127.0.0.1", 0)).expect("bind");
        let port = socket.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("timeout");
            let mut buf = [0u8; 1500];
            // Duas mensagens: o ping do `send` e o comando em si.
            for _ in 0..2 {
                let Ok((n, from)) = socket.recv_from(&mut buf) else {
                    return;
                };
                if n < HEADER_LEN {
                    continue;
                }
                match buf[10] {
                    // Ping: devolve o mesmo token, que são os 4 últimos bytes.
                    b'p' => {
                        let _ = socket.send_to(&buf[..n], from);
                    }
                    // Comando: cabeçalho + tamanho + texto.
                    b'x' => {
                        let text = b"resposta do servidor";
                        let mut out = buf[..HEADER_LEN].to_vec();
                        out.extend_from_slice(
                            &u16::try_from(text.len()).unwrap_or(0).to_le_bytes(),
                        );
                        out.extend_from_slice(text);
                        let _ = socket.send_to(&out, from);
                    }
                    _ => {}
                }
            }
        });
        (port, handle)
    }

    #[test]
    fn the_reply_carries_the_command_that_produced_it() {
        // A correlação é o que impede duas saídas trocarem de lugar no painel:
        // a resposta chega depois de um silêncio, e nada no protocolo diz a
        // que comando ela pertence.
        let (port, handle) = fake_server();
        let client = RconClient {
            addr: ServerAddr {
                host: "127.0.0.1".into(),
                port,
            },
            password: "senha".into(),
            enabled: true,
        };
        let reply = client
            .send("gmx", Duration::from_millis(600))
            .expect("servidor de teste responde");
        assert_eq!(reply.command, "gmx");
        assert_eq!(reply.lines, ["resposta do servidor"]);
        let _ = handle.join();
    }

    #[test]
    fn a_command_without_output_is_not_a_failure() {
        // `gmx` e `players` sem ninguém on-line executam e não devolvem texto.
        // Sem distinguir isso de uma falha, o sucesso silencioso ficava
        // idêntico ao erro silencioso.
        let socket = UdpSocket::bind(("127.0.0.1", 0)).expect("bind");
        let port = socket.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("timeout");
            let mut buf = [0u8; 1500];
            // Responde só ao ping; ao comando, silêncio.
            if let Ok((n, from)) = socket.recv_from(&mut buf)
                && n >= HEADER_LEN
                && buf[10] == b'p'
            {
                let _ = socket.send_to(&buf[..n], from);
            }
        });

        let client = RconClient {
            addr: ServerAddr {
                host: "127.0.0.1".into(),
                port,
            },
            password: "senha".into(),
            enabled: true,
        };
        let reply = client
            .send("gmx", Duration::from_millis(400))
            .expect("o comando foi enviado");
        // Vazio é resultado legítimo, não erro.
        assert!(reply.lines.is_empty());
        assert_eq!(reply.command, "gmx");
        let _ = handle.join();
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
