//! Registro de diagnóstico em arquivo.
//!
//! Existe porque um problema relatado sem log volta como "não funcionou", e a
//! investigação recomeça do zero. Aqui ficam os fatos: o que foi pedido, o que
//! respondeu, o que falhou.
//!
//! **Desligado por padrão.** Nada é criado nem escrito enquanto o nível for
//! `off` — o custo em uso normal é a comparação de um inteiro. Quem precisa
//! diagnosticar liga o nível, reproduz o problema e lê o arquivo.
//!
//! Cada evento é escrito duas vezes: no `pawnpro.log`, que junta tudo na ordem
//! em que aconteceu, e no arquivo do componente que o produziu. O primeiro
//! serve para seguir um problema que atravessa a extensão, o core e a engine;
//! o segundo, para isolar um deles.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Acima deste tamanho o arquivo é rotacionado para `<nome>.1`.
///
/// Um log que cresce sem limite acaba enchendo o disco do usuário —
/// justamente quem ligou o diagnóstico para investigar outra coisa.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Quanto se registra.
///
/// A ordem é significativa: cada nível inclui os anteriores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Nada é escrito, e nenhum arquivo é criado.
    #[default]
    Off,
    /// Só o que falhou.
    Error,
    /// Falhas e o que as costuma anteceder.
    Warn,
    /// Também o curso normal: subiu, conectou, recarregou.
    Info,
}

impl Level {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
        }
    }

    /// O nível a partir do texto da configuração. Desconhecido é `off`: na
    /// dúvida, não escrever.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "error" => Self::Error,
            "warn" | "warning" => Self::Warn,
            "info" => Self::Info,
            _ => Self::Off,
        }
    }

    /// Posição na escala, para comparar sem `match`.
    const fn rank(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
        }
    }
}

/// Para onde escrever. `None` enquanto ninguém disse qual é o projeto.
static DESTINATION: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

/// O nível atual, lido sem trava: é consultado antes de cada evento.
static LEVEL: AtomicU8 = AtomicU8::new(0);

fn destination() -> &'static Mutex<Option<PathBuf>> {
    DESTINATION.get_or_init(|| Mutex::new(None))
}

/// Liga (ou desliga) o registro para um projeto.
///
/// Chamar com `Level::Off` não apaga o que já foi escrito — só para de
/// escrever.
pub fn configure(workspace_root: &Path, level: Level) {
    LEVEL.store(level.rank(), Ordering::Relaxed);
    if let Ok(mut dir) = destination().lock() {
        *dir = (level != Level::Off).then(|| workspace_root.join(".pawnpro").join("logs"));
    }
}

/// O nível em vigor.
#[must_use]
pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        1 => Level::Error,
        2 => Level::Warn,
        3 => Level::Info,
        _ => Level::Off,
    }
}

/// `true` se um evento neste nível seria registrado.
///
/// As macros consultam isto antes de formatar a mensagem: desligado, o custo é
/// a leitura de um inteiro.
#[must_use]
pub fn enabled(level: Level) -> bool {
    level != Level::Off && level.rank() <= LEVEL.load(Ordering::Relaxed)
}

/// Registra um evento já formatado.
///
/// Falha de escrita é ignorada de propósito: o diagnóstico não pode ser a
/// causa de um problema novo, e não há a quem reportar que o log falhou.
pub fn write(level: Level, source: &str, message: &str) {
    if !enabled(level) {
        return;
    }
    let Ok(dir) = destination().lock() else {
        return;
    };
    let Some(dir) = dir.as_ref() else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }

    let mut line = String::with_capacity(message.len() + 64);
    let _ = write!(
        line,
        "{} {:<5} {source}  {message}",
        timestamp(),
        level.label()
    );
    line.push('\n');

    // O unificado conta a história inteira; o do componente isola quem falou.
    append(&dir.join("pawnpro.log"), &line);
    append(&dir.join(format!("{}.log", component_of(source))), &line);
}

/// O componente que produziu o evento: o que vem antes da `/` na origem.
fn component_of(source: &str) -> &str {
    source.split('/').next().unwrap_or(source)
}

/// Acrescenta uma linha, rotacionando o arquivo se ele passou do teto.
fn append(path: &Path, line: &str) {
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Data e hora em UTC, no formato ISO-8601 com milissegundos.
///
/// Escrito à mão para não trazer uma dependência de datas só por causa disto.
fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let millis = now.as_millis();
    // O relógio do sistema cabe folgado em i64/u32 até o ano 292 bilhões; o
    // `try_into` evita o `as` e devolve zero se o impossível acontecer.
    let secs = i64::try_from(millis / 1000).unwrap_or_default();
    let ms = u32::try_from(millis % 1000).unwrap_or_default();

    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Converte dias desde 1970-01-01 em (ano, mês, dia).
///
/// Algoritmo de Howard Hinnant, o mesmo que as bibliotecas de data usam.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    // `doy` está em 0..366 e `mp` em 0..11 por construção: as conversões não
    // podem falhar, e o `unwrap_or` cobre o caso impossível sem `as`.
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// O arquivo unificado, para quem quiser abri-lo.
#[must_use]
pub fn log_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(".pawnpro")
        .join("logs")
        .join("pawnpro.log")
}

/// Apaga os arquivos de log do projeto.
///
/// # Errors
/// Falha do sistema ao remover a pasta.
pub fn clear(workspace_root: &Path) -> std::io::Result<()> {
    let dir = workspace_root.join(".pawnpro").join("logs");
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Garante que os logs não entrem no repositório do usuário.
///
/// O `.pawnpro/.gitignore` já existe para o `state.json`; aqui só se acrescenta
/// a linha que falta.
pub fn ignore_logs(workspace_root: &Path) {
    let path = workspace_root.join(".pawnpro").join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == "logs/") {
        return;
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("logs/\n");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = File::create(&path).and_then(|mut f| f.write_all(updated.as_bytes()));
}

/// Registra uma falha.
#[macro_export]
macro_rules! diag_error {
    ($source:expr, $($arg:tt)*) => {
        if $crate::diagnostics::enabled($crate::diagnostics::Level::Error) {
            $crate::diagnostics::write(
                $crate::diagnostics::Level::Error, $source, &format!($($arg)*));
        }
    };
}

/// Registra algo que costuma anteceder uma falha.
#[macro_export]
macro_rules! diag_warn {
    ($source:expr, $($arg:tt)*) => {
        if $crate::diagnostics::enabled($crate::diagnostics::Level::Warn) {
            $crate::diagnostics::write(
                $crate::diagnostics::Level::Warn, $source, &format!($($arg)*));
        }
    };
}

/// Registra o curso normal.
#[macro_export]
macro_rules! diag_info {
    ($source:expr, $($arg:tt)*) => {
        if $crate::diagnostics::enabled($crate::diagnostics::Level::Info) {
            $crate::diagnostics::write(
                $crate::diagnostics::Level::Info, $source, &format!($($arg)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Os testes compartilham o destino global, então rodam em série.
    static GUARD: Mutex<()> = Mutex::new(());

    struct TempProject(PathBuf);

    impl TempProject {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("pawnpro-diag-{tag}-{nanos}"));
            fs::create_dir_all(&root).expect("criar projeto");
            Self(root)
        }
        fn logs(&self) -> PathBuf {
            self.0.join(".pawnpro").join("logs")
        }
        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.logs().join(name)).unwrap_or_default()
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn off_writes_nothing_and_creates_no_file() {
        // É o padrão: quem não pediu diagnóstico não paga por ele, e não
        // encontra pasta nenhuma aparecendo no projeto.
        let _lock = GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = TempProject::new("off");
        configure(&project.0, Level::Off);
        diag_error!("core/teste", "isto não deve ser escrito");
        assert!(
            !project.logs().exists(),
            "a pasta de logs foi criada com o registro desligado"
        );
    }

    #[test]
    fn the_level_filters_what_gets_written() {
        let _lock = GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = TempProject::new("nivel");
        configure(&project.0, Level::Warn);

        diag_error!("core/teste", "uma falha");
        diag_warn!("core/teste", "um aviso");
        diag_info!("core/teste", "o curso normal");

        let unified = project.read("pawnpro.log");
        assert!(unified.contains("uma falha"));
        assert!(unified.contains("um aviso"));
        assert!(
            !unified.contains("o curso normal"),
            "`info` passou com o nível em `warn`: {unified}"
        );
        configure(&project.0, Level::Off);
    }

    #[test]
    fn each_event_goes_to_the_unified_and_to_its_component() {
        // Um arquivo para seguir o problema entre os componentes, outro para
        // isolar cada um.
        let _lock = GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = TempProject::new("dois");
        configure(&project.0, Level::Info);

        diag_info!("core/rpc", "veio do core");
        diag_info!("engine/lsp", "veio da engine");

        let unified = project.read("pawnpro.log");
        assert!(unified.contains("veio do core") && unified.contains("veio da engine"));

        assert!(project.read("core.log").contains("veio do core"));
        assert!(
            !project.read("core.log").contains("veio da engine"),
            "o arquivo do core recebeu evento da engine"
        );
        assert!(project.read("engine.log").contains("veio da engine"));
        configure(&project.0, Level::Off);
    }

    #[test]
    fn the_line_carries_time_level_and_source() {
        let _lock = GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = TempProject::new("linha");
        configure(&project.0, Level::Error);
        diag_error!("core/rpc", "porta {} ocupada", 7777);

        let line = project.read("pawnpro.log");
        assert!(line.contains("ERROR"), "{line}");
        assert!(line.contains("core/rpc"), "{line}");
        assert!(line.contains("porta 7777 ocupada"), "{line}");
        // ISO-8601 em UTC: ordena por texto e não depende do fuso de quem lê.
        assert!(
            line.starts_with("20") && line.contains('T') && line.contains("Z "),
            "{line}"
        );
        configure(&project.0, Level::Off);
    }

    #[test]
    fn the_logs_stay_out_of_the_repository() {
        let _lock = GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = TempProject::new("gitignore");
        fs::create_dir_all(project.0.join(".pawnpro")).expect("criar .pawnpro");
        fs::write(
            project.0.join(".pawnpro").join(".gitignore"),
            "state.json\n",
        )
        .expect("escrever");

        ignore_logs(&project.0);
        ignore_logs(&project.0);

        let ignored = fs::read_to_string(project.0.join(".pawnpro").join(".gitignore"))
            .expect("ler .gitignore");
        assert_eq!(ignored.matches("logs/").count(), 1, "duplicou: {ignored}");
        assert!(ignored.contains("state.json"), "apagou o que já estava lá");
    }

    #[test]
    fn an_unknown_level_falls_back_to_off() {
        // Configuração escrita à mão erra; escrever menos é o lado seguro.
        assert_eq!(Level::from_name("verbose"), Level::Off);
        assert_eq!(Level::from_name(""), Level::Off);
        assert_eq!(Level::from_name("WARN"), Level::Warn);
        assert_eq!(Level::from_name("Info"), Level::Info);
    }

    #[test]
    fn the_timestamp_matches_a_known_date() {
        // 2026-09-07 é o dia em que isto foi escrito; a conversão de dias para
        // data civil é fácil de errar por um dia.
        assert_eq!(civil_from_days(20_703), (2026, 9, 7));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }
}
