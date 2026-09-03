//! Localização do `pawncc`.
//!
//! A busca vai do mais explícito ao mais genérico: o que o usuário apontou
//! vence o que adivinhamos.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// No Windows há variantes de extensão e arquitetura; no resto, um nome só.
fn executable_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["pawncc.exe", "pawncc64.exe", "pawncc", "pawncc.bat"]
    } else {
        &["pawncc"]
    }
}

/// Por que a detecção falhou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectError {
    /// O caminho configurado não existe ou não é executável, e a detecção
    /// automática está desligada — o usuário pediu aquele e só aquele.
    ConfiguredNotFound { path: PathBuf },
    /// Nenhum candidato deu certo.
    NotFound,
}

impl fmt::Display for DetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfiguredNotFound { path } => {
                write!(f, "pawncc não encontrado em: {}", path.display())
            }
            Self::NotFound => f.write_str(
                "não foi possível localizar o executável do pawncc. \
                 Configure `compiler.path` em `.pawnpro/config.json`.",
            ),
        }
    }
}

impl std::error::Error for DetectError {}

/// Tira aspas (que vêm de um copiar-colar do terminal) e expande o `~`, que o
/// shell resolveria mas um `spawn` direto não.
#[must_use]
pub fn normalize_input_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim().trim_matches(['"', '\'']).trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('~') {
        let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"))?;
        let rest = rest.strip_prefix(['/', '\\']).unwrap_or(rest);
        return Some(Path::new(&home).join(rest));
    }
    Some(PathBuf::from(trimmed))
}

/// No Unix confere o bit de execução: sem ele o `spawn` falha com um erro que
/// não diz o que fazer.
#[must_use]
pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Procura os nomes conhecidos nas pastas do `PATH`.
fn find_in_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        for name in executable_names() {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Pastas do projeto onde o compilador costuma vir junto.
fn workspace_candidates(workspace_root: &Path) -> Vec<PathBuf> {
    ["qawno", "pawno", "include", "tools", "bin"]
        .iter()
        .flat_map(|dir| {
            executable_names()
                .iter()
                .map(move |name| workspace_root.join(dir).join(name))
        })
        .collect()
}

/// Caminhos de instalação comuns, por plataforma.
fn common_candidates() -> Vec<PathBuf> {
    let paths: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files\Pawn\pawncc.exe",
            r"C:\Program Files (x86)\Pawn\pawncc.exe",
        ]
    } else {
        &[
            "/usr/local/bin/pawncc",
            "/usr/bin/pawncc",
            "/opt/pawn/pawncc",
        ]
    };
    paths.iter().map(PathBuf::from).collect()
}

/// Localiza o `pawncc`.
///
/// A ordem é do mais explícito ao mais genérico:
/// 1. a variável `PAWNCC`, que sobrepõe tudo — serve para experimentar outra
///    build sem mexer na configuração do projeto;
/// 2. `compiler.path` da configuração;
/// 3. o `PATH`;
/// 4. pastas do próprio projeto;
/// 5. caminhos de instalação comuns.
///
/// # Errors
/// [`DetectError::ConfiguredNotFound`] quando há caminho configurado que não
/// serve e `auto_detect` está desligado — nesse caso o usuário quer aquele
/// executável, e cair num outro seria pior que falhar.
/// [`DetectError::NotFound`] quando nenhum candidato deu certo.
pub fn detect_pawncc(
    configured: Option<&str>,
    auto_detect: bool,
    workspace_root: Option<&Path>,
) -> Result<PathBuf, DetectError> {
    if let Some(from_env) = env::var("PAWNCC")
        .ok()
        .and_then(|v| normalize_input_path(&v))
        && is_executable(&from_env)
    {
        return Ok(from_env);
    }

    if let Some(normalized) = configured.and_then(normalize_input_path) {
        // Apontar a pasta em vez do arquivo é engano comum o bastante para
        // valer completar o nome em vez de recusar.
        let candidate = if normalized.is_dir() {
            normalized.join(if cfg!(windows) {
                "pawncc.exe"
            } else {
                "pawncc"
            })
        } else {
            normalized.clone()
        };
        if is_executable(&candidate) {
            return Ok(candidate);
        }
        if !auto_detect {
            return Err(DetectError::ConfiguredNotFound { path: normalized });
        }
    }

    if let Some(found) = find_in_path() {
        return Ok(found);
    }

    let from_workspace = workspace_root.map(workspace_candidates).unwrap_or_default();
    from_workspace
        .into_iter()
        .chain(common_candidates())
        .find(|c| is_executable(c))
        .ok_or(DetectError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_quotes_and_whitespace() {
        // O usuário costuma colar o caminho com aspas, do terminal.
        assert_eq!(
            normalize_input_path("  \"/usr/bin/pawncc\" ").unwrap(),
            PathBuf::from("/usr/bin/pawncc")
        );
        assert_eq!(
            normalize_input_path("'/a/b'").unwrap(),
            PathBuf::from("/a/b")
        );
    }

    #[test]
    fn empty_input_yields_nothing() {
        for raw in ["", "   ", "\"\"", "''"] {
            assert!(normalize_input_path(raw).is_none(), "{raw:?}");
        }
    }

    #[test]
    fn expands_the_home_shortcut() {
        // O shell resolveria o `~`; um `spawn` direto não.
        unsafe { env::set_var("HOME", "/home/teste") };
        assert_eq!(
            normalize_input_path("~/bin/pawncc").unwrap(),
            PathBuf::from("/home/teste/bin/pawncc")
        );
    }

    #[test]
    fn a_directory_is_not_executable() {
        assert!(!is_executable(Path::new("/tmp")));
    }

    #[test]
    fn a_missing_path_is_not_executable() {
        assert!(!is_executable(Path::new("/nao/existe/pawncc")));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_without_the_execute_bit_is_rejected() {
        // Sem isto o erro só apareceria no `spawn`, sem dizer o que fazer.
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("pawnpro-noexec-{}", std::process::id()));
        std::fs::write(&p, b"#!/bin/sh\n").expect("escrever");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(!is_executable(&p));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_executable(&p));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_bad_configured_path_fails_when_autodetect_is_off() {
        // O usuário apontou aquele executável: cair em outro seria pior que
        // falhar dizendo qual não serviu.
        let err = detect_pawncc(Some("/nao/existe/pawncc"), false, None).unwrap_err();
        assert!(matches!(err, DetectError::ConfiguredNotFound { .. }));
        assert!(err.to_string().contains("/nao/existe/pawncc"));
    }

    #[test]
    fn the_error_says_where_to_configure() {
        let msg = DetectError::NotFound.to_string();
        assert!(msg.contains("compiler.path"));
    }
}
