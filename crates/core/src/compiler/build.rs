//! Montagem da linha de comando do `pawncc` e execução da compilação.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::flags::Supported;

/// Argumentos prontos para invocar o compilador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileArgs {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Flags retiradas por a build local não aceitá-las.
    ///
    /// Descartar em silêncio faria o usuário procurar por que a configuração
    /// dele não surtiu efeito.
    pub removed_flags: Vec<String>,
}

/// Resultado de uma compilação.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileResult {
    /// `None` quando o processo morreu por sinal.
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    /// Saída já decodificada.
    pub output: String,
}

/// As simbólicas vêm primeiro porque `-(` e `-;` não são alfanuméricas, e
/// `XD` antes de `X` porque é prefixo dela.
fn capture_flag_key(arg: &str) -> Option<String> {
    let rest = arg.strip_prefix('-')?;
    for sym in ['(', ';', '\\', '^'] {
        if rest.starts_with(sym) {
            return Some(sym.to_string());
        }
    }
    if rest.len() >= 2 && rest[..2].eq_ignore_ascii_case("XD") {
        return Some("XD".to_string());
    }
    let c = rest.chars().next()?;
    c.is_alphanumeric().then(|| c.to_string())
}

/// Argumentos do usuário depois de filtrar o que a build não aceita.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedArgs {
    pub kept: Vec<String>,
    pub removed: Vec<String>,
}

/// Remove o que esta build do compilador não entende.
///
/// Uma flag desconhecida faz o `pawncc` abortar por um motivo sem relação com
/// o código. Normaliza `/d` para `-d` e `-(` para `-(+`.
///
/// `-i` e `-o` saem sempre: quem os define é a montagem, a partir do projeto.
#[must_use]
pub fn sanitize_user_args(base: &[String], supported: &Supported) -> SanitizedArgs {
    let mut kept = Vec::new();
    let mut removed = Vec::new();

    for arg in base {
        let arg = match arg.strip_prefix('/') {
            Some(rest) => format!("-{rest}"),
            None => arg.clone(),
        };
        if arg.starts_with("-i") || arg.starts_with("-o") {
            continue;
        }
        let arg = match arg.as_str() {
            "-(" => "-(+".to_string(),
            "-;" => "-;+".to_string(),
            _ => arg,
        };
        let Some(key) = capture_flag_key(&arg) else {
            // Não é flag — um caminho, por exemplo. Passa adiante.
            kept.push(arg);
            continue;
        };
        let ok = if key.len() == 1 {
            supported.single.contains(&key)
        } else {
            supported.multi.contains(&key)
        };
        if ok {
            kept.push(arg);
        } else {
            removed.push(arg);
        }
    }

    SanitizedArgs { kept, removed }
}

/// `true` se os argumentos já pedem informação de depuração (`-d1`/`-d2`/`-d3`).
#[must_use]
pub fn has_debug_flag(args: &[String]) -> bool {
    args.iter().any(|a| {
        let t = a.trim();
        t.len() >= 3 && t.starts_with("-d") && matches!(t.as_bytes()[2], b'1' | b'2' | b'3')
    })
}

/// Depurar exige `-d3` (símbolos e linhas): `-d1` e `-d2` não bastam para o
/// hook nem para a inspeção. Vale só para esta compilação.
fn force_debug_level(args: &mut Vec<String>) {
    args.retain(|a| {
        let t = a.trim();
        !(t.len() >= 3 && t.starts_with("-d") && t.as_bytes()[2].is_ascii_digit())
    });
    args.push("-d3".to_string());
}

/// Monta a linha de comando completa para compilar um arquivo.
///
#[must_use]
pub fn build_compile_args(
    exe: PathBuf,
    supported: &Supported,
    configured_args: &[String],
    include_paths: &[PathBuf],
    file_path: &Path,
    force_debug: bool,
) -> CompileArgs {
    let mut raw: Vec<String> = if configured_args.is_empty() {
        super::flags::compute_minimal_args(supported)
    } else {
        configured_args.to_vec()
    };
    if force_debug {
        force_debug_level(&mut raw);
    }
    let SanitizedArgs { kept, removed } = sanitize_user_args(&raw, supported);

    let file_dir = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let amx = file_dir.join(format!(
        "{}.amx",
        file_path.file_stem().unwrap_or_default().to_string_lossy()
    ));

    let mut args = kept;
    args.extend(include_paths.iter().map(|p| format!("-i{}", p.display())));
    args.push(format!("-o{}", amx.display()));
    args.push(file_path.to_string_lossy().into_owned());

    CompileArgs {
        exe,
        args,
        cwd: file_dir,
        removed_flags: removed,
    }
}

/// Executa o compilador e devolve a saída decodificada.
///
/// `stdout` e `stderr` no mesmo buffer: o `pawncc` mistura os dois, e
/// separá-los embaralharia a relação entre erro e contexto.
///
/// # Errors
/// Falha ao lançar o processo.
pub fn run_compile(
    exe: &Path,
    args: &[String],
    cwd: &Path,
    encoding: &str,
) -> std::io::Result<CompileResult> {
    // Sem shell: um caminho com espaço ou metacaractere não vira injeção.
    let out = Command::new(exe).args(args).current_dir(cwd).output()?;

    let mut bytes = out.stdout;
    bytes.extend_from_slice(&out.stderr);

    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        out.status.signal().map(|s| s.to_string())
    };
    #[cfg(not(unix))]
    let signal = None;

    Ok(CompileResult {
        exit_code: out.status.code(),
        signal,
        output: decode_output(&bytes, encoding),
    })
}

/// O `pawncc` escreve em windows-1252 na maioria das builds; ler como UTF-8
/// transformaria acentos em lixo no meio das mensagens de erro.
#[must_use]
pub fn decode_output(bytes: &[u8], encoding: &str) -> String {
    let label = if encoding.is_empty() {
        "windows-1252"
    } else {
        encoding
    };
    let enc =
        encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::WINDOWS_1252);
    enc.decode(bytes).0.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::flags::parse_help;

    fn supported() -> Supported {
        parse_help(
            "        -d<num>  debugging level\n\
             \x20       -O<num>  optimization\n\
             \x20       -w<num>  disable warning\n\
             \x20       -i<name> include path\n\
             \x20       -o<name> output\n\
             \x20       -XD<num> data limit\n\
             \x20       -;[+/-]  semicolon\n\
             \x20       -([+/-]  parentheses\n",
        )
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn recognizes_symbolic_and_multichar_keys() {
        assert_eq!(capture_flag_key("-(+"), Some("(".into()));
        assert_eq!(capture_flag_key("-;+"), Some(";".into()));
        // `XD` antes de `X`, senão nunca seria reconhecida.
        assert_eq!(capture_flag_key("-XD500"), Some("XD".into()));
        assert_eq!(capture_flag_key("-d3"), Some("d".into()));
        assert_eq!(capture_flag_key("arquivo.pwn"), None);
    }

    #[test]
    fn unsupported_flags_are_reported_not_dropped_silently() {
        // Descartar em silêncio faria o usuário procurar por que a config dele
        // não surtiu efeito.
        let out = sanitize_user_args(&args(&["-d3", "-Z9"]), &supported());
        assert_eq!(out.kept, ["-d3"]);
        assert_eq!(out.removed, ["-Z9"]);
    }

    #[test]
    fn windows_style_slashes_become_dashes() {
        let out = sanitize_user_args(&args(&["/d3"]), &supported());
        assert_eq!(out.kept, ["-d3"]);
    }

    #[test]
    fn short_symbolic_flags_get_their_full_form() {
        let out = sanitize_user_args(&args(&["-(", "-;"]), &supported());
        assert_eq!(out.kept, ["-(+", "-;+"]);
    }

    #[test]
    fn include_and_output_flags_are_always_stripped() {
        // Quem os define é a montagem, a partir do projeto.
        let out = sanitize_user_args(&args(&["-i/x", "-o/y", "-d3"]), &supported());
        assert_eq!(out.kept, ["-d3"]);
        assert!(out.removed.is_empty());
    }

    #[test]
    fn non_flag_arguments_pass_through() {
        let out = sanitize_user_args(&args(&["arquivo.pwn"]), &supported());
        assert_eq!(out.kept, ["arquivo.pwn"]);
    }

    #[test]
    fn detects_an_existing_debug_flag() {
        assert!(has_debug_flag(&args(&["-d1"])));
        assert!(has_debug_flag(&args(&["-O1", " -d3 "])));
        assert!(!has_debug_flag(&args(&["-d0"])));
        assert!(!has_debug_flag(&args(&["-O1"])));
    }

    #[test]
    fn force_debug_replaces_any_existing_level() {
        // `-d1` e `-d2` não bastam para o hook nem para a inspeção.
        let mut a = args(&["-O1", "-d1", "-w239"]);
        force_debug_level(&mut a);
        assert_eq!(a, ["-O1", "-w239", "-d3"]);
    }

    #[test]
    fn output_goes_next_to_the_source() {
        let built = build_compile_args(
            PathBuf::from("/bin/pawncc"),
            &supported(),
            &args(&["-d3"]),
            &[PathBuf::from("/proj/include")],
            Path::new("/proj/gamemodes/main.pwn"),
            false,
        );
        assert_eq!(built.cwd, PathBuf::from("/proj/gamemodes"));
        assert!(built.args.contains(&"-i/proj/include".to_string()));
        assert!(
            built
                .args
                .contains(&"-o/proj/gamemodes/main.amx".to_string())
        );
        // O arquivo vem por último, como o pawncc espera.
        assert_eq!(built.args.last().unwrap(), "/proj/gamemodes/main.pwn");
    }

    #[test]
    fn empty_configuration_falls_back_to_the_minimal_set() {
        let built = build_compile_args(
            PathBuf::from("/bin/pawncc"),
            &supported(),
            &[],
            &[],
            Path::new("/p/a.pwn"),
            false,
        );
        assert!(built.args.contains(&"-d1".to_string()));
        assert!(built.args.contains(&"-O1".to_string()));
    }

    #[test]
    fn force_debug_wins_over_the_user_level() {
        let built = build_compile_args(
            PathBuf::from("/bin/pawncc"),
            &supported(),
            &args(&["-d1"]),
            &[],
            Path::new("/p/a.pwn"),
            true,
        );
        assert!(built.args.contains(&"-d3".to_string()));
        assert!(!built.args.contains(&"-d1".to_string()));
    }

    #[test]
    fn output_is_decoded_as_windows_1252_by_default() {
        // O pawncc escreve em windows-1252: 0xE7 é `ç` e 0xE3 é `ã`. Lidos como
        // UTF-8 seriam bytes inválidos, e o acento viraria lixo bem no meio da
        // mensagem de erro.
        let bytes = [b'a', 0xE7, 0xE3, b'o'];
        assert_eq!(decode_output(&bytes, ""), "ação");
    }

    #[test]
    fn an_explicit_encoding_is_honored() {
        // Os mesmos bytes em UTF-8 são a sequência de `ç`.
        let bytes = [b'a', 0xC3, 0xA7, b'a', b'o'];
        assert_eq!(decode_output(&bytes, "utf-8"), "açao");
    }

    #[test]
    fn an_unknown_encoding_falls_back_instead_of_failing() {
        let bytes = *b"ok";
        assert_eq!(decode_output(&bytes, "nao-existe"), "ok");
    }
}
