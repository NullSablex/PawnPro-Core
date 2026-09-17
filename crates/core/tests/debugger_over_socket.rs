//! O depurador hospedado pelo core, pelo soquete único.
//!
//! A extensão conecta e se apresenta como `dap`; o servidor que a sessão sobe
//! recebe no ambiente o endereço e o id, e o plugin dele conecta no mesmo
//! soquete se apresentando como `plugin <id>`. O servidor aqui é um `sh` de
//! verdade, e o plugin é o próprio teste.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pawnpro_core::gateway::Gateway;
use pawnpro_core::rpc::Sender;
use pawnpro_core::supervisor::debugger::DebuggerService;
use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(10);

struct Discard;

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn wait_until(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn greet(address: &str, greeting: &str) -> UnixStream {
    let mut stream = UnixStream::connect(address).expect("conectar no core");
    writeln!(stream, "{greeting}").expect("apresentar");
    stream
}

/// Uma conexão DAP com as mensagens recebidas guardadas em ordem.
struct Editor {
    stream: UnixStream,
    received: Arc<Mutex<Vec<Value>>>,
    seq: i64,
}

impl Editor {
    fn connect(address: &str) -> Self {
        let stream = greet(address, "PAWNPRO/1 dap");
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        let mut reader = BufReader::new(stream.try_clone().expect("clonar"));
        std::thread::spawn(move || {
            while let Some(message) = read_dap(&mut reader) {
                sink.lock().unwrap().push(message);
            }
        });
        Self {
            stream,
            received,
            seq: 0,
        }
    }

    fn request(&mut self, command: &str, arguments: &Value) {
        self.seq += 1;
        let body = json!({
            "seq": self.seq, "type": "request", "command": command, "arguments": arguments
        })
        .to_string();
        write!(self.stream, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("escrever");
    }

    fn console(&self) -> String {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m["event"] == "output")
            .filter_map(|m| m["body"]["output"].as_str())
            .collect()
    }

    fn has_event(&self, name: &str) -> bool {
        self.received
            .lock()
            .unwrap()
            .iter()
            .any(|m| m["event"] == name)
    }
}

fn read_dap(reader: &mut BufReader<UnixStream>) -> Option<Value> {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.parse().ok()?;
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// O que o servidor falso anunciou: o ambiente que o plugin receberia e o PID.
struct Announced {
    endpoint: String,
    session: String,
    pid: u32,
}

/// Lança um servidor que anuncia o ambiente de depuração e fica de pé.
fn launch(editor: &mut Editor) {
    editor.request("initialize", &json!({ "locale": "en" }));
    editor.request(
        "launch",
        &json!({
            "program": "/tmp/pawnpro-missing.amx",
            "serverCommand": {
                "exe": "sh",
                "args": ["-c", "echo \"announce endpoint=$PAWNPRO_DBG_ENDPOINT session=$PAWNPRO_DBG_SESSION pid=$$ end\"; exec sleep 30"],
                "cwd": ""
            }
        }),
    );
}

fn announced(editor: &Editor, nth: usize) -> Announced {
    assert!(
        wait_until(|| editor.console().matches("announce ").count() > nth),
        "o servidor não anunciou: {}",
        editor.console()
    );
    let console = editor.console();
    let line = console.split("announce ").nth(nth + 1).unwrap();
    let field = |name: &str| {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(name))
            .unwrap()
            .to_string()
    };
    Announced {
        endpoint: field("endpoint="),
        session: field("session="),
        pid: field("pid=").parse().unwrap(),
    }
}

fn is_alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z "))
}

/// `true` se o core fechou a conexão.
///
/// Fechar um soquete Unix com bytes ainda não lidos faz o Linux responder
/// `ECONNRESET` em vez de EOF; as duas formas são o core fechando. Prazo
/// estourado não é: é o core segurando a conexão.
fn is_closed(stream: &mut UnixStream) -> bool {
    stream
        .set_read_timeout(Some(DEADLINE))
        .expect("prazo de leitura");
    let mut buf = [0u8; 1];
    match stream.read(&mut buf) {
        Ok(0) => true,
        Err(e) => e.kind() == std::io::ErrorKind::ConnectionReset,
        Ok(_) => false,
    }
}

fn start() -> (Arc<Gateway>, DebuggerService, String) {
    let gateway = Gateway::new();
    let debugger = DebuggerService::new(Arc::clone(&gateway));
    let address = debugger
        .start(&Sender::new(Box::new(Discard)))
        .expect("abrir o soquete");
    (gateway, debugger, address)
}

#[test]
fn the_plugin_reaches_its_session_through_the_same_socket() {
    let (_gateway, _debugger, address) = start();
    let mut editor = Editor::connect(&address);
    launch(&mut editor);
    let server = announced(&editor, 0);
    assert_eq!(
        server.endpoint, address,
        "o plugin conecta no mesmo soquete"
    );

    let mut plugin = greet(&address, &format!("PAWNPRO/1 plugin {}", server.session));
    editor.request("configurationDone", &json!({}));
    let mut commands = BufReader::new(plugin.try_clone().unwrap());
    let mut received = String::new();
    while !received.contains("configured") {
        let mut line = String::new();
        assert_ne!(
            commands.read_line(&mut line).unwrap(),
            0,
            "o core fechou o plugin"
        );
        received.push_str(&line);
    }

    writeln!(
        plugin,
        "{}",
        json!({ "event": "paused", "reason": "breakpoint", "frames": [] })
    )
    .unwrap();
    assert!(wait_until(|| editor.has_event("stopped")));

    editor.request("disconnect", &json!({}));
    assert!(
        wait_until(|| !is_alive(server.pid)),
        "o fim da sessão derruba o servidor"
    );
}

/// Uma reserva atende um servidor só: um segundo plugin com o mesmo id, um id
/// que ninguém reservou ou um id de antes do restart não alcançam a sessão.
#[test]
fn only_the_awaited_plugin_is_accepted() {
    let (_gateway, _debugger, address) = start();
    let mut editor = Editor::connect(&address);
    launch(&mut editor);
    let first = announced(&editor, 0);

    let _plugin = greet(&address, &format!("PAWNPRO/1 plugin {}", first.session));
    assert!(wait_until(|| editor.console().contains("Connected")));
    let mut intruder = greet(&address, &format!("PAWNPRO/1 plugin {}", first.session));
    assert!(is_closed(&mut intruder), "o segundo plugin com o mesmo id");
    let mut stranger = greet(&address, "PAWNPRO/1 plugin 999999");
    assert!(is_closed(&mut stranger), "um id que ninguém reservou");
    let mut nameless = greet(&address, "PAWNPRO/1 plugin");
    assert!(is_closed(&mut nameless), "sem id");

    editor.request("restart", &json!({}));
    let second = announced(&editor, 1);
    assert_ne!(first.session, second.session, "cada servidor tem o seu id");
    assert!(!is_alive(first.pid));
    let mut stale = greet(&address, &format!("PAWNPRO/1 plugin {}", first.session));
    assert!(is_closed(&mut stale), "o id do servidor derrubado");

    editor.request("disconnect", &json!({}));
    assert!(wait_until(|| !is_alive(second.pid)));
}

#[test]
fn a_connection_that_does_not_greet_is_closed() {
    let (_gateway, _debugger, address) = start();

    let mut raw = UnixStream::connect(&address).unwrap();
    write!(raw, "Content-Length: 2\r\n\r\n{{}}").unwrap();
    assert!(is_closed(&mut raw), "DAP sem apresentação");

    let mut unknown = greet(&address, "PAWNPRO/1 ftp");
    assert!(is_closed(&mut unknown), "canal desconhecido");
}

/// Fechar o core — a extensão fecha o stdin — não pode deixar servidor órfão
/// nem soquete para trás.
#[test]
fn closing_the_core_ends_sessions_and_their_servers() {
    let (gateway, debugger, address) = start();
    let mut editor = Editor::connect(&address);
    launch(&mut editor);
    let server = announced(&editor, 0);

    drop(debugger);
    drop(gateway);

    assert!(
        wait_until(|| !is_alive(server.pid)),
        "o servidor sobreviveu ao core"
    );
    assert!(!std::path::Path::new(&address).exists(), "o soquete ficou");
}
