//! A engine hospedada pelo core atende LSP no soquete local.
//!
//! O que se verifica aqui não é a análise de Pawn — isso é da engine —, mas a
//! ligação: o endereço que o core informa aceita conexão, a apresentação `lsp`
//! leva a um servidor LSP de verdade, e só o dono alcança o soquete.
//!
//! Só roda no Unix: no Windows o transporte é um named pipe, e o teste
//! equivalente precisaria do cliente de lá.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use pawnpro_core::rpc::Sender;
use pawnpro_core::supervisor::engine::EngineService;

/// Um projeto qualquer: estes testes não olham a configuração.
fn project() -> std::path::PathBuf {
    std::env::temp_dir()
}

/// Tempo máximo para a engine começar a aceitar conexões.
const STARTUP: Duration = Duration::from_secs(5);

/// Descarta o que o core notifica: aqui só interessa o socket.
struct Discard;

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Conecta no soquete e se apresenta como LSP, insistindo enquanto a thread do
/// gateway não subiu.
fn connect(address: &str) -> UnixStream {
    let deadline = Instant::now() + STARTUP;
    loop {
        match UnixStream::connect(address) {
            Ok(mut stream) => {
                stream.write_all(b"PAWNPRO/1 lsp\n").expect("apresentar");
                return stream;
            }
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("a engine não aceitou conexão em {STARTUP:?}: {e}"),
        }
    }
}

/// Envia uma mensagem LSP, com o enquadramento por `Content-Length`.
fn send(stream: &mut UnixStream, body: &str) {
    write!(stream, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("escrever");
    stream.flush().expect("descarregar");
}

/// Lê uma mensagem LSP e devolve o corpo.
fn receive(reader: &mut BufReader<UnixStream>) -> String {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("ler cabeçalho");
        assert_ne!(read, 0, "a engine fechou a conexão antes de responder");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.parse().expect("comprimento");
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).expect("ler corpo");
    String::from_utf8(body).expect("utf-8")
}

#[test]
fn the_engine_answers_the_initialize_where_the_core_says_it_listens() {
    let engine = EngineService::new();
    let sender = Sender::new(Box::new(Discard));
    let address = engine
        .start(&sender, &project(), "pt-br")
        .expect("subir a engine");

    let mut stream = connect(&address);
    stream
        .set_read_timeout(Some(STARTUP))
        .expect("prazo de leitura");
    send(
        &mut stream,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"capabilities":{}}}"#,
    );

    let mut reader = BufReader::new(stream.try_clone().expect("clonar"));
    let body = receive(&mut reader);
    let answer: serde_json::Value = serde_json::from_str(&body).expect("json");

    assert_eq!(answer["id"], 1);
    assert!(
        answer["result"]["capabilities"].is_object(),
        "sem capacidades não é um servidor LSP: {body}"
    );

    engine.stop();
}

/// Com a engine parada, a conexão fecha na hora: ficar numa fila deixaria o
/// cliente esperando uma resposta que não vem.
#[test]
fn a_stopped_engine_closes_the_connection() {
    let engine = EngineService::new();
    let sender = Sender::new(Box::new(Discard));
    let address = engine
        .start(&sender, &project(), "pt-br")
        .expect("subir a engine");
    engine.stop();

    // O laço leva até um intervalo de espera para sair; uma conexão que chega
    // antes disso ainda é atendida e fica muda. Tenta até uma ser fechada.
    let deadline = Instant::now() + STARTUP;
    let closed = loop {
        let mut stream = connect(&address);
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .expect("prazo de leitura");
        let mut buf = [0u8; 1];
        if matches!(stream.read(&mut buf), Ok(0)) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
    };
    assert!(closed, "com a engine parada, a conexão devia fechar");
}

#[test]
fn the_address_survives_a_stop_and_a_new_start() {
    // A extensão guarda o endereço; mudá-lo a cada início a obrigaria a
    // perguntar de novo depois de qualquer queda.
    let engine = EngineService::new();
    let sender = Sender::new(Box::new(Discard));

    let first = engine
        .start(&sender, &project(), "pt-br")
        .expect("primeiro início");
    engine.stop();
    let second = engine
        .start(&sender, &project(), "pt-br")
        .expect("segundo início");

    assert_eq!(first, second);
    engine.stop();
}

#[test]
fn only_the_owner_can_reach_the_socket() {
    // É esta permissão que substitui a autenticação que o LSP não tem: sem
    // ela, qualquer processo da máquina pediria à engine a leitura de
    // qualquer arquivo do usuário.
    use std::os::unix::fs::PermissionsExt;

    let engine = EngineService::new();
    let sender = Sender::new(Box::new(Discard));
    let address = engine
        .start(&sender, &project(), "pt-br")
        .expect("subir a engine");

    let socket = std::path::Path::new(&address);
    let dir = socket.parent().expect("diretório do soquete");
    let mode = std::fs::metadata(dir)
        .expect("ler o diretório")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(mode, 0o700, "o diretório do soquete está aberto a outros");
    engine.stop();
}

#[test]
fn the_socket_does_not_outlive_the_service() {
    // O soquete é um arquivo: deixá-lo para trás encheria a temporária e faria
    // o próximo core com o mesmo PID falhar ao criar o dele.
    let sender = Sender::new(Box::new(Discard));
    let address = {
        let engine = EngineService::new();
        let address = engine
            .start(&sender, &project(), "pt-br")
            .expect("subir a engine");
        engine.stop();
        address
    };

    assert!(
        !std::path::Path::new(&address).exists(),
        "o soquete continuou em {address}"
    );
}
