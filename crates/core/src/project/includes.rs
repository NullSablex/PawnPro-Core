//! Descoberta de includes: onde procurar `.inc` e o que eles declaram.
//!
//! O compilador Pawn resolve `#include` por uma lista de raízes, e cada projeto
//! organiza a sua de um jeito — `qawno/include` no open.mp, `pawno/include` no
//! SA-MP, ou um `include/` solto. A extensão precisa da mesma lista para
//! oferecer navegação e completar nativas.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::{PAWNPRO_DIR, PawnProConfig};

/// Subpastas de include, na ordem em que o compilador as procura.
const INCLUDE_SUBDIRS: [&str; 3] = ["qawno/include", "pawno/include", "include"];

/// Pastas que a varredura de includes nunca desce.
const IGNORED_DIRS: [&str; 3] = ["node_modules", ".git", ".vscode"];

/// Uma função nativa declarada num `.inc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeEntry {
    pub name: String,
    /// Os parâmetros como estão escritos, sem os parênteses.
    pub signature: String,
    pub file_path: PathBuf,
    /// Base zero, como o protocolo LSP espera.
    pub line: usize,
}

fn is_dir(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_dir())
}

/// Extrai os caminhos passados como `-i` nos argumentos do compilador.
///
/// O usuário pode configurar includes tanto na extensão quanto direto nos
/// argumentos; ignorar os segundos daria uma lista diferente da que o
/// compilador de fato usa.
fn include_paths_from_args(args: &[String]) -> Vec<PathBuf> {
    args.iter()
        .filter_map(|a| a.strip_prefix("-i"))
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Sobe de `start_dir` até `stop_at` procurando uma pasta de includes.
///
/// Serve ao arquivo aberto fora da raiz do projeto — um gamemode em
/// `gamemodes/`, por exemplo. Para no primeiro nível que tiver alguma: subir
/// além disso pegaria includes de outro projeto.
fn find_include_roots_from_dir(start_dir: &Path, stop_at: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dir = start_dir.to_path_buf();
    loop {
        for sub in INCLUDE_SUBDIRS {
            let candidate = dir.join(sub);
            if is_dir(&candidate) {
                found.push(candidate);
            }
        }
        if !found.is_empty() || dir == stop_at {
            break;
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => break,
        }
    }
    found
}

/// Monta a lista de raízes de include para um arquivo.
///
/// A ordem importa: o que o usuário configurou vem antes do que foi descoberto,
/// porque uma configuração explícita deve vencer um palpite. Duplicatas são
/// removidas mantendo a primeira ocorrência, e caminhos inexistentes saem — uma
/// raiz que não existe só faria o compilador reclamar.
#[must_use]
pub fn build_include_paths(
    configured: &[PathBuf],
    compiler_args: &[String],
    workspace_root: &Path,
    file_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let search_root = if workspace_root.as_os_str().is_empty() {
        file_dir.map(Path::to_path_buf)
    } else {
        Some(workspace_root.to_path_buf())
    };

    let defaults: Vec<PathBuf> = search_root
        .as_ref()
        .map(|root| {
            INCLUDE_SUBDIRS
                .iter()
                .map(|sub| root.join(sub))
                .filter(|p| is_dir(p))
                .collect()
        })
        .unwrap_or_default();

    // Só quando nada foi encontrado na raiz: o arquivo pode estar num
    // subprojeto com include próprio.
    let fallback = match (
        defaults.is_empty(),
        file_dir,
        workspace_root.as_os_str().is_empty(),
    ) {
        (true, Some(dir), false) => find_include_roots_from_dir(dir, workspace_root),
        _ => Vec::new(),
    };

    let mut seen = HashSet::new();
    configured
        .iter()
        .cloned()
        .chain(include_paths_from_args(compiler_args))
        .chain(defaults)
        .chain(fallback)
        .filter(|p| !p.as_os_str().is_empty())
        .filter(|p| is_dir(p))
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

/// As raízes de include do projeto, a partir da configuração dele.
///
/// É a única montagem: a engine, a compilação e a árvore de includes da
/// extensão passam por aqui. Cada um repetindo a chamada à mão abria espaço
/// para a compilação procurar num lugar e a análise em outro.
#[must_use]
pub fn include_paths_for(
    cfg: &PawnProConfig,
    workspace_root: &Path,
    file_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let configured: Vec<PathBuf> = cfg.include_paths.iter().map(PathBuf::from).collect();
    build_include_paths(&configured, &cfg.compiler.args, workspace_root, file_dir)
}

/// Lista recursivamente os `.inc` sob `root`.
///
/// O teto de profundidade e o conjunto de caminhos reais visitados existem pelo
/// mesmo motivo: um symlink apontando para um ancestral faria a varredura
/// girar para sempre.
#[must_use]
pub fn list_inc_files_recursive(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    walk(root, 0, max_depth, &mut visited, &mut out);
    out
}

fn walk(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    // Pelo caminho REAL: dois symlinks diferentes para a mesma pasta são a
    // mesma pasta, e visitá-la duas vezes duplicaria os resultados.
    let Ok(real) = fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(real) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if IGNORED_DIRS.contains(&name.as_ref()) || name == PAWNPRO_DIR {
                continue;
            }
            walk(&path, depth + 1, max_depth, visited, out);
        } else if kind.is_file() && name.to_lowercase().ends_with(".inc") {
            out.push(path);
        }
    }
}

/// A expressão que reconhece uma declaração de nativa.
///
/// Aceita o `forward` opcional e a tag de retorno (`Float:`), que aparecem nos
/// includes do SA-MP e do open.mp.
fn native_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*(?:forward[ \t]+)?native[ \t]+(?:[A-Za-z_]\w*:)?[ \t]*([A-Za-z_]\w*)[ \t]*\(([^)]*)\)[ \t]*;")
            .expect("regex de nativa é constante e válida")
    })
}

/// Lista as nativas declaradas num arquivo, ordenadas por nome.
///
/// Arquivo ilegível devolve lista vazia: um `.inc` sem permissão não vale
/// interromper a indexação do projeto inteiro.
#[must_use]
pub fn list_natives(file_path: &Path) -> Vec<NativeEntry> {
    let Ok(text) = fs::read_to_string(file_path) else {
        return Vec::new();
    };

    let mut out: Vec<NativeEntry> = native_regex()
        .captures_iter(&text)
        .filter_map(|c| {
            let whole = c.get(0)?;
            Some(NativeEntry {
                name: c.get(1)?.as_str().to_string(),
                signature: c
                    .get(2)
                    .map_or(String::new(), |m| m.as_str().trim().to_string()),
                file_path: file_path.to_path_buf(),
                line: text[..whole.start()].matches('\n').count(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
            p.push(format!("pawnpro-inc-{tag}-{nanos}"));
            fs::create_dir_all(&p).expect("criar temp");
            Self(p)
        }
        fn dir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(&p).expect("criar dir");
            p
        }
        fn file(&self, rel: &str, body: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).expect("criar dir");
            }
            fs::write(&p, body).expect("escrever");
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn args_yield_only_the_dash_i_paths() {
        let args: Vec<String> = ["-i/a", "-O1", "-i/b", "-i", "-d3"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            include_paths_from_args(&args),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn known_include_layouts_are_found() {
        let tmp = TempDir::new("layouts");
        tmp.dir("qawno/include");
        tmp.dir("pawno/include");
        tmp.dir("include");
        let found = build_include_paths(&[], &[], &tmp.0, None);
        // A ordem é a de busca do compilador, não a do sistema de arquivos.
        assert_eq!(
            found,
            vec![
                tmp.0.join("qawno/include"),
                tmp.0.join("pawno/include"),
                tmp.0.join("include"),
            ]
        );
    }

    #[test]
    fn configured_paths_come_before_discovered_ones() {
        // Configuração explícita vence palpite: o compilador procura na ordem
        // da lista, e inverter mudaria qual `.inc` é resolvido.
        let tmp = TempDir::new("order");
        let custom = tmp.dir("custom");
        tmp.dir("include");
        let found = build_include_paths(std::slice::from_ref(&custom), &[], &tmp.0, None);
        assert_eq!(found.first(), Some(&custom));
    }

    #[test]
    fn nonexistent_paths_are_dropped() {
        let tmp = TempDir::new("missing");
        let found = build_include_paths(&[tmp.0.join("nao-existe")], &[], &tmp.0, None);
        assert!(found.is_empty());
    }

    #[test]
    fn duplicates_keep_only_the_first_occurrence() {
        let tmp = TempDir::new("dup");
        let inc = tmp.dir("include");
        let args = vec![format!("-i{}", inc.display())];
        let found = build_include_paths(std::slice::from_ref(&inc), &args, &tmp.0, None);
        assert_eq!(found.iter().filter(|p| **p == inc).count(), 1);
    }

    #[test]
    fn walks_up_when_the_root_has_no_includes() {
        // Arquivo em `gamemodes/`, includes na raiz do projeto.
        let tmp = TempDir::new("walkup");
        let root = tmp.dir("proj");
        fs::create_dir_all(root.join("include")).expect("criar");
        let deep = root.join("gamemodes");
        fs::create_dir_all(&deep).expect("criar");
        let found = build_include_paths(&[], &[], &root, Some(&deep));
        assert_eq!(found, vec![root.join("include")]);
    }

    #[test]
    fn finds_inc_files_recursively() {
        let tmp = TempDir::new("list");
        tmp.file("a.inc", "");
        tmp.file("sub/b.inc", "");
        tmp.file("sub/deep/c.INC", "");
        tmp.file("sub/nao.txt", "");
        let mut found = list_inc_files_recursive(&tmp.0, 20);
        found.sort();
        assert_eq!(found.len(), 3, "achou {found:?}");
        assert!(
            found
                .iter()
                .all(|p| p.to_string_lossy().to_lowercase().ends_with(".inc"))
        );
    }

    #[test]
    fn skips_ignored_directories() {
        let tmp = TempDir::new("ignored");
        tmp.file("ok.inc", "");
        tmp.file("node_modules/x.inc", "");
        tmp.file(".git/y.inc", "");
        tmp.file(".pawnpro/z.inc", "");
        let found = list_inc_files_recursive(&tmp.0, 20);
        assert_eq!(found, vec![tmp.0.join("ok.inc")]);
    }

    #[test]
    fn depth_limit_is_honored() {
        let tmp = TempDir::new("depth");
        tmp.file("a/b/c/deep.inc", "");
        assert!(list_inc_files_recursive(&tmp.0, 1).is_empty());
        assert_eq!(list_inc_files_recursive(&tmp.0, 5).len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cycle_does_not_hang() {
        // Um link para um ancestral faria a varredura girar para sempre sem o
        // conjunto de caminhos reais já visitados.
        let tmp = TempDir::new("cycle");
        tmp.file("real.inc", "");
        let sub = tmp.dir("sub");
        std::os::unix::fs::symlink(&tmp.0, sub.join("loop")).expect("symlink");
        let found = list_inc_files_recursive(&tmp.0, 20);
        assert_eq!(found, vec![tmp.0.join("real.inc")]);
    }

    #[test]
    fn parses_native_declarations() {
        let tmp = TempDir::new("natives");
        let f = tmp.file(
            "a.inc",
            "#include <x>\n\
             native SendClientMessage(playerid, color, const message[]);\n\
             // comentário\n\
             forward native Float:GetX(playerid);\n\
             native NoArgs();\n",
        );
        let natives = list_natives(&f);
        assert_eq!(natives.len(), 3);
        // Ordenado por nome, não por posição no arquivo.
        assert_eq!(natives[0].name, "GetX");
        assert_eq!(natives[1].name, "NoArgs");
        assert_eq!(natives[2].name, "SendClientMessage");
        assert_eq!(natives[2].signature, "playerid, color, const message[]");
        assert_eq!(natives[1].signature, "");
    }

    #[test]
    fn native_line_is_zero_based() {
        let tmp = TempDir::new("lines");
        let f = tmp.file("a.inc", "linha0\nlinha1\nnative Foo();\n");
        let natives = list_natives(&f);
        assert_eq!(natives[0].line, 2);
    }

    #[test]
    fn unreadable_file_yields_no_natives() {
        assert!(list_natives(Path::new("/nao/existe/x.inc")).is_empty());
    }
}
