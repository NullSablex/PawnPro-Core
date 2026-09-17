//! Uma sessão inteira pelo laço público: editor → adaptador → servidor → plugin.
//!
//! O núcleo e o plugin são falsos; o servidor é um processo de verdade, para o
//! teste ver o que acontece com ele no restart e no fim da sessão.

#![cfg(target_os = "linux")]

use std::io::{PipeWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pawnpro_dap_adapter::{PluginHost, PluginLink, PluginTicket, serve};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Sink {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Solta a reserva ao cair, como o núcleo faz.
struct Registration(Arc<AtomicBool>);

impl Drop for Registration {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct FakeCore {
    reserved: AtomicUsize,
    /// O canal da reserva mais recente e o aviso de que ela foi solta.
    latest: Mutex<Option<(Sender<PluginLink>, Arc<AtomicBool>)>>,
}

impl PluginHost for FakeCore {
    fn reserve(&self) -> std::io::Result<PluginTicket> {
        let id = self.reserved.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let released = Arc::new(AtomicBool::new(false));
        *self.latest.lock().unwrap() = Some((tx, Arc::clone(&released)));
        Ok(PluginTicket {
            endpoint: "/run/fake-core.sock".into(),
            session: id.to_string(),
            link: rx,
            registration: Box::new(Registration(released)),
        })
    }
}

fn wait_until(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn request(editor: &mut PipeWriter, seq: i64, command: &str, arguments: &Value) {
    let body = json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments })
        .to_string();
    write!(editor, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    editor.flush().unwrap();
}

fn is_alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.contains(") Z "))
}

/// PIDs que o servidor falso anunciou no console, na ordem.
fn announced_pids(output: &str) -> Vec<u32> {
    output
        .split("server-pid=")
        .skip(1)
        .filter_map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        })
        .collect()
}

/// O plugin falso: devolve a ponta por onde o teste manda eventos e o que o
/// adaptador escreveu para ele.
fn connect_plugin(core: &FakeCore) -> (PipeWriter, Sink) {
    let (reader, writer) = std::io::pipe().unwrap();
    let commands = Sink::default();
    let (tx, _) = core.latest.lock().unwrap().clone().unwrap();
    tx.send(PluginLink {
        reader: Box::new(reader),
        writer: Box::new(commands.clone()),
    })
    .unwrap();
    (writer, commands)
}

#[test]
fn restart_and_disconnect_manage_server_and_reservation() {
    let core = FakeCore::default();
    let output = Sink::default();
    let (input, editor) = std::io::pipe().unwrap();

    thread::scope(|scope| {
        // Dentro do escopo: se uma asserção falhar, o editor cai no unwind, a
        // sessão lê EOF e sai, e o teste falha em vez de travar.
        let mut editor = editor;
        let session = scope.spawn(|| serve(input, Box::new(output.clone()), &core));

        request(&mut editor, 1, "initialize", &json!({ "locale": "en" }));
        request(
            &mut editor,
            2,
            "launch",
            &json!({
                "program": "/tmp/pawnpro-missing.amx",
                "serverCommand": {
                    "exe": "sh",
                    // `exec` mantém o PID anunciado.
                    "args": ["-c", "echo server-pid=$$; exec sleep 30"],
                    "cwd": ""
                }
            }),
        );
        assert!(wait_until(|| announced_pids(&output.text()).len() == 1));
        let first_pid = announced_pids(&output.text())[0];
        let first_released = core.latest.lock().unwrap().as_ref().unwrap().1.clone();

        // O plugin chega, recebe a configuração e pausa.
        let (mut plugin, commands) = connect_plugin(&core);
        request(&mut editor, 3, "configurationDone", &json!({}));
        assert!(wait_until(|| commands.text().contains("configured")));
        writeln!(
            plugin,
            "{}",
            json!({ "event": "paused", "reason": "breakpoint", "frames": [] })
        )
        .unwrap();
        assert!(wait_until(|| output.text().contains("\"stopped\"")));

        // Restart: servidor novo, reserva nova, e o velho some por inteiro.
        request(&mut editor, 4, "restart", &json!({}));
        assert!(wait_until(|| announced_pids(&output.text()).len() == 2));
        let second_pid = announced_pids(&output.text())[1];
        assert!(!is_alive(first_pid), "o servidor antigo tem de cair");
        assert!(is_alive(second_pid));
        assert!(
            first_released.load(Ordering::Relaxed),
            "e a reserva dele tem de ser solta"
        );
        assert_eq!(core.reserved.load(Ordering::Relaxed), 2);

        // O servidor derrubado ainda fala: nada disso pode chegar ao editor.
        let stopped_before = output.text().matches("\"stopped\"").count();
        let _ = writeln!(
            plugin,
            "{}",
            json!({ "event": "paused", "reason": "breakpoint", "frames": [] })
        );
        drop(plugin);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(output.text().matches("\"stopped\"").count(), stopped_before);
        assert!(!output.text().contains("\"terminated\""));

        let second_released = core.latest.lock().unwrap().as_ref().unwrap().1.clone();
        // Parar pelo editor: `terminate` derruba o servidor e mantém a sessão,
        // e o `disconnect` seguinte ainda recebe resposta.
        request(&mut editor, 5, "terminate", &json!({}));
        assert!(
            wait_until(|| !is_alive(second_pid)),
            "o terminate derruba o servidor"
        );
        assert!(second_released.load(Ordering::Relaxed));
        request(&mut editor, 6, "disconnect", &json!({}));
        assert!(wait_until(|| output
            .text()
            .contains("\"command\":\"disconnect\"")));
        session.join().unwrap().unwrap();
        assert_eq!(output.text().matches("\"terminated\"").count(), 1);
    });
}

#[test]
fn server_that_cannot_start_ends_the_session() {
    let core = FakeCore::default();
    let output = Sink::default();
    let (input, editor) = std::io::pipe().unwrap();

    thread::scope(|scope| {
        // Dentro do escopo: se uma asserção falhar, o editor cai no unwind, a
        // sessão lê EOF e sai, e o teste falha em vez de travar.
        let mut editor = editor;
        let session = scope.spawn(|| serve(input, Box::new(output.clone()), &core));
        request(
            &mut editor,
            1,
            "launch",
            &json!({
                "program": "/tmp/gm.amx",
                "serverCommand": { "exe": "/nonexistent/pawnpro-server", "args": [], "cwd": "" }
            }),
        );
        assert!(wait_until(|| output.text().contains("\"terminated\"")));
        assert!(output.text().contains("Failed to start the server"));
        drop(editor);
        session.join().unwrap().unwrap();
    });
}

/// Editar uma variável só responde sucesso quando o plugin confirma a escrita;
/// recusada, o editor recebe o erro e o painel continua com o valor real.
#[test]
fn variable_edit_follows_the_plugin_answer() {
    let core = FakeCore::default();
    let output = Sink::default();
    let (input, editor) = std::io::pipe().unwrap();

    thread::scope(|scope| {
        // Dentro do escopo: se uma asserção falhar, o editor cai no unwind, a
        // sessão lê EOF e sai, e o teste falha em vez de travar.
        let mut editor = editor;
        let session = scope.spawn(|| serve(input, Box::new(output.clone()), &core));
        request(
            &mut editor,
            1,
            "launch",
            &json!({
                "program": "/tmp/pawnpro-missing.amx",
                "serverCommand": { "exe": "sh", "args": ["-c", "exec sleep 30"], "cwd": "" }
            }),
        );
        assert!(wait_until(|| core.latest.lock().unwrap().is_some()));
        let (mut plugin, commands) = connect_plugin(&core);
        writeln!(
            plugin,
            "{}",
            json!({ "event": "paused", "reason": "breakpoint", "frames": [
                { "name": "main", "line": 1, "vars": [ { "name": "x", "value": "5" } ] }
            ] })
        )
        .unwrap();
        assert!(wait_until(|| output.text().contains("\"stopped\"")));

        let answer = |plugin: &mut PipeWriter, nth: usize, ok: bool| {
            assert!(wait_until(|| commands
                .text()
                .matches("setVariable")
                .count()
                > nth));
            let text = commands.text();
            let command: Value = serde_json::from_str(
                text.lines()
                    .filter(|l| l.contains("setVariable"))
                    .nth(nth)
                    .unwrap(),
            )
            .unwrap();
            writeln!(
                plugin,
                "{}",
                json!({ "event": "variableSet", "id": command["id"], "ok": ok })
            )
            .unwrap();
        };
        let responses = || {
            output
                .text()
                .split("Content-Length")
                .filter(|m| m.contains("\"command\":\"setVariable\""))
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };

        request(
            &mut editor,
            2,
            "setVariable",
            &json!({ "variablesReference": 1, "name": "x", "value": "9" }),
        );
        answer(&mut plugin, 0, false);
        assert!(wait_until(|| responses().len() == 1));
        assert!(
            responses()[0].contains("\"success\":false"),
            "{}",
            responses()[0]
        );
        assert!(responses()[0].contains("could not write 'x'"));

        // O painel continua com o valor real.
        request(
            &mut editor,
            3,
            "variables",
            &json!({ "variablesReference": 1 }),
        );
        assert!(wait_until(|| output.text().contains("\"value\":\"5\"")));

        request(
            &mut editor,
            4,
            "setVariable",
            &json!({ "variablesReference": 1, "name": "x", "value": "9" }),
        );
        answer(&mut plugin, 1, true);
        assert!(wait_until(|| responses().len() == 2));
        assert!(responses()[1].contains("\"success\":true"));
        assert!(responses()[1].contains("\"value\":\"9\""));

        request(&mut editor, 5, "disconnect", &json!({}));
        session.join().unwrap().unwrap();
    });
}
