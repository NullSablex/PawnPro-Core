//! O core é a única fonte da configuração da engine.
//!
//! A engine não lê `config.json` nem os arquivos de lista: quem lê é o core, e
//! o que ele entrega é o que ela usa. Estes testes exercitam o caminho inteiro
//! — arquivo no disco, leitura pelo core, entrega pelo canal, análise da engine
//! — porque é o único jeito de perceber quando uma das pontas para de casar com
//! a outra.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use pawnpro_core::rpc::Sender;
use pawnpro_core::supervisor::engine::EngineService;

/// Tempo máximo para a engine aceitar conexão e para cada resposta.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Descarta o que o core notifica.
struct Discard;

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Um projeto Pawn mínimo: um include com uma nativa e um `.pwn` que a chama.
struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        private_home();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-contract-{nanos}"));
        // `libs`, e não `include`: o core procura `include`, `pawno/include` e
        // `qawno/include` por conta própria, e o teste ficaria cego para o que
        // veio da configuração.
        std::fs::create_dir_all(root.join("libs")).expect("criar projeto");
        std::fs::create_dir_all(root.join(".pawnpro")).expect("criar .pawnpro");
        std::fs::write(
            root.join("libs").join("lib.inc"),
            "native MyOwnNative(value);\n",
        )
        .expect("escrever include");
        std::fs::write(
            root.join("main.pwn"),
            "#include <lib>\n\nmain()\n{\n    MyOwnNative(1);\n}\n",
        )
        .expect("escrever fonte");
        Self(root)
    }

    fn main_uri(&self) -> String {
        format!("file://{}", self.0.join("main.pwn").display())
    }

    fn include_dir(&self) -> PathBuf {
        self.0.join("libs")
    }

    /// Escreve o `.pawnpro/config.json` do projeto com os includes dados.
    fn configure_includes(&self, paths: &[PathBuf]) {
        let list: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let config = serde_json::json!({ "includePaths": list });
        std::fs::write(
            self.0.join(".pawnpro").join("config.json"),
            serde_json::to_string_pretty(&config).expect("json"),
        )
        .expect("escrever config");
    }
}

/// Aponta `HOME` para um diretório vazio.
///
/// Sem isto os testes leriam o `~/.pawnpro/config.json` de quem os roda, e
/// passariam ou falhariam conforme a máquina.
fn private_home() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let home = std::env::temp_dir().join("pawnpro-contract-home");
        std::fs::create_dir_all(&home).expect("criar home de teste");
        // Antes de qualquer thread do teste ler a configuração.
        unsafe { std::env::set_var("HOME", &home) };
    });
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn connect(address: &str) -> UnixStream {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match UnixStream::connect(address) {
            Ok(stream) => return stream,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("a engine não aceitou conexão: {e}"),
        }
    }
}

fn send(stream: &mut UnixStream, body: &str) {
    write!(stream, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("escrever");
    stream.flush().expect("descarregar");
}

fn receive(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("ler cabeçalho");
        assert_ne!(read, 0, "a engine fechou a conexão");
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
    serde_json::from_slice(&body).expect("json")
}

/// Lê até chegar o `publishDiagnostics` do arquivo, ignorando o resto.
fn diagnostics_for(reader: &mut BufReader<UnixStream>, uri: &str) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let Some(message) = receive_opt(reader) else {
            continue;
        };
        if message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"].as_str() == Some(uri)
        {
            return message["params"]["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default();
        }
    }
    panic!("a engine não publicou diagnósticos para {uri}");
}

/// Como `receive`, mas devolve `None` quando o prazo de leitura estoura.
///
/// Esperar por uma publicação que talvez não venha é parte do teste — o
/// silêncio é uma resposta, não um erro.
fn receive_opt(reader: &mut BufReader<UnixStream>) -> Option<serde_json::Value> {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => panic!("a engine fechou a conexão"),
            Ok(_) => {}
            Err(_) => return None,
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.parse().expect("comprimento");
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Uma sessão LSP aberta contra a engine que o core hospeda.
struct Session {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    uri: String,
}

impl Session {
    /// Sobe a engine para o projeto e abre o arquivo nela.
    fn open(engine: &EngineService, project: &Project) -> Self {
        let sender = Sender::new(Box::new(Discard));
        let address = engine
            .start(&sender, &project.0, "pt-br")
            .expect("subir a engine");

        let stream = connect(&address);
        stream.set_read_timeout(Some(TIMEOUT)).expect("prazo");
        let reader = BufReader::new(stream.try_clone().expect("clonar"));
        let mut session = Self {
            stream,
            reader,
            uri: project.main_uri(),
        };

        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "capabilities": {},
                "rootUri": format!("file://{}", project.0.display()),
            }
        });
        send(&mut session.stream, &initialize.to_string());
        let answer = receive(&mut session.reader);
        assert_eq!(answer["id"], 1, "o initialize não foi respondido");

        send(
            &mut session.stream,
            &serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}}).to_string(),
        );

        let source = std::fs::read_to_string(project.0.join("main.pwn")).expect("ler fonte");
        let did_open = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": session.uri,
                    "languageId": "pawn",
                    "version": 1,
                    "text": source,
                }
            }
        });
        send(&mut session.stream, &did_open.to_string());
        session
    }

    /// Os próximos diagnósticos publicados para o arquivo.
    fn diagnostics(&mut self) -> Vec<serde_json::Value> {
        diagnostics_for(&mut self.reader, &self.uri)
    }
}

/// `true` se algum diagnóstico acusa a nativa como desconhecida.
fn complains_about_the_native(diagnostics: &[serde_json::Value]) -> bool {
    diagnostics.iter().any(|d| {
        d["message"]
            .as_str()
            .is_some_and(|m| m.contains("MyOwnNative"))
    })
}

#[test]
fn the_engine_uses_the_includes_the_core_read_from_the_config() {
    // O core lê o `.pawnpro/config.json`, resolve os caminhos e entrega. A
    // engine não abriu esse arquivo.
    let project = Project::new();
    project.configure_includes(&[project.include_dir()]);

    let engine = EngineService::new();
    let mut session = Session::open(&engine, &project);

    assert!(
        !complains_about_the_native(&session.diagnostics()),
        "a engine não recebeu o include que o core resolveu"
    );
}

#[test]
fn without_the_include_the_engine_does_complain() {
    // O contraponto: sem esta queixa, o teste acima passaria mesmo que a
    // configuração nunca chegasse.
    let project = Project::new();
    project.configure_includes(&[]);

    let engine = EngineService::new();
    let mut session = Session::open(&engine, &project);

    assert!(
        complains_about_the_native(&session.diagnostics()),
        "esperava a queixa sobre a nativa desconhecida"
    );
}

#[test]
fn editing_the_config_reaches_the_engine_without_the_editor_asking() {
    // O ponto do observador: quem percebe a edição é o core, e ele reentrega
    // sozinho. O editor não pediu nada, e mesmo assim os diagnósticos mudam.
    let project = Project::new();
    project.configure_includes(&[]);

    let engine = EngineService::new();
    let mut session = Session::open(&engine, &project);
    assert!(
        complains_about_the_native(&session.diagnostics()),
        "o cenário deveria começar com a queixa"
    );

    project.configure_includes(&[project.include_dir()]);

    // A republicação vem por conta do core; o prazo cobre o intervalo com que
    // ele confere os arquivos.
    let deadline = Instant::now() + TIMEOUT;
    let mut quiet = false;
    while Instant::now() < deadline {
        if !complains_about_the_native(&session.diagnostics()) {
            quiet = true;
            break;
        }
    }
    assert!(quiet, "a mudança no config.json não chegou à engine");
}

#[test]
fn opening_another_project_moves_the_watching_along() {
    // Um core serve um projeto por vez, e a extensão pode trocar a pasta com a
    // engine já de pé. Se o observador ficasse no projeto anterior, ele
    // acabaria entregando a configuração do errado por cima da do certo.
    let first = Project::new();
    first.configure_includes(&[first.include_dir()]);
    let second = Project::new();
    second.configure_includes(&[]);

    let engine = EngineService::new();
    {
        let mut opened = Session::open(&engine, &first);
        assert!(
            !complains_about_the_native(&opened.diagnostics()),
            "o primeiro projeto deveria começar sem queixa"
        );
    }

    // Troca de projeto: a mesma engine, outra pasta.
    let mut moved = Session::open(&engine, &second);
    assert!(
        complains_about_the_native(&moved.diagnostics()),
        "o segundo projeto não tem o include configurado"
    );

    // A edição que importa agora é a do segundo.
    second.configure_includes(&[second.include_dir()]);
    let deadline = Instant::now() + TIMEOUT;
    let mut quiet = false;
    while Instant::now() < deadline {
        if !complains_about_the_native(&moved.diagnostics()) {
            quiet = true;
            break;
        }
    }
    assert!(quiet, "o observador não seguiu para o projeto novo");
}

#[test]
fn two_cores_with_different_folders_do_not_interfere() {
    // Duas janelas do editor, cada uma com a sua pasta: o editor ativa a
    // extensão por janela, e cada uma sobe o seu core. Aqui os dois vivem no
    // mesmo processo, que é pior do que a realidade — se nem assim se
    // atrapalham, dois processos separados não se atrapalham.
    let with_include = Project::new();
    with_include.configure_includes(&[with_include.include_dir()]);
    let without = Project::new();
    without.configure_includes(&[]);

    let first_core = EngineService::new();
    let second_core = EngineService::new();

    let mut first = Session::open(&first_core, &with_include);
    let mut second = Session::open(&second_core, &without);

    // Endereços distintos: um core não pode atender no soquete do outro.
    assert_ne!(
        first_core.address(),
        second_core.address(),
        "os dois cores reservaram o mesmo endereço"
    );

    // Cada um analisa com a configuração da sua pasta.
    assert!(
        !complains_about_the_native(&first.diagnostics()),
        "o core da pasta configurada perdeu os includes dela"
    );
    assert!(
        complains_about_the_native(&second.diagnostics()),
        "o core da pasta sem includes recebeu a configuração da outra"
    );

    // E a edição numa pasta não alcança o core da outra.
    without.configure_includes(&[without.include_dir()]);
    let deadline = Instant::now() + TIMEOUT;
    let mut quiet = false;
    while Instant::now() < deadline {
        if !complains_about_the_native(&second.diagnostics()) {
            quiet = true;
            break;
        }
    }
    assert!(quiet, "a edição não chegou ao core da própria pasta");
}
