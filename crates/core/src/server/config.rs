//! Descoberta do servidor e leitura da sua configuração.
//!
//! Dois formatos incompatíveis: `server.cfg` (`chave valor` por linha) no
//! SA-MP e `config.json` no open.mp. Deles saem porta, host e senha de RCON.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::types::{ServerConfig, ServerType};
use crate::server::types::SampCfgData;

/// Nomes de executável de servidor, por plataforma.
fn server_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["omp-server.exe", "samp-server.exe", "samp03svr.exe"]
    } else {
        &["omp-server", "samp03svr", "samp-server"]
    }
}

/// Pastas onde o servidor costuma ficar, relativas à raiz do projeto.
const SERVER_DIRS: [&str; 6] = ["", "server", "samp", "samp-server", "samp03", "open.mp"];

/// Porta padrão do SA-MP e do open.mp.
const DEFAULT_PORT: u16 = 7777;

/// Localiza o executável do servidor dentro do projeto.
///
/// `None` quando não há nenhum: o usuário então configura `server.path` à mão.
#[must_use]
pub fn detect_server_executable(workspace_root: &Path) -> Option<PathBuf> {
    if workspace_root.as_os_str().is_empty() {
        return None;
    }
    SERVER_DIRS
        .iter()
        .map(|dir| {
            if dir.is_empty() {
                workspace_root.to_path_buf()
            } else {
                workspace_root.join(dir)
            }
        })
        .flat_map(|dir| server_names().iter().map(move |name| dir.join(name)))
        .find(|c| crate::compiler::detect::is_executable(c))
}

/// `0.0.0.0` é o curinga "todas as interfaces": como destino não serve.
fn host_from_bind(bind: &str) -> String {
    let bind = bind.trim();
    if bind.is_empty() || bind == "0.0.0.0" {
        "127.0.0.1".to_string()
    } else {
        bind.to_string()
    }
}

/// Porta válida, com o padrão para o que não for número.
fn port_or_default(raw: &str) -> u16 {
    raw.trim()
        .parse()
        .ok()
        .filter(|p| *p > 0)
        .unwrap_or(DEFAULT_PORT)
}

/// Lê o `server.cfg` do SA-MP.
///
/// Arquivo ausente devolve os padrões: o painel avisa quando a conexão falha.
#[must_use]
pub fn load_samp_config(cwd: &Path) -> SampCfgData {
    let cfg_path = cwd.join("server.cfg");
    let mut rcon_password = String::new();
    let mut port = String::new();
    let mut bind = String::new();

    if let Ok(text) = std::fs::read_to_string(&cfg_path) {
        for raw in text.lines() {
            // Comentários: `;` e `#` do formato, e `//` que aparece na prática.
            let line = raw
                .split([';', '#'])
                .next()
                .unwrap_or("")
                .split("//")
                .next()
                .unwrap_or("")
                .trim();
            let mut parts = line.split_whitespace();
            let Some(key) = parts.next() else {
                continue;
            };
            let value = parts.collect::<Vec<_>>().join(" ");
            match key.to_lowercase().as_str() {
                "rcon_password" => rcon_password = value,
                "port" => port = value,
                "bind" => bind = value,
                _ => {}
            }
        }
    }

    SampCfgData {
        rcon_password,
        port: port_or_default(&port),
        host: host_from_bind(&bind),
        cfg_path,
        // O SA-MP não tem chave equivalente: o RCON está sempre disponível.
        rcon_enabled: true,
    }
}

/// Lê o `config.json` do open.mp.
#[must_use]
pub fn load_omp_config(cwd: &Path) -> SampCfgData {
    let cfg_path = cwd.join("config.json");
    let json: Value = std::fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);

    let rcon = json.get("rcon");
    let network = json.get("network");

    SampCfgData {
        rcon_password: rcon
            .and_then(|r| r.get("password"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        port: network
            .and_then(|n| n.get("port"))
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p > 0)
            .unwrap_or(DEFAULT_PORT),
        host: host_from_bind(
            network
                .and_then(|n| n.get("bind"))
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        cfg_path,
        // Sem ler isto, a extensão manda pacotes para quem não escuta e o
        // timeout passa calado.
        rcon_enabled: rcon
            .and_then(|r| r.get("enable"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
    }
}

/// Decide se `cwd` é um servidor open.mp ou SA-MP.
///
/// A presença de `config.json` não basta: o open.mp só o gera na primeira
/// execução, e outras ferramentas usam esse nome. Daí a ordem abaixo, do sinal
/// mais forte ao mais fraco.
#[must_use]
pub fn detect_server_type(cwd: &Path) -> ServerType {
    let exe = if cfg!(windows) { ".exe" } else { "" };

    // 1. Executável: nomeia o servidor sem ambiguidade.
    if cwd.join(format!("omp-server{exe}")).exists() {
        return ServerType::Omp;
    }
    if cwd.join(format!("samp03svr{exe}")).exists()
        || cwd.join(format!("samp-server{exe}")).exists()
    {
        return ServerType::Samp;
    }

    // 2. `components/`: diretório exclusivo do open.mp.
    if cwd.join("components").is_dir() {
        return ServerType::Omp;
    }

    // 3. `config.json` com as chaves que só o open.mp escreve.
    if is_omp_config_file(&cwd.join("config.json")) {
        return ServerType::Omp;
    }

    // 4. O palpite final: é o formato mais antigo e mais comum.
    ServerType::Samp
}

/// `true` se o arquivo é mesmo o `config.json` do open.mp, e não um homônimo.
fn is_omp_config_file(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(json)) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    // Chaves de topo que só o open.mp escreve.
    ["pawn", "rcon", "network", "logging", "max_players"]
        .iter()
        .any(|k| json.contains_key(*k))
}

/// Lê a configuração do servidor, escolhendo o formato pelo tipo.
///
/// `Auto` detecta pelo conteúdo da pasta.
#[must_use]
pub fn load_server_config(cwd: &Path, server_type: ServerType) -> SampCfgData {
    let resolved = match server_type {
        ServerType::Auto => detect_server_type(cwd),
        other => other,
    };
    match resolved {
        ServerType::Omp => load_omp_config(cwd),
        _ => load_samp_config(cwd),
    }
}

/// Configuração do servidor com os caminhos já resolvidos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedServer {
    pub exe: Option<PathBuf>,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub clear_on_start: bool,
    pub log_path: Option<PathBuf>,
    pub log_encoding: String,
    pub follow: crate::config::types::FollowMode,
}

/// Resolve executável, diretório e log.
///
/// O configurado tem prioridade; o resto é descoberto. O `cwd` segue o
/// executável: o servidor espera rodar da própria pasta, e de outro lugar os
/// caminhos relativos dele quebram.
#[must_use]
pub fn resolve_server_config(config: &ServerConfig, workspace_root: &Path) -> ResolvedServer {
    let exe = if config.path.is_empty() {
        detect_server_executable(workspace_root)
    } else {
        Some(PathBuf::from(&config.path))
    };

    let cwd = if config.cwd.is_empty() {
        exe.as_ref()
            .and_then(|e| e.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| workspace_root.to_path_buf())
    } else {
        PathBuf::from(&config.cwd)
    };

    let log_path = if config.log_path.is_empty() {
        (!cwd.as_os_str().is_empty()).then(|| resolve_log_path(&cwd, config.server_type))
    } else {
        Some(PathBuf::from(&config.log_path))
    };

    ResolvedServer {
        exe,
        cwd,
        args: config.args.clone(),
        clear_on_start: config.clear_on_start,
        log_path,
        log_encoding: if config.log_encoding.is_empty() {
            "windows1252".to_string()
        } else {
            config.log_encoding.to_lowercase()
        },
        follow: config.output.follow,
    }
}

/// Caminho do arquivo de log, conforme o tipo de servidor.
fn resolve_log_path(cwd: &Path, server_type: ServerType) -> PathBuf {
    let resolved = match server_type {
        ServerType::Auto => detect_server_type(cwd),
        other => other,
    };
    match resolved {
        ServerType::Omp => cwd.join(omp_log_file(cwd)),
        _ => cwd.join("server_log.txt"),
    }
}

/// O nome do log é configurável no open.mp.
fn omp_log_file(cwd: &Path) -> String {
    std::fs::read_to_string(cwd.join("config.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|json| {
            json.get("logging")?
                .get("file")?
                .as_str()
                .map(ToString::to_string)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "log.txt".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            p.push(format!("pawnpro-srv-{tag}-{nanos}"));
            std::fs::create_dir_all(&p).expect("criar temp");
            Self(p)
        }
        fn file(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(name), body).expect("escrever");
        }
        fn dir(&self, name: &str) {
            std::fs::create_dir_all(self.0.join(name)).expect("criar dir");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn samp_cfg_is_parsed() {
        let tmp = TempDir::new("samp");
        tmp.file(
            "server.cfg",
            "rcon_password segredo\nport 7778\nbind 192.168.0.5\n",
        );
        let cfg = load_samp_config(&tmp.0);
        assert_eq!(cfg.rcon_password, "segredo");
        assert_eq!(cfg.port, 7778);
        assert_eq!(cfg.host, "192.168.0.5");
        // O SA-MP não tem chave de RCON: está sempre disponível.
        assert!(cfg.rcon_enabled);
    }

    #[test]
    fn samp_cfg_comments_are_stripped() {
        let tmp = TempDir::new("samp-comments");
        tmp.file(
            "server.cfg",
            "; comentário\nport 7000 # depois\nrcon_password x // fim\n",
        );
        let cfg = load_samp_config(&tmp.0);
        assert_eq!(cfg.port, 7000);
        assert_eq!(cfg.rcon_password, "x");
    }

    #[test]
    fn a_missing_samp_cfg_yields_the_defaults() {
        // Entrega o que sabe em vez de falhar: o painel avisa se não conectar.
        let tmp = TempDir::new("samp-missing");
        let cfg = load_samp_config(&tmp.0);
        assert_eq!(cfg.port, 7777);
        assert_eq!(cfg.host, "127.0.0.1");
    }

    #[test]
    fn the_wildcard_bind_becomes_loopback() {
        // `0.0.0.0` é "todas as interfaces": como destino não serve.
        let tmp = TempDir::new("bind");
        tmp.file("server.cfg", "bind 0.0.0.0\n");
        assert_eq!(load_samp_config(&tmp.0).host, "127.0.0.1");
    }

    #[test]
    fn an_invalid_port_falls_back_to_the_default() {
        let tmp = TempDir::new("badport");
        tmp.file("server.cfg", "port abc\n");
        assert_eq!(load_samp_config(&tmp.0).port, 7777);
    }

    #[test]
    fn omp_config_is_parsed() {
        let tmp = TempDir::new("omp");
        tmp.file(
            "config.json",
            r#"{"rcon":{"password":"s3","enable":true},"network":{"port":7779,"bind":"10.0.0.2"}}"#,
        );
        let cfg = load_omp_config(&tmp.0);
        assert_eq!(cfg.rcon_password, "s3");
        assert_eq!(cfg.port, 7779);
        assert_eq!(cfg.host, "10.0.0.2");
        assert!(cfg.rcon_enabled);
    }

    #[test]
    fn omp_rcon_disabled_is_detected() {
        // Sem ler isto, a extensão manda pacotes para quem não escuta e o
        // timeout passa calado.
        let tmp = TempDir::new("omp-off");
        tmp.file("config.json", r#"{"rcon":{"enable":false}}"#);
        assert!(!load_omp_config(&tmp.0).rcon_enabled);
    }

    #[test]
    fn a_malformed_omp_config_yields_the_defaults() {
        let tmp = TempDir::new("omp-broken");
        tmp.file("config.json", "{ \"rcon\": ");
        let cfg = load_omp_config(&tmp.0);
        assert_eq!(cfg.port, 7777);
        assert!(cfg.rcon_enabled);
    }

    #[test]
    fn components_directory_means_openmp() {
        // Diretório exclusivo do open.mp.
        let tmp = TempDir::new("type-components");
        tmp.dir("components");
        assert_eq!(detect_server_type(&tmp.0), ServerType::Omp);
    }

    #[test]
    fn a_homonym_config_json_does_not_fool_the_detection() {
        // Outra ferramenta pode usar esse nome; só as chaves do open.mp contam.
        let tmp = TempDir::new("type-homonym");
        tmp.file("config.json", r#"{"outraFerramenta":true}"#);
        assert_eq!(detect_server_type(&tmp.0), ServerType::Samp);
    }

    #[test]
    fn an_openmp_config_json_is_recognized() {
        let tmp = TempDir::new("type-omp");
        tmp.file("config.json", r#"{"pawn":{"main_scripts":[]}}"#);
        assert_eq!(detect_server_type(&tmp.0), ServerType::Omp);
    }

    #[test]
    fn samp_is_the_final_guess() {
        // Formato mais antigo e mais comum.
        let tmp = TempDir::new("type-empty");
        assert_eq!(detect_server_type(&tmp.0), ServerType::Samp);
    }

    #[test]
    fn load_server_config_follows_the_explicit_type() {
        // Com o tipo definido, não há detecção: o usuário já decidiu.
        let tmp = TempDir::new("explicit");
        tmp.file("server.cfg", "port 7001\n");
        tmp.file("config.json", r#"{"network":{"port":7002}}"#);
        assert_eq!(load_server_config(&tmp.0, ServerType::Samp).port, 7001);
        assert_eq!(load_server_config(&tmp.0, ServerType::Omp).port, 7002);
    }

    #[test]
    fn the_working_directory_follows_the_executable() {
        // O servidor espera rodar da própria pasta: iniciá-lo de outro lugar
        // quebra os caminhos relativos dele.
        let tmp = TempDir::new("resolve-cwd");
        let cfg = crate::config::types::ServerConfig {
            path: tmp
                .0
                .join("bin")
                .join("omp-server")
                .to_string_lossy()
                .into_owned(),
            cwd: String::new(),
            ..Default::default()
        };
        let resolved = resolve_server_config(&cfg, &tmp.0);
        assert_eq!(resolved.cwd, tmp.0.join("bin"));
    }

    #[test]
    fn a_configured_working_directory_wins() {
        let tmp = TempDir::new("resolve-explicit");
        let cfg = crate::config::types::ServerConfig {
            path: "/x/omp-server".to_string(),
            cwd: "/y".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_server_config(&cfg, &tmp.0).cwd, PathBuf::from("/y"));
    }

    #[test]
    fn the_openmp_log_name_comes_from_the_config() {
        // É configurável: assumir `log.txt` deixaria o painel mudo.
        let tmp = TempDir::new("logname");
        tmp.file("config.json", r#"{"pawn":{},"logging":{"file":"meu.log"}}"#);
        let cfg = crate::config::types::ServerConfig {
            cwd: tmp.0.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let resolved = resolve_server_config(&cfg, &tmp.0);
        assert_eq!(resolved.log_path, Some(tmp.0.join("meu.log")));
    }

    #[test]
    fn samp_logs_to_server_log_txt() {
        let tmp = TempDir::new("logsamp");
        tmp.file("server.cfg", "port 7777\n");
        let cfg = crate::config::types::ServerConfig {
            cwd: tmp.0.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let resolved = resolve_server_config(&cfg, &tmp.0);
        assert_eq!(resolved.log_path, Some(tmp.0.join("server_log.txt")));
    }
}
