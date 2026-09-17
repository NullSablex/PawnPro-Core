//! A configuração do projeto aberto, com um dono só.
//!
//! A engine lia a configuração a cada entrega e a extensão lia por conta
//! própria, em TypeScript: dois leitores do mesmo arquivo são dois donos livres
//! para discordar. Aqui fica a única instância, o único observador dos arquivos
//! e a lista de quem precisa saber quando ela muda.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use super::manager::ConfigManager;
use super::types::PawnProConfig;

/// De quanto em quanto tempo os arquivos de configuração são conferidos.
///
/// Comparar carimbos de tempo, e não assinar eventos do sistema de arquivos:
/// são quatro `stat` neste intervalo, contra uma dependência a mais e o
/// problema de editores que salvam trocando o arquivo — o que faz o observador
/// ingênuo perder o alvo justamente na hora em que ele mudou.
pub const CONFIG_POLL: Duration = Duration::from_secs(2);

/// Quem precisa saber quando a configuração muda.
///
/// É chamado com a configuração travada: chamar o serviço de volta travaria.
pub type Listener = Box<dyn Fn(&ConfigManager) + Send + Sync>;

/// A configuração resolvida e o que a extensão precisa para lidar com os
/// arquivos dela.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub config: PawnProConfig,
    pub global_path: PathBuf,
    pub project_path: PathBuf,
    /// Chaves ignoradas por terem o tipo errado.
    pub rejected: Vec<String>,
}

impl Snapshot {
    /// O estado de um `ConfigManager`, pronto para atravessar o RPC.
    #[must_use]
    pub fn of(manager: &ConfigManager) -> Self {
        Self {
            config: manager.get_all().clone(),
            global_path: manager.global_config_path().to_path_buf(),
            project_path: manager.project_config_path().to_path_buf(),
            rejected: manager.rejected_keys().to_vec(),
        }
    }
}

/// O que o último aviso cobriu: o projeto e os carimbos dos arquivos.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    root: PathBuf,
    stamps: Vec<Option<SystemTime>>,
}

/// Os arquivos cuja edição muda a configuração, e quando mudaram.
///
/// As listas entram porque o desenvolvedor as edita no dia a dia. Um arquivo
/// ausente entra como `None` — passar a existir é mudança tanto quanto ser
/// editado.
fn seen_of(manager: &ConfigManager) -> Seen {
    let naming = &manager.get_all().analysis.naming;
    let mut files = vec![
        manager.global_config_path().to_path_buf(),
        manager.project_config_path().to_path_buf(),
    ];
    for path in [&naming.blocklist_file, &naming.loop_indices_file] {
        if !path.is_empty() {
            files.push(PathBuf::from(path));
        }
    }
    Seen {
        root: manager.project_root().to_path_buf(),
        stamps: files
            .iter()
            .map(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())
            .collect(),
    }
}

/// O diretório do usuário, de onde vem a configuração global.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// A configuração do projeto aberto e quem a acompanha.
///
/// **Um projeto por vez**: a extensão sobe um core por janela, e é a janela que
/// define o projeto. Abrir outra pasta troca a configuração de todos os
/// assinantes.
pub struct ConfigService {
    home: Option<PathBuf>,
    manager: Mutex<Option<ConfigManager>>,
    listeners: Mutex<Vec<Listener>>,
    seen: Mutex<Option<Seen>>,
    watching: AtomicBool,
}

impl ConfigService {
    /// Um serviço que lê a configuração global do diretório do usuário.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_home(home_dir())
    }

    /// Um serviço com a configuração global em `home`.
    ///
    /// Os testes apontam para uma pasta vazia: lendo o `~/.pawnpro` de quem os
    /// roda, passariam ou falhariam conforme a máquina.
    #[must_use]
    pub fn with_home(home: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            home,
            manager: Mutex::new(None),
            listeners: Mutex::new(Vec::new()),
            seen: Mutex::new(None),
            watching: AtomicBool::new(false),
        })
    }

    /// Registra quem precisa saber das mudanças.
    pub fn subscribe(&self, listener: Listener) {
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push(listener);
        }
    }

    /// Abre um projeto — ou relê o mesmo — e avisa quem assina.
    ///
    /// Reabrir relê do disco: o que estiver lá agora é o que vale.
    pub fn open(self: &Arc<Self>, root: &Path) {
        // Sem diretório do usuário não há configuração global; apontar para o
        // próprio projeto mescla o arquivo sobre si mesmo, sem efeito.
        let home = self.home.clone().unwrap_or_else(|| root.to_path_buf());
        if let Ok(mut slot) = self.manager.lock() {
            *slot = Some(ConfigManager::new(root, &home));
        }
        self.changed();
        self.watch();
    }

    /// O estado atual, se já há projeto aberto.
    #[must_use]
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.read(Snapshot::of)
    }

    /// Consulta a configuração sem alterá-la. `None` sem projeto aberto.
    #[must_use]
    pub fn read<R>(&self, f: impl FnOnce(&ConfigManager) -> R) -> Option<R> {
        let slot = self.manager.lock().ok()?;
        slot.as_ref().map(f)
    }

    /// Altera a configuração e avisa quem assina. `None` sem projeto aberto.
    ///
    /// Avisa mesmo quando `f` falha: parte da escrita pode ter chegado ao
    /// disco, e o que vale é o que foi relido.
    pub fn update<R>(&self, f: impl FnOnce(&mut ConfigManager) -> R) -> Option<R> {
        let result = {
            let mut slot = self.manager.lock().ok()?;
            f(slot.as_mut()?)
        };
        self.changed();
        Some(result)
    }

    /// Relê os arquivos e avisa, sem esperar o observador.
    pub fn reload(&self) {
        self.update(ConfigManager::reload);
    }

    /// Avisa quem assina e registra o que o aviso cobriu.
    ///
    /// Sem o registro, o observador veria na volta seguinte os carimbos de uma
    /// escrita que o próprio core fez, e avisaria de novo.
    fn changed(&self) {
        let Ok(slot) = self.manager.lock() else {
            return;
        };
        let Some(manager) = slot.as_ref() else {
            return;
        };
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(seen_of(manager));
        }
        if let Ok(listeners) = self.listeners.lock() {
            for listener in listeners.iter() {
                listener(manager);
            }
        }
    }

    /// Relê e avisa se algo mudou desde o último aviso.
    ///
    /// Compara também a pasta: entre uma troca de projeto e esta volta cabe
    /// uma edição, que de outro modo passaria por já avisada.
    fn poll(&self) {
        let Some(current) = self.read(seen_of) else {
            return;
        };
        let known = self
            .seen
            .lock()
            .is_ok_and(|seen| seen.as_ref() == Some(&current));
        if !known {
            self.reload();
        }
    }

    /// Sobe o observador, uma vez só.
    ///
    /// A thread segura só um `Weak`: quando o serviço deixa de existir, ela
    /// termina na volta seguinte, em vez de sobreviver a ele.
    fn watch(self: &Arc<Self>) {
        if self.watching.swap(true, Ordering::Relaxed) {
            return;
        }
        let weak: Weak<Self> = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("pawnpro-config".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(CONFIG_POLL);
                    let Some(service) = weak.upgrade() else {
                        break;
                    };
                    service.poll();
                }
            });
        if spawned.is_err() {
            // Sem a thread não há observação; o que foi lido continua valendo,
            // e as escritas pelo core seguem avisando.
            self.watching.store(false, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Scope;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("pawnpro-cfgsvc-{tag}-{nanos}"));
            std::fs::create_dir_all(path.join("proj").join(".pawnpro")).expect("criar");
            std::fs::create_dir_all(path.join("other").join(".pawnpro")).expect("criar");
            std::fs::create_dir_all(path.join("home")).expect("criar");
            Self(path)
        }
        fn root(&self) -> PathBuf {
            self.0.join("proj")
        }
        fn service(&self) -> Arc<ConfigService> {
            ConfigService::with_home(Some(self.0.join("home")))
        }
        fn write_config(&self, sub: &str, body: &str) {
            std::fs::write(self.0.join(sub).join(".pawnpro").join("config.json"), body)
                .expect("escrever");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Conta os avisos e guarda o `locale` de cada um.
    fn counting(service: &ConfigService) -> (Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (c, s) = (Arc::clone(&count), Arc::clone(&seen));
        service.subscribe(Box::new(move |manager| {
            c.fetch_add(1, Ordering::SeqCst);
            s.lock()
                .expect("trava")
                .push(manager.get_all().locale.clone());
        }));
        (count, seen)
    }

    #[test]
    fn nothing_is_known_before_a_project_opens() {
        let tmp = TempDir::new("closed");
        let service = tmp.service();
        assert!(service.snapshot().is_none());
        assert!(service.update(ConfigManager::reload).is_none());
    }

    #[test]
    fn opening_notifies_with_what_is_on_disk() {
        let tmp = TempDir::new("open");
        tmp.write_config("proj", r#"{"locale":"ru"}"#);
        let service = tmp.service();
        let (count, seen) = counting(&service);
        service.open(&tmp.root());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(*seen.lock().expect("trava"), ["ru"]);
    }

    #[test]
    fn a_write_notifies_once_and_the_next_poll_stays_quiet() {
        // O aviso da escrita já cobre a mudança: o observador, vendo os
        // carimbos novos na volta seguinte, não pode avisar de novo.
        let tmp = TempDir::new("write");
        let service = tmp.service();
        let (count, seen) = counting(&service);
        service.open(&tmp.root());
        service
            .update(|m| m.set_key("locale", json!("es"), Scope::Project))
            .expect("aberto")
            .expect("gravar");
        assert_eq!(count.load(Ordering::SeqCst), 2);
        service.poll();
        assert_eq!(count.load(Ordering::SeqCst), 2, "avisou duas vezes");
        assert_eq!(
            seen.lock().expect("trava").last().map(String::as_str),
            Some("es")
        );
    }

    #[test]
    fn an_edit_on_disk_is_noticed_by_the_poll() {
        let tmp = TempDir::new("external");
        let service = tmp.service();
        let (count, _) = counting(&service);
        service.open(&tmp.root());
        // Outro programa grava. Na maioria dos sistemas de arquivos o carimbo
        // tem nanossegundos; a pausa cobre os que não têm.
        std::thread::sleep(Duration::from_millis(20));
        tmp.write_config("proj", r#"{"locale":"ro"}"#);
        service.poll();
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(
            service.snapshot().map(|s| s.config.locale),
            Some("ro".to_string())
        );
    }

    #[test]
    fn opening_another_project_switches_everyone_to_it() {
        let tmp = TempDir::new("switch");
        tmp.write_config("proj", r#"{"locale":"ru"}"#);
        tmp.write_config("other", r#"{"locale":"en"}"#);
        let service = tmp.service();
        let (_, seen) = counting(&service);
        service.open(&tmp.root());
        service.open(&tmp.0.join("other"));
        assert_eq!(*seen.lock().expect("trava"), ["ru", "en"]);
        assert!(
            service
                .snapshot()
                .is_some_and(|s| s.project_path.starts_with(tmp.0.join("other")))
        );
    }

    #[test]
    fn the_snapshot_carries_what_was_refused() {
        let tmp = TempDir::new("snapshot");
        tmp.write_config(
            "proj",
            r#"{"locale":5,"analysis":{"naming":{"blocklist":["x"]}}}"#,
        );
        let service = tmp.service();
        service.open(&tmp.root());
        let snapshot = service.snapshot().expect("aberto");
        assert_eq!(snapshot.rejected, ["locale"]);

        // A extensão lê camelCase; o `serde` precisa entregar assim.
        let json = serde_json::to_value(&snapshot).expect("serializar");
        assert!(json.get("globalPath").is_some());
        assert!(json["config"].get("includePaths").is_some());
    }
}
