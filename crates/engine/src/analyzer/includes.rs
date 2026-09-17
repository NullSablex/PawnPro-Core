use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::messages::{Locale, MsgKey, msg};
use crate::parser::lexer::decode_bytes;
use crate::parser::{IncludeDirective, ParsedFile, parse_file};

use super::{codes, diagnostic::PawnDiagnostic};
use crate::util::to_u32;

#[derive(Clone)]
pub struct IncludeEntry {
    pub text: String,
    pub parsed: ParsedFile,
}

pub struct ResolvedIncludes {
    pub paths: Vec<PathBuf>,
    pub files: HashMap<PathBuf, IncludeEntry>,
    /// Maps each resolved include path to the set of file paths that directly include it.
    pub reverse_deps: HashMap<PathBuf, HashSet<PathBuf>>,
}

// Extensions tried in order, mirroring the real compiler (sc2.c plungequalifiedfile):
// exact path first, then .inc, .p, .pawn, .pwn
static EXTENSIONS: &[&str] = &["", ".inc", ".p", ".pawn", ".pwn"];

// Quotes search relative to the current file first, then fall back to include_paths —
// matching the Pawn compiler's own resolution order.
pub fn resolve_include(
    directive: &IncludeDirective,
    file_dir: &Path,
    include_paths: &[PathBuf],
) -> Option<PathBuf> {
    let token = &directive.token;

    if directive.is_angle {
        include_paths
            .iter()
            .find_map(|base| try_resolve(&base.join(token)))
    } else {
        try_resolve(&file_dir.join(token)).or_else(|| {
            include_paths
                .iter()
                .find_map(|base| try_resolve(&base.join(token)))
        })
    }
}

// On Linux, performs a case-insensitive directory scan when the exact path fails,
// covering mismatched casing like `evf.inc` vs `EVF.inc`.
fn try_resolve(path: &Path) -> Option<PathBuf> {
    let base = path.to_string_lossy();

    for ext in EXTENSIONS {
        let candidate = if ext.is_empty() {
            path.to_path_buf()
        } else {
            PathBuf::from(format!("{base}{ext}"))
        };

        if candidate.exists() {
            return Some(candidate);
        }

        #[cfg(not(target_os = "windows"))]
        if let (Some(parent), Some(file_name)) = (candidate.parent(), candidate.file_name()) {
            let needle = file_name.to_string_lossy().to_ascii_lowercase();
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().to_ascii_lowercase() == needle {
                        return Some(entry.path());
                    }
                }
            }
        }
    }

    None
}

pub fn analyze_includes(
    directives: &[IncludeDirective],
    file_path: &Path,
    include_paths: &[PathBuf],
    locale: Locale,
) -> Vec<PawnDiagnostic> {
    let file_dir = file_path.parent().unwrap_or_else(|| Path::new("."));

    directives
        .iter()
        .filter_map(|dir| {
            let col_end = dir.col + to_u32(dir.token.len());
            if resolve_include(dir, file_dir, include_paths).is_some() {
                return None;
            }
            let diag = if dir.is_try {
                PawnDiagnostic::hint(
                    dir.line,
                    dir.col,
                    col_end,
                    codes::PP0013,
                    msg(locale, MsgKey::TryIncludeNotFound).replace("{}", &dir.token),
                )
            } else {
                let message = build_not_found_message(dir, file_dir, include_paths, locale);
                PawnDiagnostic::error(dir.line, dir.col, col_end, codes::PP0001, message)
            };
            Some(diag)
        })
        .collect()
}

fn build_not_found_message(
    dir: &IncludeDirective,
    file_dir: &Path,
    include_paths: &[PathBuf],
    locale: Locale,
) -> String {
    let mut out = msg(locale, MsgKey::IncludeNotFound).replace("{}", &dir.token);
    out.push_str(&msg(locale, MsgKey::IncludeTried).replace("{}", &dir.token));

    if dir.is_angle {
        if include_paths.is_empty() {
            out.push_str(msg(locale, MsgKey::IncludeNoPathsConfigured));
        } else {
            let paths: Vec<String> = include_paths
                .iter()
                .take(2)
                .map(|p| p.display().to_string())
                .collect();
            let suffix = if include_paths.len() > 2 { "..." } else { "" };
            let template = msg(locale, MsgKey::IncludeSearchedIn);
            out.push_str(
                &template
                    .replacen("{}", &paths.join(", "), 1)
                    .replacen("{}", suffix, 1),
            );
        }
    } else {
        out.push_str(
            &msg(locale, MsgKey::IncludeRelativeTo).replace("{}", &file_dir.display().to_string()),
        );
    }

    out
}

/// Coleta os includes lendo cada um do disco.
pub fn collect_included_files(
    file_path: &Path,
    include_paths: &[PathBuf],
    directives: &[IncludeDirective],
    max_depth: usize,
    max_files: usize,
) -> ResolvedIncludes {
    let from_disk = |path: &Path| std::fs::read(path).ok().map(|b| decode_bytes(&b));
    collect_included_files_with(
        file_path,
        include_paths,
        directives,
        max_depth,
        max_files,
        &from_disk,
    )
}

/// Coleta os includes com o texto que `read` der — o do editor, para um
/// arquivo aberto com mudanças ainda não salvas.
// BFS ensures direct includes are processed before transitive ones,
// so max_files never cuts off first-level dependencies.
pub fn collect_included_files_with(
    file_path: &Path,
    include_paths: &[PathBuf],
    directives: &[IncludeDirective],
    max_depth: usize,
    max_files: usize,
    read: &dyn Fn(&Path) -> Option<String>,
) -> ResolvedIncludes {
    let mut ordered: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut files: HashMap<PathBuf, IncludeEntry> = HashMap::new();
    let mut reverse_deps: HashMap<PathBuf, HashSet<PathBuf>> = HashMap::new();

    let root_canon = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    let file_dir = file_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let mut queue: VecDeque<(Vec<IncludeDirective>, PathBuf, PathBuf, usize)> = VecDeque::new();
    queue.push_back((directives.to_vec(), file_dir, root_canon, 1));

    while let Some((dirs, dir, parent_canon, depth)) = queue.pop_front() {
        for directive in &dirs {
            if ordered.len() >= max_files {
                break;
            }

            let Some(resolved) = resolve_include(directive, &dir, include_paths) else {
                continue;
            };
            let norm = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());

            reverse_deps
                .entry(norm.clone())
                .or_default()
                .insert(parent_canon.clone());

            if seen.contains(&norm) {
                continue;
            }
            seen.insert(norm.clone());
            ordered.push(norm.clone());

            if depth < max_depth {
                let entry = files.entry(norm.clone()).or_insert_with(|| {
                    let text = read(&resolved).unwrap_or_default();
                    let parsed = parse_file(&text);
                    IncludeEntry { text, parsed }
                });
                let nested_dirs = entry.parsed.includes.clone();
                let nested_dir = resolved
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf();
                queue.push_back((nested_dirs, nested_dir, norm, depth + 1));
            }
        }
    }

    ResolvedIncludes {
        paths: ordered,
        files,
        reverse_deps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::diagnostic::Severity;

    /// Uma pasta temporária com `include/`, que se apaga sozinha.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("pawnpro-includes-{tag}-{nanos}"));
            std::fs::create_dir_all(root.join("include")).expect("criar pasta");
            Self(root)
        }

        fn write(&self, relative: &str, body: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::write(&path, body).expect("escrever arquivo");
            path
        }

        fn include_dir(&self) -> PathBuf {
            self.0.join("include")
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn check(dir: &Dir, src: &str) -> Vec<PawnDiagnostic> {
        let main = dir.write("main.pwn", src);
        let parsed = parse_file(src);
        analyze_includes(
            &parsed.includes,
            &main,
            &[dir.include_dir()],
            Locale::default(),
        )
    }

    #[test]
    fn a_missing_include_is_an_error_on_its_name() {
        let dir = Dir::new("ausente");
        let diags = check(&dir, "#include <nada>\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, codes::PP0001);
        assert!(matches!(diags[0].severity, Severity::Error));
        assert_eq!((diags[0].col_start, diags[0].col_end), (10, 14));
    }

    #[test]
    fn a_missing_tryinclude_is_only_a_hint() {
        let dir = Dir::new("try");
        let diags = check(&dir, "#tryinclude <nada>\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, codes::PP0013);
        assert!(matches!(diags[0].severity, Severity::Hint));
    }

    #[test]
    fn angle_includes_look_in_the_include_folders() {
        let dir = Dir::new("angular");
        dir.write("include/a_samp.inc", "");
        assert!(check(&dir, "#include <a_samp>\n").is_empty());
    }

    #[test]
    fn quoted_includes_look_beside_the_file_first() {
        let dir = Dir::new("aspas");
        dir.write("local.inc", "");
        dir.write("include/global.inc", "");
        // Ao lado do arquivo, e com fallback para as pastas de include.
        assert!(check(&dir, "#include \"local\"\n#include \"global\"\n").is_empty());
    }

    #[test]
    fn angle_includes_do_not_look_beside_the_file() {
        let dir = Dir::new("angular-local");
        dir.write("local.inc", "");
        assert_eq!(check(&dir, "#include <local>\n").len(), 1);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn a_different_case_still_resolves() {
        let dir = Dir::new("caixa");
        dir.write("include/EVF.inc", "");
        assert!(check(&dir, "#include <evf>\n").is_empty());
    }

    #[test]
    fn collection_follows_nested_includes() {
        let dir = Dir::new("aninhado");
        dir.write("include/a.inc", "#include <b>\n");
        dir.write("include/b.inc", "stock B() {}\n");
        let src = "#include <a>\n";
        let main = dir.write("main.pwn", src);
        let parsed = parse_file(src);
        let resolved =
            collect_included_files(&main, &[dir.include_dir()], &parsed.includes, 8, 100);
        let names: Vec<_> = resolved
            .paths
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.inc", "b.inc"]);
    }

    #[test]
    fn collection_stops_at_the_file_limit() {
        let dir = Dir::new("limite");
        dir.write("include/a.inc", "");
        dir.write("include/b.inc", "");
        let src = "#include <a>\n#include <b>\n";
        let main = dir.write("main.pwn", src);
        let parsed = parse_file(src);
        let resolved = collect_included_files(&main, &[dir.include_dir()], &parsed.includes, 8, 1);
        assert_eq!(resolved.paths.len(), 1);
    }

    #[test]
    fn a_circular_include_does_not_hang() {
        let dir = Dir::new("circular");
        dir.write("include/a.inc", "#include <b>\n");
        dir.write("include/b.inc", "#include <a>\n");
        let src = "#include <a>\n";
        let main = dir.write("main.pwn", src);
        let parsed = parse_file(src);
        let resolved =
            collect_included_files(&main, &[dir.include_dir()], &parsed.includes, 8, 100);
        assert_eq!(resolved.paths.len(), 2);
    }
}
