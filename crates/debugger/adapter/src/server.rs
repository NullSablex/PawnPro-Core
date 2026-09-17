//! O servidor do jogo como processo filho da sessão.
//!
//! O servidor vive exatamente o tempo da sessão que o subiu: cai no `Drop` do
//! [`ServerChild`], e no Linux também quando a thread da sessão morre sem
//! passar pelo `Drop` (ver [`spawn_server`]).

use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};

use pawnpro_dbg_protocol::transport::env;

use crate::dap::DapOut;
use crate::session::SpawnSpec;

/// O servidor em execução. Soltá-lo mata o processo e espera ele sair.
pub struct ServerChild(Child);

impl Drop for ServerChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Sobe o servidor com o que o plugin precisa para achar a sessão.
///
/// `stdout`/`stderr` do servidor vão ao console de depuração do editor como
/// eventos `output`, para o desenvolvedor ver os `print` do gamemode sem um
/// terminal à parte.
///
/// No Linux o filho recebe `PR_SET_PDEATHSIG`: se a sessão morrer sem o `Drop`
/// rodar, o kernel mata o servidor. O sinal segue a **thread** que criou o
/// filho, não o processo — por isso esta função precisa ser chamada pela
/// thread que vive a sessão inteira. Numa thread de passagem, o servidor
/// morreria assim que ela terminasse.
///
/// # Errors
/// Falha do sistema ao criar o processo.
pub fn spawn_server(
    spec: &SpawnSpec,
    endpoint: &str,
    session: &str,
    out: &DapOut,
) -> io::Result<ServerChild> {
    let mut cmd = Command::new(&spec.exe);
    cmd.args(&spec.args)
        .env(env::ENDPOINT, endpoint)
        .env(env::SESSION, session)
        .env(env::AMX_DEBUG, &spec.amx_path)
        .env(env::LOCALE, &spec.locale)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !spec.cwd.is_empty() {
        cmd.current_dir(&spec.cwd);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: entre o `fork` e o `exec` só se chama `prctl`, que é seguro
        // nesse intervalo (não aloca nem toma travas).
        unsafe {
            cmd.pre_exec(|| {
                kill_with_parent();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;

    // Uma thread por fluxo. Terminam sozinhas no EOF, quando o servidor morre
    // e a pipe fecha.
    if let Some(stdout) = child.stdout.take() {
        forward_stream(stdout, "stdout", out.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        forward_stream(stderr, "stderr", out.clone());
    }
    Ok(ServerChild(child))
}

/// `prctl(PR_SET_PDEATHSIG, SIGKILL)`, sem dependência externa.
#[cfg(target_os = "linux")]
fn kill_with_parent() {
    const PR_SET_PDEATHSIG: i32 = 1;
    const SIGKILL: i32 = 9;
    unsafe extern "C" {
        fn prctl(option: i32, ...) -> i32;
    }
    // SAFETY: `prctl` com `PR_SET_PDEATHSIG` recebe um único inteiro e não
    // toca memória do chamador.
    unsafe {
        prctl(PR_SET_PDEATHSIG, SIGKILL);
    }
}

fn forward_stream<R: Read + Send + 'static>(stream: R, category: &'static str, out: DapOut) {
    std::thread::spawn(move || pump_stream(stream, category, &out));
}

/// Lê `stream` linha a linha e emite cada uma como `output`. `read_until`
/// (em vez de `lines()`) preserva a quebra de linha e a última linha sem `\n`.
fn pump_stream<R: Read>(stream: R, category: &'static str, out: &DapOut) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    while let Ok(n) = reader.read_until(b'\n', &mut buf) {
        if n == 0 {
            break;
        }
        out.event(
            "output",
            serde_json::json!({ "category": category, "output": decode_console(&buf) }),
        );
        buf.clear();
    }
}

/// Bytes do console do servidor em texto.
///
/// O SA-MP/open.mp escreve o console em Windows-1252, não em UTF-8: em
/// português `ção` sai como `\xe7\xe3o`, que `from_utf8_lossy` trocaria por `�`.
/// Tentamos UTF-8 primeiro — é o que um gamemode moderno pode emitir — e só
/// caímos no cp1252 quando a sequência não é UTF-8 válida.
///
/// Em cp1252 os bytes 0xA0–0xFF já são os mesmos code points Unicode (herança
/// do Latin-1); apenas 0x80–0x9F têm tabela própria.
fn decode_console(bytes: &[u8]) -> String {
    /// Os 32 code points de 0x80–0x9F, onde cp1252 difere do Latin-1.
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2C6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8D}', '\u{17D}',
        '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}',
        '\u{2014}', '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\u{9D}', '\u{17E}',
        '\u{178}',
    ];
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => HIGH[usize::from(b - 0x80)],
            _ => char::from(b),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[test]
    fn console_decodes_windows_1252() {
        // Bytes como o servidor os escreve: "ção" = 0xE7 0xE3 0x6F.
        let raw = b"deslocamento=4 (acentua\xe7\xe3o: cora\xe7\xe3o)\n";
        assert_eq!(
            decode_console(raw),
            "deslocamento=4 (acentuação: coração)\n"
        );
    }

    /// UTF-8 válido tem prioridade: um gamemode moderno pode emitir UTF-8, e
    /// interpretá-lo como cp1252 daria mojibake ao contrário.
    #[test]
    fn console_keeps_valid_utf8() {
        assert_eq!(decode_console("ação".as_bytes()), "ação");
        assert_eq!(decode_console(b"plain ascii"), "plain ascii");
    }

    #[derive(Clone)]
    struct SharedSink(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn pump_emits_one_output_event_per_line() {
        let sink = SharedSink(Arc::new(Mutex::new(Vec::new())));
        let out = DapOut::new(Box::new(sink.clone()), 0);

        // Uma linha normal, uma com byte não-UTF-8 (0xFF) e uma final sem `\n`
        // (o servidor pode morrer no meio de uma linha).
        pump_stream(&b"alpha\n\xff\nbeta"[..], "stdout", &out);

        let raw = String::from_utf8_lossy(&sink.0.lock().unwrap()).into_owned();
        assert_eq!(raw.matches("\"event\":\"output\"").count(), 3);
        assert!(raw.contains("alpha\\n"));
        assert!(raw.contains("beta"));
        assert!(raw.contains("\"category\":\"stdout\""));
        // Cai na leitura cp1252, onde 0xFF é `ÿ`, em vez de virar `\u{FFFD}`.
        assert!(raw.contains('ÿ'));
        assert!(!raw.contains('\u{fffd}'));
    }

    #[cfg(target_os = "linux")]
    fn is_alive(pid: u32) -> bool {
        // Um processo morto mas não colhido continua em `/proc` como zumbi.
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !stat.contains(") Z "))
    }

    #[cfg(target_os = "linux")]
    fn spec(exe: &str, args: &[&str]) -> SpawnSpec {
        SpawnSpec {
            exe: exe.into(),
            args: args.iter().map(ToString::to_string).collect(),
            cwd: String::new(),
            amx_path: "/tmp/gm.amx".into(),
            locale: "pt-BR".into(),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dropping_the_child_kills_the_server() {
        let out = DapOut::new(Box::new(io::sink()), 0);
        let child = spawn_server(&spec("sleep", &["30"]), "/x", "1", &out).unwrap();
        let pid = child.0.id();
        assert!(is_alive(pid));
        drop(child);
        assert!(!is_alive(pid));
    }

    /// O plugin acha a sessão pelo ambiente: sem estas variáveis, ele não tem
    /// onde conectar.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_server_receives_what_the_plugin_needs() {
        let sink = SharedSink(Arc::new(Mutex::new(Vec::new())));
        let out = DapOut::new(Box::new(sink.clone()), 0);
        let script = format!(
            "echo ${}:${}:${}:${}",
            env::ENDPOINT,
            env::SESSION,
            env::AMX_DEBUG,
            env::LOCALE
        );
        let child =
            spawn_server(&spec("sh", &["-c", &script]), "/run/core.sock", "7", &out).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let expected = "/run/core.sock:7:/tmp/gm.amx:pt-BR";
        while std::time::Instant::now() < deadline
            && !String::from_utf8_lossy(&sink.0.lock().unwrap()).contains(expected)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(child);
        assert!(String::from_utf8_lossy(&sink.0.lock().unwrap()).contains(expected));
    }

    /// O `PR_SET_PDEATHSIG` segue a thread que criou o filho: é a razão de a
    /// sessão inteira rodar numa thread só. Se a thread que subiu o servidor
    /// morrer, ele cai junto, mesmo sem `Drop`.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_server_dies_with_the_thread_that_spawned_it() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let out = DapOut::new(Box::new(io::sink()), 0);
            let child = spawn_server(&spec("sleep", &["30"]), "/x", "1", &out).unwrap();
            let pid = child.0.id();
            // Sem `Drop`: simula a sessão morrendo sem limpeza.
            std::mem::forget(child);
            tx.send(pid).unwrap();
        })
        .join()
        .unwrap();
        let pid = rx.recv().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && is_alive(pid) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!is_alive(pid));
    }
}
