//! Introspecção das flags que o compilador aceita.
//!
//! `pawncc` tem muitas variantes — versões antigas, forks, builds de open.mp — e
//! passar uma flag que a build local não conhece faz a compilação falhar por um
//! motivo que não tem nada a ver com o código do usuário. Em vez de manter uma
//! tabela por versão, perguntamos ao próprio binário: `pawncc -?` imprime a
//! ajuda, e dela sai o conjunto suportado.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Flags que uma build do compilador aceita.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Supported {
    /// Flags de um caractere: `d`, `O`, `w`, `(`, `;`…
    pub single: HashSet<String>,
    /// Flags de mais de um caractere, hoje só `XD`.
    pub multi: HashSet<String>,
    /// A ajuda como veio, para diagnóstico.
    pub raw_help: String,
}

/// Cache por caminho de executável.
///
/// Rodar `pawncc -?` custa um processo, e a resposta não muda enquanto o
/// binário for o mesmo. O `Mutex` basta: a contenção é nula na prática, já que a
/// detecção acontece uma vez por executável.
fn cache() -> &'static Mutex<HashMap<String, Supported>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Supported>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Lê da ajuda do compilador quais flags ele aceita.
///
/// Falha ao executar devolve um conjunto vazio, e não um erro: o chamador então
/// não injeta flag nenhuma, que é o comportamento seguro — compilar sem os
/// extras é melhor que não compilar.
#[must_use]
pub fn detect_supported_flags(exe: &str) -> Supported {
    if let Ok(guard) = cache().lock()
        && let Some(hit) = guard.get(exe)
    {
        return hit.clone();
    }

    let out = Command::new(exe).arg("-?").output().ok();
    let help = out.map_or_else(String::new, |o| {
        let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&o.stderr));
        s
    });

    let supported = parse_help(&help);
    if let Ok(mut guard) = cache().lock() {
        guard.insert(exe.to_string(), supported.clone());
    }
    supported
}

/// Extrai as flags de um texto de ajuda.
///
/// Separado de [`detect_supported_flags`] para ser testável sem executar
/// processo nenhum.
#[must_use]
pub fn parse_help(help: &str) -> Supported {
    let mut single = HashSet::new();
    let mut multi = HashSet::new();

    for line in help.lines() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        // A ajuda lista uma flag por linha, iniciada por `-` ou `/`.
        let Some(rest) = trimmed.strip_prefix(['-', '/']) else {
            continue;
        };
        // `XD` antes das alternativas de um caractere: começando pelo `X`
        // sozinho, `XD` nunca seria reconhecida.
        if rest.starts_with("XD") {
            multi.insert("XD".to_string());
            continue;
        }
        let Some(c) = rest.chars().next() else {
            continue;
        };
        if c.is_ascii_alphabetic() || matches!(c, '^' | '\\' | ';' | '(') {
            single.insert(c.to_string());
        }
    }

    Supported {
        single,
        multi,
        raw_help: help.to_string(),
    }
}

/// Conjunto conservador de argumentos que funciona na maioria das versões.
///
/// `-i` (includes) e `-o` (saída) entram no chamador, nunca aqui: dependem do
/// projeto, não da build do compilador.
#[must_use]
pub fn compute_minimal_args(supported: &Supported) -> Vec<String> {
    let mut args = Vec::new();
    for (flag, arg) in [
        ("d", "-d1"),
        ("O", "-O1"),
        ("(", "-(+"),
        (";", "-;+"),
        ("w", "-w239"),
    ] {
        if supported.single.contains(flag) {
            args.push(arg.to_string());
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trecho no formato que o `pawncc` imprime.
    const HELP: &str = "\
Usage:  pawncc <filename> [more filenames] [options]

Options:
        -A<num>  alignment in bytes of the data segment and the stack
        -a       output assembler code
        -C[+/-]  compact encoding for output file (default=-)
        -d<num>  debugging level (default=-d1)
        -e<name> set name of error file (quiet compile)
        -i<name> path for include files
        -l       create list file (preprocess only)
        -o<name> set base name of (P-code) output file
        -O<num>  enabling optimizations level (default=-O1)
        -w<num>  disable a specific warning by its number
        -X<num>  abstract machine size limit in bytes
        -XD<num> abstract machine data/stack size limit in bytes
        -\\       use '\\' for escape characters
        -^       use '^' for escape characters
        -;[+/-]  require a semicolon to end each statement (default=-)
        -([+/-]  require parantheses for function invocation (default=-)
";

    #[test]
    fn recognizes_single_char_flags() {
        let s = parse_help(HELP);
        for f in ["A", "a", "C", "d", "e", "i", "l", "o", "O", "w", "X"] {
            assert!(s.single.contains(f), "faltou -{f}");
        }
    }

    #[test]
    fn recognizes_xd_as_multi() {
        let s = parse_help(HELP);
        assert!(s.multi.contains("XD"));
        // `X` sozinho também existe e não pode ser engolido por `XD`.
        assert!(s.single.contains("X"));
    }

    #[test]
    fn recognizes_symbolic_flags() {
        let s = parse_help(HELP);
        for f in ["\\", "^", ";", "("] {
            assert!(s.single.contains(f), "faltou -{f}");
        }
    }

    #[test]
    fn ignores_non_flag_lines() {
        let s = parse_help("Usage: pawncc <filename>\n\nOptions:\n");
        assert!(s.single.is_empty());
        assert!(s.multi.is_empty());
    }

    #[test]
    fn minimal_args_follow_the_help() {
        let s = parse_help(HELP);
        assert_eq!(
            compute_minimal_args(&s),
            vec!["-d1", "-O1", "-(+", "-;+", "-w239"]
        );
    }

    #[test]
    fn minimal_args_omit_what_the_build_rejects() {
        // Compilador antigo, sem otimização nem controle de warning: injetar
        // `-O1` ali faria a compilação falhar por motivo alheio ao código.
        let s = parse_help("        -d<num>  debugging level\n");
        assert_eq!(compute_minimal_args(&s), vec!["-d1"]);
    }

    #[test]
    fn accepts_the_slash_prefix() {
        // Builds no Windows imprimem a ajuda com `/` no lugar de `-`.
        let s = parse_help("        /d<num>  debugging level\n        /XD<num> limit\n");
        assert!(s.single.contains("d"));
        assert!(s.multi.contains("XD"));
    }

    #[test]
    fn missing_executable_injects_nothing() {
        // Sem conseguir perguntar, o seguro é não injetar flag nenhuma.
        let s = detect_supported_flags("/nao/existe/pawncc");
        assert!(s.single.is_empty());
        assert!(compute_minimal_args(&s).is_empty());
    }
}
