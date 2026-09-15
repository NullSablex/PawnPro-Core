//! Quem ocupa a porta do servidor, e como encerrá-lo.
//!
//! A porta vem do `config.json` do projeto, que é arquivo do repositório: um
//! gamemode com `"port": 53` tornaria o botão de encerrar uma arma contra
//! serviços do sistema. Por isso toda operação destrutiva passa por
//! [`project_servers_on_port`], que aplica o filtro de dono.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

/// PIDs escutando na porta UDP.
///
/// Vazio quando nenhuma ferramenta está disponível: o chamador trata isso como
/// "não sei", não como "não há".
#[must_use]
pub fn pids_on_port(port: u16) -> Vec<u32> {
    let raw = if cfg!(windows) {
        windows_udp_pids(port)
    } else {
        unix_udp_pids(port)
    };
    let mine = std::process::id();
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter(|pid| *pid > 1 && *pid != mine && seen.insert(*pid))
        .collect()
}

/// Só o stdout: o `fuser` manda o rótulo `7777/udp:` para o stderr, e lê-lo
/// junto faria a porta virar um PID candidato.
fn unix_udp_pids(port: u16) -> Vec<u32> {
    let attempts: [(&str, Vec<String>); 2] = [
        ("lsof", vec!["-ti".into(), format!("udp:{port}")]),
        ("fuser", vec!["-n".into(), "udp".into(), port.to_string()]),
    ];
    for (exe, args) in attempts {
        let Ok(out) = Command::new(exe).args(&args).output() else {
            continue;
        };
        let pids: Vec<u32> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        if !pids.is_empty() {
            return pids;
        }
    }
    Vec::new()
}

/// O `-n` evita a resolução de nomes, que traria `*:domain` no lugar da porta.
fn windows_udp_pids(port: u16) -> Vec<u32> {
    let Ok(out) = Command::new("netstat").args(["-ano", "-p", "UDP"]).output() else {
        return Vec::new();
    };
    let suffix = format!(":{port}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 3 || !cols[0].eq_ignore_ascii_case("UDP") {
                return None;
            }
            // Sufixo com `:` cobre `0.0.0.0:7777` e `[::]:7777`, e impede
            // `17777` de casar com `7777`.
            if !cols[1].ends_with(&suffix) {
                return None;
            }
            cols.last()?.parse().ok()
        })
        .collect()
}

/// `true` se o processo é o executável deste projeto e roda sob o mesmo usuário.
///
/// O executável impede encerrar um serviço alheio que esteja na porta; o dono
/// impede prometer um encerramento que o sistema recusaria. Na dúvida, `false`.
#[must_use]
pub fn is_project_server(pid: u32, server_exe: &Path) -> bool {
    if server_exe.as_os_str().is_empty() {
        return false;
    }
    let Ok(target) = std::fs::canonicalize(server_exe) else {
        return false;
    };

    let target_pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[target_pid]),
        true,
        ProcessRefreshKind::nothing()
            .with_exe(sysinfo::UpdateKind::Always)
            .with_user(sysinfo::UpdateKind::Always),
    );
    let Some(proc) = sys.process(target_pid) else {
        return false;
    };

    // O dono primeiro: é a checagem barata.
    if proc.user_id() != current_user_id().as_ref() {
        return false;
    }

    // `canonicalize` dos dois lados: o caminho reportado pode vir por um
    // symlink diferente do configurado.
    proc.exe()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .is_some_and(|real| paths_match(&real, &target))
}

/// Varredura própria porque o alvo pode ser este mesmo processo: pedir os dois
/// PIDs juntos passaria um valor duplicado ao `sysinfo`, que não devolve nada.
fn current_user_id() -> Option<sysinfo::Uid> {
    let me = sysinfo::get_current_pid().ok()?;
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[me]),
        true,
        ProcessRefreshKind::nothing().with_user(sysinfo::UpdateKind::Always),
    );
    sys.process(me)?.user_id().cloned()
}

/// No Windows o sistema de arquivos ignora a caixa; comparar sem normalizar
/// daria falso negativo.
fn paths_match(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// PIDs na porta que são comprovadamente o servidor deste projeto.
///
/// Busca e filtro numa função só, para que nenhum chamador possa esquecer o
/// filtro.
#[must_use]
pub fn project_servers_on_port(port: u16, server_exe: &Path) -> Vec<u32> {
    pids_on_port(port)
        .into_iter()
        .filter(|pid| is_project_server(*pid, server_exe))
        .collect()
}

/// Encerra um processo: pedido gracioso primeiro, força depois do prazo.
///
/// O gracioso dá ao servidor a chance de salvar e desligar os componentes.
/// `false` se ele não morreu — de outro usuário, ou travado.
#[must_use]
pub fn kill_process(pid: u32, timeout: Duration) -> bool {
    let pid = Pid::from_u32(pid);
    signal(pid, false);

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    signal(pid, true);
    std::thread::sleep(Duration::from_millis(300));
    !is_alive(pid)
}

/// Um zumbi não conta como vivo: já morreu e só ocupa a tabela até o pai
/// colhê-lo. Tratá-lo como vivo faria o encerramento gastar o prazo inteiro e
/// reportar falha depois de ter funcionado.
fn is_alive(pid: Pid) -> bool {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .is_some_and(|p| p.status() != sysinfo::ProcessStatus::Zombie)
}

/// No Windows não há sinal gracioso: um `kill` encerra na hora e o servidor não
/// salva nada. O `taskkill` sem `/F` é o equivalente ao `SIGTERM`.
fn signal(pid: Pid, force: bool) {
    if cfg!(windows) {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.as_u32().to_string()]);
        if force {
            cmd.args(["/T", "/F"]);
        }
        let _ = cmd.output();
        return;
    }

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    if let Some(proc) = sys.process(pid) {
        let sig = if force { Signal::Kill } else { Signal::Term };
        // `kill_with` devolve `None` quando o sinal não existe na plataforma;
        // `is_alive` dá a resposta de qualquer forma.
        let _ = proc.kill_with(sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_process_is_never_listed() {
        // O core não pode se encerrar por engano.
        assert!(!pids_on_port(7777).contains(&std::process::id()));
    }

    #[test]
    fn an_unlikely_port_has_no_owner() {
        assert!(pids_on_port(59991).is_empty());
    }

    #[test]
    fn an_empty_executable_matches_nothing() {
        // Sem executável configurado não há como saber de quem é o processo.
        assert!(!is_project_server(std::process::id(), Path::new("")));
    }

    #[test]
    fn a_missing_pid_matches_nothing() {
        let exe = std::env::current_exe().expect("exe do teste");
        assert!(!is_project_server(0x7fff_fff0, &exe));
    }

    #[test]
    fn a_different_executable_does_not_match() {
        // O que impede um `"port": 53` no config.json de encerrar um serviço
        // do sistema.
        assert!(!is_project_server(std::process::id(), Path::new("/bin/sh")));
    }

    #[test]
    fn our_own_process_matches_its_own_executable() {
        // Mesmo binário, mesmo usuário: é o caso positivo do filtro.
        let exe = std::env::current_exe().expect("exe do teste");
        assert!(is_project_server(std::process::id(), &exe));
    }

    #[test]
    fn project_servers_applies_the_owner_filter() {
        // Sem executável nada é devolvido, mesmo que a porta esteja ocupada.
        assert!(project_servers_on_port(7777, Path::new("")).is_empty());
    }

    #[test]
    fn killing_a_missing_process_reports_success() {
        // Já não existe: o objetivo — o processo fora do ar — está cumprido.
        assert!(kill_process(0x7fff_fff0, Duration::from_millis(300)));
    }

    #[cfg(unix)]
    #[test]
    fn a_real_child_is_terminated() {
        // `sleep` ignora nada e morre no SIGTERM: confirma o caminho gracioso.
        let child = Command::new("sleep").arg("30").spawn();
        let Ok(mut child) = child else {
            return; // sem `sleep` no ambiente
        };
        let pid = child.id();
        assert!(kill_process(pid, Duration::from_secs(3)));
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn a_zombie_does_not_count_as_alive() {
        // O filho morre mas fica na tabela até o pai colhê-lo. Tratá-lo como
        // vivo faria o encerramento gastar o prazo inteiro e reportar falha
        // mesmo tendo funcionado.
        let Ok(mut child) = Command::new("true").spawn() else {
            return;
        };
        let pid = Pid::from_u32(child.id());
        // Espera o processo terminar SEM colhê-lo: vira zumbi. Com prazo fixo
        // este teste falhava de vez em quando — sob carga, 300 ms podiam não
        // bastar para o filho morrer, e a falha não tinha a ver com zumbis.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while is_alive(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!is_alive(pid), "zumbi contado como vivo");
        let _ = child.wait();
    }

    #[test]
    fn paths_are_compared_case_sensitively_off_windows() {
        let a = Path::new("/A/B");
        let b = Path::new("/a/b");
        assert_eq!(paths_match(a, b), cfg!(windows));
    }
}
