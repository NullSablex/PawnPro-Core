//! A conexão da sessão com o plugin que roda dentro do servidor.
//!
//! O plugin conecta no núcleo, e o núcleo entrega a conexão pelo canal que a
//! sessão reservou ao subir o servidor ([`crate::PluginTicket`]). Até ela
//! chegar, os comandos esperam numa fila; quando chega, uma thread lê os
//! eventos do plugin e os traduz em eventos DAP.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use pawnpro_dbg_protocol::messages::{self, Locale, MsgKey};
use pawnpro_dbg_protocol::{self as wire, Command, Event};
use serde_json::json;

use crate::PluginLink;
use crate::dap::DapOut;
use crate::frames::FrameCache;

/// Quanto esperar o plugin conectar.
///
/// O servidor, sobretudo com gamemodes grandes, leva vários segundos para
/// carregar os plugins; passado isto, o problema é outro e o usuário precisa
/// saber.
const CONNECT_TIMEOUT: Duration = Duration::from_mins(1);

/// Quanto esperar a resposta de uma leitura de memória ou de uma escrita.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Trava que continua servindo depois de um panic em outra thread: o que ela
/// protege (uma fila e um mapa) não fica pela metade de um jeito que importe.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lado de escrita: a conexão quando existe, e a fila dos comandos que
/// chegaram antes dela (ex.: `setBreakpoints` enquanto o servidor sobe).
struct Writer {
    link: Option<Box<dyn Write + Send>>,
    pending: Vec<String>,
}

/// O que o plugin responde a um pedido correlacionado.
enum Reply {
    Memory(Vec<u8>),
    Written(bool),
}

/// Pedidos esperando resposta do plugin, por `id`.
#[derive(Default)]
struct Pending {
    next_id: AtomicU64,
    waiting: Mutex<HashMap<u64, Sender<Reply>>>,
}

impl Pending {
    /// Registra um pedido, deixa `send` enviá-lo com o `id` e espera a
    /// resposta. `None` no prazo estourado.
    fn request(&self, send: impl FnOnce(u64)) -> Option<Reply> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&self.waiting).insert(id, tx);
        send(id);
        let reply = rx.recv_timeout(REPLY_TIMEOUT).ok();
        // Na resposta a thread leitora já removeu; no prazo estourado, sobra.
        lock(&self.waiting).remove(&id);
        reply
    }

    fn deliver(&self, id: u64, reply: Reply) {
        let waiting = lock(&self.waiting).remove(&id);
        if let Some(tx) = waiting {
            let _ = tx.send(reply);
        }
    }
}

/// O que a thread do plugin precisa da sessão.
pub struct PluginContext {
    pub out: DapOut,
    pub frames: FrameCache,
    pub locale: Locale,
}

/// A conexão com o plugin de um servidor.
///
/// Soltá-la aposenta a conexão: o que o servidor antigo ainda mandar não chega
/// mais ao editor. É o que impede um `stopped` atrasado do servidor derrubado
/// num restart de aparecer na sessão do servidor novo.
pub struct PluginClient {
    writer: Arc<Mutex<Writer>>,
    pending: Arc<Pending>,
    retired: Arc<AtomicBool>,
}

impl PluginClient {
    /// Passa a esperar o plugin pelo canal dado, numa thread, e retorna na hora.
    #[must_use]
    pub fn attach(link: Receiver<PluginLink>, context: PluginContext) -> Self {
        let client = Self {
            writer: Arc::new(Mutex::new(Writer {
                link: None,
                pending: Vec::new(),
            })),
            pending: Arc::default(),
            retired: Arc::new(AtomicBool::new(false)),
        };
        let connection = Connection {
            writer: Arc::clone(&client.writer),
            pending: Arc::clone(&client.pending),
            retired: Arc::clone(&client.retired),
            context,
        };
        thread::spawn(move || connection.run(&link, CONNECT_TIMEOUT));
        client
    }

    /// Envia um comando ao plugin, ou o enfileira se ele ainda não conectou.
    pub fn send(&self, cmd: &Command) {
        let Ok(line) = wire::to_line(cmd) else { return };
        let mut writer = lock(&self.writer);
        if let Some(link) = writer.link.as_mut() {
            let _ = link.write_all(line.as_bytes()).and_then(|()| link.flush());
        } else {
            writer.pending.push(line);
        }
    }

    /// Lê `count` bytes de memória a partir da variável `name` (elemento
    /// `path`, se array) no `frame`, mais `offset`. Bloqueia até a resposta
    /// correlacionada chegar; `None` no prazo estourado.
    #[must_use]
    pub fn read_memory(
        &self,
        frame: usize,
        name: String,
        path: Vec<usize>,
        offset: i64,
        count: usize,
    ) -> Option<Vec<u8>> {
        let reply = self.pending.request(|id| {
            self.send(&Command::ReadMemory {
                id,
                frame,
                name,
                path,
                offset,
                count,
            });
        });
        match reply? {
            Reply::Memory(bytes) => Some(bytes),
            Reply::Written(_) => None,
        }
    }

    /// Grava `value` na célula da variável `name` (elemento `path`, se array)
    /// no `frame` e espera a confirmação. `false` se o plugin recusou ou não
    /// respondeu no prazo.
    #[must_use]
    pub fn set_variable(&self, frame: usize, name: String, path: Vec<usize>, value: i32) -> bool {
        let reply = self.pending.request(|id| {
            self.send(&Command::SetVariable {
                id,
                frame,
                name,
                path,
                value,
            });
        });
        matches!(reply, Some(Reply::Written(true)))
    }
}

impl Drop for PluginClient {
    fn drop(&mut self) {
        self.retired.store(true, Ordering::Relaxed);
    }
}

/// O lado da thread: espera a conexão e traduz os eventos.
struct Connection {
    writer: Arc<Mutex<Writer>>,
    pending: Arc<Pending>,
    retired: Arc<AtomicBool>,
    context: PluginContext,
}

impl Connection {
    fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Relaxed)
    }

    fn console(&self, category: &str, text: &str) {
        self.context.out.event(
            "output",
            json!({ "category": category, "output": format!("{text}\n") }),
        );
    }

    fn run(self, links: &Receiver<PluginLink>, timeout: Duration) {
        let locale = self.context.locale;
        self.console("console", messages::msg(locale, MsgKey::WaitingForPlugin));

        let link = match links.recv_timeout(timeout) {
            Ok(link) => link,
            Err(RecvTimeoutError::Timeout) => {
                if !self.is_retired() {
                    let seconds = timeout.as_secs().to_string();
                    let text = messages::format(locale, MsgKey::PluginNotConnected, &[&seconds]);
                    self.console("stderr", &text);
                }
                return;
            }
            // A reserva foi solta: a sessão acabou ou trocou de servidor antes
            // de o plugin chegar. Não há a quem avisar.
            Err(RecvTimeoutError::Disconnected) => return,
        };

        let PluginLink { reader, writer } = link;
        {
            let mut state = lock(&self.writer);
            let mut writer = writer;
            for line in state.pending.drain(..) {
                let _ = writer.write_all(line.as_bytes());
            }
            let _ = writer.flush();
            state.link = Some(writer);
        }
        if !self.is_retired() {
            self.console("console", messages::msg(locale, MsgKey::PluginConnected));
        }

        let mut exited = false;
        for received in BufReader::new(reader).lines() {
            let Ok(text) = received else { break };
            if self.is_retired() {
                return;
            }
            if text.trim().is_empty() {
                continue;
            }
            // Linha malformada não derruba a conexão.
            let Ok(event) = wire::from_line::<Event>(&text) else {
                continue;
            };
            if matches!(event, Event::Exited) {
                exited = true;
            }
            self.handle(event);
            if exited {
                break;
            }
        }

        // A conexão caiu sem o plugin se despedir: o servidor morreu. Sem este
        // aviso o editor seguiria mostrando uma sessão de pé sem nada por trás.
        if !exited && !self.is_retired() {
            self.context
                .out
                .event("terminated", serde_json::Value::Null);
        }
    }

    fn handle(&self, event: Event) {
        let out = &self.context.out;
        match event {
            Event::Paused {
                reason,
                frames,
                description,
            } => {
                self.context.frames.replace(frames);
                let mut body = json!({
                    "reason": reason,
                    "threadId": 1,
                    "allThreadsStopped": true,
                });
                // Erro de runtime: `description`/`text` mostram a causa no
                // cabeçalho da call stack do editor (reason "exception").
                if let Some(desc) = description {
                    body["description"] = json!(desc);
                    body["text"] = json!(desc);
                }
                out.event("stopped", body);
            }
            // Logpoint: a VM não pausou — só ecoa no console do editor.
            Event::Output { text } => self.console("console", &text),
            Event::MemoryData { id, bytes } => self.pending.deliver(id, Reply::Memory(bytes)),
            Event::VariableSet { id, ok } => self.pending.deliver(id, Reply::Written(ok)),
            Event::Hello { version } => self.warn_version_mismatch(&version),
            Event::Exited => out.event("terminated", serde_json::Value::Null),
        }
    }

    /// Avisa no console quando o plugin do servidor e o adaptador têm versões
    /// diferentes.
    ///
    /// Versões diferentes conversam até a primeira mensagem que um dos lados
    /// não entende, e o sintoma é a depuração simplesmente não fazer nada.
    /// Dizer qual é a diferença troca uma falha muda por uma instrução.
    fn warn_version_mismatch(&self, plugin: &str) {
        let ours = env!("CARGO_PKG_VERSION");
        if plugin == ours {
            return;
        }
        let text = messages::format(
            self.context.locale,
            MsgKey::PluginVersionMismatch,
            &[plugin, ours, ours],
        );
        self.console("important", &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            lock(&self.0).extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8_lossy(&lock(&self.0)).into_owned()
        }
    }

    fn context(sink: &Sink, locale: Locale) -> PluginContext {
        PluginContext {
            out: DapOut::new(Box::new(sink.clone()), 1),
            frames: FrameCache::default(),
            locale,
        }
    }

    /// Um plugin falso: o que ele escreve chega à conexão, e o que a conexão
    /// escreve para ele fica em `commands`.
    fn fake_plugin(events: &str) -> (PluginLink, Sink) {
        let commands = Sink::default();
        let link = PluginLink {
            reader: Box::new(std::io::Cursor::new(events.as_bytes().to_vec())),
            writer: Box::new(commands.clone()),
        };
        (link, commands)
    }

    fn wait_until(condition: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn line(event: &Event) -> String {
        wire::to_line(event).unwrap()
    }

    #[test]
    fn same_version_does_not_warn() {
        let sink = Sink::default();
        let (tx, rx) = mpsc::channel();
        let hello = line(&Event::Hello {
            version: env!("CARGO_PKG_VERSION").into(),
        });
        let (link, _) = fake_plugin(&(hello + &line(&Event::Exited)));
        tx.send(link).unwrap();
        let client = PluginClient::attach(rx, context(&sink, Locale::En));
        assert!(wait_until(|| sink.text().contains("terminated")));
        assert!(!sink.text().contains("important"));
        drop(client);
    }

    /// O idioma vem da sessão. Antes vinha de uma variável de ambiente que só
    /// o servidor recebia, e o aviso saía sempre em inglês.
    #[test]
    fn version_mismatch_warns_in_the_session_locale() {
        let sink = Sink::default();
        let (tx, rx) = mpsc::channel();
        let hello = line(&Event::Hello {
            version: "0.0.1".into(),
        });
        let (link, _) = fake_plugin(&(hello + &line(&Event::Exited)));
        tx.send(link).unwrap();
        let _client = PluginClient::attach(rx, context(&sink, Locale::PtBr));
        assert!(wait_until(|| sink.text().contains("terminated")));
        let text = sink.text();
        assert!(text.contains("Plugin de depuração 0.0.1"), "{text}");
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
    }

    /// O editor manda breakpoints antes de o servidor terminar de subir: eles
    /// não podem se perder enquanto o plugin não conecta.
    #[test]
    fn commands_sent_before_the_plugin_connects_are_delivered() {
        let sink = Sink::default();
        let (tx, rx) = mpsc::channel();
        let client = PluginClient::attach(rx, context(&sink, Locale::En));
        client.send(&Command::Configured);

        let (link, commands) = fake_plugin("");
        tx.send(link).unwrap();
        assert!(wait_until(|| commands.text().contains("configured")));
    }

    #[test]
    fn pause_fills_the_session_frames() {
        let sink = Sink::default();
        let (tx, rx) = mpsc::channel();
        let ctx = context(&sink, Locale::En);
        let frames = ctx.frames.clone();
        let paused = line(&Event::Paused {
            reason: "breakpoint".into(),
            frames: vec![wire::Frame {
                name: "OnGameModeInit".into(),
                file: None,
                line: Some(7),
                vars: Vec::new(),
            }],
            description: None,
        });
        let (link, _) = fake_plugin(&(paused + &line(&Event::Exited)));
        tx.send(link).unwrap();
        let _client = PluginClient::attach(rx, ctx);
        assert!(wait_until(|| sink.text().contains("stopped")));
        assert_eq!(frames.all()[0].name, "OnGameModeInit");
    }

    /// Servidor que morre sem descarregar o plugin não manda `Exited`: sem o
    /// `terminated` o editor mostraria uma sessão de pé sem servidor.
    #[test]
    fn connection_lost_without_exit_terminates() {
        let sink = Sink::default();
        let (tx, rx) = mpsc::channel();
        let (link, _) = fake_plugin("");
        tx.send(link).unwrap();
        let _client = PluginClient::attach(rx, context(&sink, Locale::En));
        assert!(wait_until(|| sink.text().contains("terminated")));
    }

    /// No restart a conexão antiga é aposentada: o que o servidor derrubado
    /// ainda mandar não pode aparecer na sessão do servidor novo.
    #[test]
    fn retired_client_stays_silent() {
        struct Gate(Receiver<()>, std::io::Cursor<Vec<u8>>);
        impl Read for Gate {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                // Só entrega os bytes depois que o teste aposentou o cliente.
                let _ = self.0.recv();
                self.1.read(buf)
            }
        }

        let sink = Sink::default();
        let (tx, rx) = mpsc::channel();
        let (open_tx, open_rx) = mpsc::channel();
        let paused = line(&Event::Paused {
            reason: "breakpoint".into(),
            frames: Vec::new(),
            description: None,
        });
        tx.send(PluginLink {
            reader: Box::new(Gate(open_rx, std::io::Cursor::new(paused.into_bytes()))),
            writer: Box::new(Sink::default()),
        })
        .unwrap();

        let client = PluginClient::attach(rx, context(&sink, Locale::En));
        assert!(wait_until(|| sink.text().contains("Connected")));
        drop(client);
        drop(open_tx);
        thread::sleep(Duration::from_millis(100));
        let text = sink.text();
        assert!(!text.contains("stopped"), "{text}");
        assert!(!text.contains("terminated"), "{text}");
    }

    #[test]
    fn released_reservation_ends_the_wait_quietly() {
        let sink = Sink::default();
        let (tx, rx) = mpsc::channel::<PluginLink>();
        let client = PluginClient::attach(rx, context(&sink, Locale::En));
        drop(tx);
        drop(client);
        thread::sleep(Duration::from_millis(100));
        assert!(!sink.text().contains("did not connect"));
    }

    #[test]
    fn timeout_explains_what_to_check() {
        let sink = Sink::default();
        let (_tx, rx) = mpsc::channel::<PluginLink>();
        let connection = Connection {
            writer: Arc::new(Mutex::new(Writer {
                link: None,
                pending: Vec::new(),
            })),
            pending: Arc::default(),
            retired: Arc::new(AtomicBool::new(false)),
            context: context(&sink, Locale::En),
        };
        connection.run(&rx, Duration::from_millis(10));
        assert!(sink.text().contains("did not connect"));
    }

    #[test]
    fn memory_read_gets_the_correlated_answer() {
        /// Só responde depois de ver o pedido chegar, como o plugin real.
        struct Answer {
            commands: Sink,
            reply: std::io::Cursor<Vec<u8>>,
        }
        impl Read for Answer {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                assert!(wait_until(|| self.commands.text().contains("readMemory")));
                self.reply.read(buf)
            }
        }

        let sink = Sink::default();
        let (tx, rx) = mpsc::channel();
        let client = PluginClient::attach(rx, context(&sink, Locale::En));
        let commands = Sink::default();
        let reply = line(&Event::MemoryData {
            id: 0,
            bytes: vec![1, 2, 3],
        });
        tx.send(PluginLink {
            reader: Box::new(Answer {
                commands: commands.clone(),
                reply: std::io::Cursor::new(reply.into_bytes()),
            }),
            writer: Box::new(commands),
        })
        .unwrap();

        assert_eq!(
            client.read_memory(0, "x".into(), Vec::new(), 0, 3),
            Some(vec![1, 2, 3])
        );
    }

    /// O painel só mostra o valor novo se o plugin gravou: a resposta de cada
    /// escrita volta pelo `id`, com o resultado dela.
    #[test]
    fn variable_write_waits_for_the_plugin_answer() {
        /// Responde a cada `setVariable` com o `ok` dado, pelo `id` recebido.
        struct Answer {
            commands: Sink,
            ok: bool,
            answered: usize,
            buffer: std::io::Cursor<Vec<u8>>,
        }
        impl Read for Answer {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                loop {
                    let n = self.buffer.read(buf)?;
                    if n > 0 {
                        return Ok(n);
                    }
                    assert!(wait_until(|| self
                        .commands
                        .text()
                        .matches("setVariable")
                        .count()
                        > self.answered));
                    let text = self.commands.text();
                    let command = text
                        .lines()
                        .filter(|l| l.contains("setVariable"))
                        .nth(self.answered)
                        .unwrap();
                    let id = wire::from_line::<Command>(command)
                        .map(|c| match c {
                            Command::SetVariable { id, .. } => id,
                            _ => unreachable!(),
                        })
                        .unwrap();
                    self.answered += 1;
                    self.buffer = std::io::Cursor::new(
                        line(&Event::VariableSet { id, ok: self.ok }).into_bytes(),
                    );
                }
            }
        }

        for ok in [true, false] {
            let sink = Sink::default();
            let (tx, rx) = mpsc::channel();
            let client = PluginClient::attach(rx, context(&sink, Locale::En));
            let commands = Sink::default();
            tx.send(PluginLink {
                reader: Box::new(Answer {
                    commands: commands.clone(),
                    ok,
                    answered: 0,
                    buffer: std::io::Cursor::new(Vec::new()),
                }),
                writer: Box::new(commands),
            })
            .unwrap();
            assert_eq!(client.set_variable(0, "x".into(), vec![], 5), ok);
        }
    }
}
