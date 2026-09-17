//! Estado local do projeto: favoritos e histórico do painel do servidor.
//!
//! Vive em `.pawnpro/state.json`. Não é configuração: são dados de operação,
//! que não pertencem ao repositório nem a outro usuário da máquina.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Pasta do projeto onde a extensão guarda configuração e estado.
///
/// Reexportada de `config`: o nome estava repetido em oito lugares no
/// TypeScript, e renomeá-lo exigiria achar todos.
pub use crate::config::PAWNPRO_DIR;

/// Estado do painel do servidor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerState {
    pub favorites: Vec<String>,
    pub history: Vec<String>,
}

/// Todo o estado local do projeto.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PawnProState {
    pub server: ServerState,
}

/// Lê e grava `.pawnpro/state.json`.
#[derive(Debug)]
pub struct StateManager {
    file_path: PathBuf,
    data: PawnProState,
}

impl StateManager {
    /// Abre o estado de um projeto, criando o que faltar.
    ///
    /// Arquivo ausente ou corrompido cai no padrão: o histórico não vale
    /// interromper o carregamento do projeto.
    #[must_use]
    pub fn new(project_root: &Path) -> Self {
        let dir = project_root.join(PAWNPRO_DIR);
        let file_path = dir.join("state.json");
        ensure_ignored(&dir);
        let data = read_state(&file_path).unwrap_or_default();
        Self { file_path, data }
    }

    #[must_use]
    pub const fn get_all(&self) -> &PawnProState {
        &self.data
    }

    /// Substitui o estado do servidor e grava.
    ///
    /// # Errors
    /// Falha de escrita — sem permissão, disco cheio, caminho inválido.
    pub fn update_server(&mut self, value: ServerState) -> io::Result<()> {
        self.data.server = value;
        self.save()
    }

    /// Grava o estado atual.
    ///
    /// # Errors
    /// Falha de escrita.
    pub fn save(&self) -> io::Result<()> {
        write_state(&self.file_path, &self.data)
    }
}

/// Lê o estado campo a campo.
///
/// Desserializar de uma vez faria um `favorites` de tipo errado apagar também
/// o histórico: cada lista é aproveitada por conta própria, e um item que não
/// é texto fica de fora.
fn read_state(path: &Path) -> Option<PawnProState> {
    let raw = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let server = value.get("server");
    let list = |key: &str| -> Vec<String> {
        server
            .and_then(|s| s.get(key))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(PawnProState {
        server: ServerState {
            favorites: list("favorites"),
            history: list("history"),
        },
    })
}

/// Garante que `.pawnpro/` tenha um `.gitignore` cobrindo o estado local.
///
/// `state.json` guarda o histórico de comandos do servidor — dados da operação
/// de quem desenvolve, que não pertencem ao repositório. Um `.gitignore` dentro
/// da própria pasta protege sem exigir que cada projeto lembre de listá-la, e
/// sem tocar no `.gitignore` da raiz, que é do usuário.
fn ensure_ignored(dir: &Path) {
    let file = dir.join(".gitignore");
    if file.exists() {
        return;
    }
    // Sem permissão de escrita, o estado ainda funciona; só não se autoprotege.
    let _ = fs::create_dir_all(dir);
    let _ = fs::write(
        &file,
        "# Estado local do PawnPro — não pertence ao repositório.\nstate.json\n",
    );
}

/// Grava de forma atômica e com permissão restrita.
fn write_state(path: &Path, data: &PawnProState) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut json = serde_json::to_string_pretty(data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    json.push('\n');

    // Um `write` interrompido deixaria JSON truncado; o `rename` é atômico.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    restrict_permissions(&tmp);
    fs::rename(&tmp, path)?;
    // O `rename` preserva o modo do temporário, mas um arquivo que já existia
    // de uma versão anterior mantém a permissão antiga.
    restrict_permissions(path);
    Ok(())
}

/// Restringe o arquivo ao dono (0600).
///
/// Mesmo filtrando o que parece credencial, o histórico revela a operação do
/// servidor — não há motivo para outros usuários da máquina lerem.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

/// No Windows vale a ACL do diretório, não o modo POSIX.
#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// Atalhos só dos testes: a produção lê por `get_all`.
#[cfg(test)]
impl StateManager {
    fn state_file_path(&self) -> &Path {
        &self.file_path
    }

    const fn server(&self) -> &ServerState {
        &self.data.server
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Diretório temporário que se apaga sozinho, sem dependência externa.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            p.push(format!("pawnpro-state-{tag}-{nanos}"));
            fs::create_dir_all(&p).expect("criar temp");
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_file_yields_default_state() {
        let tmp = TempDir::new("missing");
        let st = StateManager::new(&tmp.0);
        assert_eq!(st.get_all(), &PawnProState::default());
        assert!(st.server().favorites.is_empty());
    }

    #[test]
    fn corrupt_file_falls_back_to_default() {
        // Um JSON truncado não pode impedir o projeto de carregar: o histórico
        // do painel não vale interromper a abertura.
        let tmp = TempDir::new("corrupt");
        let dir = tmp.0.join(PAWNPRO_DIR);
        fs::create_dir_all(&dir).expect("criar dir");
        fs::write(dir.join("state.json"), "{ \"server\": ").expect("escrever");
        let st = StateManager::new(&tmp.0);
        assert_eq!(st.get_all(), &PawnProState::default());
    }

    #[test]
    fn a_bad_list_does_not_erase_the_other() {
        // Um `favorites` corrompido não pode levar junto o histórico.
        let tmp = TempDir::new("bad-list");
        let dir = tmp.0.join(PAWNPRO_DIR);
        fs::create_dir_all(&dir).expect("criar dir");
        fs::write(
            dir.join("state.json"),
            r#"{"server":{"favorites":5,"history":["players",7,"gmx"]}}"#,
        )
        .expect("escrever");
        let st = StateManager::new(&tmp.0);
        assert!(st.server().favorites.is_empty());
        assert_eq!(st.server().history, ["players", "gmx"]);
    }

    #[test]
    fn round_trip_survives_reload() {
        let tmp = TempDir::new("roundtrip");
        let mut st = StateManager::new(&tmp.0);
        st.update_server(ServerState {
            favorites: vec!["gmx".into()],
            history: vec!["players".into(), "gmx".into()],
        })
        .expect("gravar");

        let reread = StateManager::new(&tmp.0);
        assert_eq!(reread.server().favorites, ["gmx"]);
        assert_eq!(reread.server().history, ["players", "gmx"]);
    }

    #[test]
    fn creates_self_protecting_gitignore() {
        let tmp = TempDir::new("gitignore");
        let _ = StateManager::new(&tmp.0);
        let ignore = tmp.0.join(PAWNPRO_DIR).join(".gitignore");
        let body = fs::read_to_string(&ignore).expect("gitignore criado");
        assert!(body.contains("state.json"));
    }

    #[test]
    fn keeps_an_existing_gitignore() {
        // O arquivo pode ter sido editado pelo usuário: sobrescrever apagaria
        // regras que não são nossas.
        let tmp = TempDir::new("keep-gitignore");
        let dir = tmp.0.join(PAWNPRO_DIR);
        fs::create_dir_all(&dir).expect("criar dir");
        fs::write(dir.join(".gitignore"), "regra-do-usuario\n").expect("escrever");
        let _ = StateManager::new(&tmp.0);
        let body = fs::read_to_string(dir.join(".gitignore")).expect("ler");
        assert_eq!(body, "regra-do-usuario\n");
    }

    #[test]
    fn leaves_no_temporary_file_behind() {
        // A escrita é atômica via temporário + rename; se o temporário sobrar,
        // o diretório do usuário acumula lixo.
        let tmp = TempDir::new("atomic");
        let mut st = StateManager::new(&tmp.0);
        st.update_server(ServerState::default()).expect("gravar");
        let dir = tmp.0.join(PAWNPRO_DIR);
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .expect("listar")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "sobrou temporário");
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_readable_only_by_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("perms");
        let mut st = StateManager::new(&tmp.0);
        st.update_server(ServerState::default()).expect("gravar");
        let mode = fs::metadata(st.state_file_path())
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "modo {:o}", mode & 0o777);
    }

    #[cfg(unix)]
    #[test]
    fn rewriting_tightens_permissions_of_an_old_file() {
        // Um `state.json` de versão anterior pode estar 0644: gravar por cima
        // precisa corrigir, não herdar.
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("old-perms");
        let dir = tmp.0.join(PAWNPRO_DIR);
        fs::create_dir_all(&dir).expect("criar dir");
        let file = dir.join("state.json");
        fs::write(&file, "{}\n").expect("escrever");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");

        let mut st = StateManager::new(&tmp.0);
        st.update_server(ServerState::default()).expect("gravar");
        let mode = fs::metadata(&file).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "modo {:o}", mode & 0o777);
    }
}
