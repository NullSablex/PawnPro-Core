use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use dashmap::DashMap;
use tower_lsp::lsp_types::Url;

use crate::analyzer::PawnDiagnostic;
use crate::analyzer::includes::{
    collect_included_files, collect_included_files_with, resolve_include,
};
use crate::analyzer::{
    deprecated, hints, includes, indentation, naming, pragmas, semantic, undefined, unused,
};
use crate::config::EngineConfig;
use crate::messages::Locale;
use crate::parser::lexer::decode_bytes;
use crate::parser::{ParsedFile, parse_file};

#[derive(Debug, Clone)]
pub struct Document {
    pub text: String,
    pub version: i32,
    /// Os identificadores deste texto, para o `unused` dos outros arquivos:
    /// calculados na primeira consulta e descartados a cada edição.
    idents: OnceLock<Arc<unused::FileIdents>>,
}

impl Document {
    fn new(text: String, version: i32) -> Self {
        Self {
            text,
            version,
            idents: OnceLock::new(),
        }
    }

    fn idents(&self) -> Arc<unused::FileIdents> {
        Arc::clone(
            self.idents
                .get_or_init(|| Arc::new(unused::FileIdents::of(&self.text))),
        )
    }
}

pub struct WorkspaceState {
    pub workspace_root: Option<PathBuf>,
    pub config: EngineConfig,
    pub locale: Locale,
    /// Raízes de include, entregues resolvidas pelo core. Vazio enquanto ele
    /// não entregou: a engine não tem política própria de onde procurar.
    pub include_paths: Vec<PathBuf>,
    pub open_docs: DashMap<String, Document>,
    pub parsed_cache: DashMap<PathBuf, Arc<ParsedFile>>,
    pub dep_graph: DashMap<PathBuf, HashSet<PathBuf>>,
    // tabsize is compiler-global — a single `#pragma tabsize N` in any included file
    // affects all files compiled after it, so we cache the value workspace-wide.
    // `Option<Option<u32>>` é memoização: outer = "já computado?", inner = "tem valor?".
    #[allow(clippy::option_option)]
    pub tabsize_cache: Mutex<Option<Option<u32>>>,
    pub sdk_file: Option<PathBuf>,
    pub sdk_parsed: Option<ParsedFile>,
    /// Estilo de formatação configurado (preset + overrides). O `tab_size` e o
    /// `insert_spaces` reais vêm do editor por chamada e são aplicados sobre este.
    pub format_style: crate::intellisense::FormatStyle,
    /// Os identificadores de cada arquivo do workspace, com a data de
    /// modificação em que foram lidos. Reler e varrer o projeto inteiro a cada
    /// tecla era o que fazia a análise levar segundos.
    ident_cache: DashMap<PathBuf, (Option<SystemTime>, Arc<unused::FileIdents>)>,
}

impl WorkspaceState {
    pub fn new() -> Self {
        Self {
            workspace_root: None,
            config: EngineConfig::default(),
            locale: Locale::default(),
            include_paths: Vec::new(),
            open_docs: DashMap::new(),
            parsed_cache: DashMap::new(),
            dep_graph: DashMap::new(),
            tabsize_cache: Mutex::new(None),
            sdk_file: None,
            sdk_parsed: None,
            format_style: crate::intellisense::FormatStyle::default(),
            ident_cache: DashMap::new(),
        }
    }

    pub fn set_sdk_file(&mut self, path: PathBuf) {
        self.sdk_parsed = parse_sdk(&path);
        self.sdk_file = Some(path);
    }

    pub fn set_sdk_file_opt(&mut self, path: Option<PathBuf>) {
        if let Some(p) = path {
            self.set_sdk_file(p);
        } else {
            self.sdk_file = None;
            self.sdk_parsed = None;
        }
    }

    pub fn set_workspace_root(&mut self, root: PathBuf) {
        // Só a raiz: a configuração chega pelo canal do core, que é quem lê o
        // `config.json` do projeto e do usuário.
        self.workspace_root = Some(root);
        self.invalidate_tabsize_cache();
    }

    pub fn invalidate_tabsize_cache(&self) {
        *self.tabsize_cache.lock().unwrap() = None;
    }

    pub fn open_document(&self, uri: String, text: String, version: i32) {
        self.evict_uri_from_cache(&uri);
        self.open_docs.insert(uri, Document::new(text, version));
    }

    pub fn change_document(&self, uri: &str, text: String, version: i32) {
        self.evict_uri_from_cache(uri);

        self.open_docs
            .insert(uri.to_string(), Document::new(text, version));

        // If an include file changed, also evict every file that depends on it.
        if let Some(path) = uri_to_path(uri) {
            self.evict_dependents(&path);
        }
    }

    pub fn close_document(&self, uri: &str) {
        self.open_docs.remove(uri);
        self.evict_uri_from_cache(uri);
    }

    pub fn get_text(&self, uri: &str) -> Option<String> {
        if let Some(doc) = self.open_docs.get(uri) {
            return Some(doc.text.clone());
        }
        let path = uri_to_path(uri)?;
        std::fs::read(&path).ok().map(|b| decode_bytes(&b))
    }

    pub fn get_parsed(&self, uri: &str) -> Option<Arc<ParsedFile>> {
        let path = uri_to_path(uri)?;
        if let Some(cached) = self.parsed_cache.get(&path) {
            return Some(Arc::clone(cached.value()));
        }
        let text = self.get_text(uri)?;
        let parsed = Arc::new(parse_file(&text));
        self.parsed_cache.insert(path, Arc::clone(&parsed));
        Some(parsed)
    }

    pub fn get_parsed_by_path(&self, path: &Path) -> Option<Arc<ParsedFile>> {
        if let Some(cached) = self.parsed_cache.get(path) {
            return Some(Arc::clone(cached.value()));
        }
        let bytes = std::fs::read(path).ok()?;
        let text = decode_bytes(&bytes);
        let parsed = Arc::new(parse_file(&text));
        self.parsed_cache
            .insert(path.to_path_buf(), Arc::clone(&parsed));
        Some(parsed)
    }

    pub fn open_dependents(&self, uri: &str) -> Vec<String> {
        let Some(start) = uri_to_path(uri) else {
            return vec![];
        };
        let start = start.canonicalize().unwrap_or(start);

        let mut visited: HashSet<PathBuf> = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let mut result = Vec::new();
        queue.push_back(start);

        while let Some(node) = queue.pop_front() {
            if !visited.insert(node.clone()) {
                continue;
            }
            if let Some(parents) = self.dep_graph.get(&node) {
                for parent in parents.value() {
                    if !visited.contains(parent) {
                        queue.push_back(parent.clone());
                    }
                }
            }
        }

        for open_uri in self.open_docs.iter().map(|e| e.key().clone()) {
            if let Some(p) = uri_to_path(&open_uri) {
                let p = p.canonicalize().unwrap_or(p);
                if visited.contains(&p) {
                    result.push(open_uri);
                }
            }
        }

        result
    }

    /// Os diagnósticos sem a versão, para os testes e a sonda. O servidor usa
    /// `analyze_versioned`, para não publicar resultado de texto já editado.
    #[cfg(test)]
    pub fn analyze(&self, uri: &str) -> Vec<PawnDiagnostic> {
        self.analyze_versioned(uri).1
    }

    /// A versão do documento aberto; `None` se ele não está aberto.
    pub fn current_version(&self, uri: &str) -> Option<i32> {
        self.open_docs.get(uri).map(|doc| doc.version)
    }

    /// Analisa e diz de qual versão do documento — `None` para um arquivo lido
    /// do disco. Texto e versão são lidos juntos: o resultado nunca é atribuído
    /// a um texto diferente do analisado.
    pub fn analyze_versioned(&self, uri: &str) -> (Option<i32>, Vec<PawnDiagnostic>) {
        let snapshot = self
            .open_docs
            .get(uri)
            .map(|doc| (doc.text.clone(), Some(doc.version)));
        let Some((text, version)) = snapshot.or_else(|| self.get_text(uri).map(|t| (t, None)))
        else {
            return (None, vec![]);
        };
        let Some(file_path) = uri_to_path(uri) else {
            return (version, vec![]);
        };

        if self.config.analysis.suppress_diagnostics_in_inc && is_include_file(&file_path) {
            return (version, vec![]);
        }

        let parsed = Arc::new(parse_file(&text));
        let inc_paths = self.include_paths.clone();
        let open = self.open_paths();
        let resolved = collect_included_files_with(
            &file_path,
            &inc_paths,
            &parsed.includes,
            16,
            1000,
            &|path| self.text_at(&open, path),
        );

        self.record_dependencies(&resolved.reverse_deps);

        let locale = self.locale;
        let inc_texts: Vec<&str> = resolved.files.values().map(|e| e.text.as_str()).collect();
        let global_tabsize = self.cached_tabsize(&inc_paths);

        let mut diags = Vec::new();
        diags.extend(includes::analyze_includes(
            &parsed.includes,
            &file_path,
            &inc_paths,
            self.workspace_root.as_deref(),
            locale,
        ));
        diags.extend(semantic::analyze_semantics(&text, locale));
        let others = self.other_idents(&file_path, &open);
        diags.extend(unused::analyze_unused(
            &text,
            &file_path,
            &parsed,
            &resolved,
            self.config.analysis.warn_unused_in_inc,
            &others,
            locale,
        ));
        diags.extend(deprecated::analyze_deprecated(
            &text, &file_path, &parsed, &inc_paths, &resolved, locale,
        ));
        diags.extend(pragmas::analyze_pragmas(&text, locale));
        diags.extend(hints::analyze_hints(&text, &parsed.symbols, locale));
        diags.extend(undefined::analyze_undefined(
            &text,
            &file_path,
            &parsed,
            &resolved,
            self.sdk_parsed.as_ref(),
            locale,
        ));
        diags.extend(indentation::analyze_indentation(
            &text,
            &inc_texts,
            global_tabsize,
            locale,
        ));
        diags.extend(naming::analyze_naming(
            &text,
            &parsed.symbols,
            &self.config.analysis.naming,
            locale,
        ));

        self.parsed_cache.insert(file_path, parsed);

        (version, diags)
    }

    // --- private helpers ---

    fn evict_uri_from_cache(&self, uri: &str) {
        if let Some(path) = uri_to_path(uri) {
            self.parsed_cache.remove(&path);
        }
    }

    pub fn evict_path_from_cache(&self, path: &Path) {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.parsed_cache.remove(&canon);
        // A data de modificação sozinha não separa duas gravações no mesmo
        // instante; o aviso do editor sobre o arquivo alterado, sim. A chave é o
        // caminho como a varredura o listou, que pode não ser o canônico.
        self.ident_cache.remove(path);
        self.ident_cache.remove(&canon);
        self.evict_dependents(&canon);
    }

    /// Os documentos abertos, pelo caminho com que o editor os abriu e pelo
    /// canônico — os includes resolvidos chegam canônicos. Montado uma vez por
    /// análise, para não canonicalizar a cada arquivo consultado.
    pub(crate) fn open_paths(&self) -> HashMap<PathBuf, String> {
        let mut open = HashMap::new();
        for uri in self.open_docs.iter().map(|e| e.key().clone()) {
            let Some(path) = uri_to_path(&uri) else {
                continue;
            };
            if let Ok(canon) = path.canonicalize() {
                open.insert(canon, uri.clone());
            }
            open.insert(path, uri);
        }
        open
    }

    /// O texto de um arquivo como o usuário o vê: o do editor, se aberto —
    /// com o que ainda não foi salvo —, senão o do disco.
    pub(crate) fn text_at(&self, open: &HashMap<PathBuf, String>, path: &Path) -> Option<String> {
        if let Some(uri) = open.get(path)
            && let Some(doc) = self.open_docs.get(uri)
        {
            return Some(doc.text.clone());
        }
        std::fs::read(path).ok().map(|b| decode_bytes(&b))
    }

    /// Os arquivos compilados junto com `file`: cada programa do workspace que
    /// o inclui, direta ou indiretamente, com tudo o que esse programa inclui.
    /// Um arquivo que nenhum programa inclui fica com ele mesmo e seus
    /// includes. Programas diferentes — um gamemode e uma filterscript — não
    /// se enxergam, mesmo com funções de mesmo nome.
    ///
    /// Programa é o `.pwn` que nenhum arquivo do projeto inclui: há projetos
    /// que dão `.pwn` a trechos incluídos, e um trecho não se compila sozinho.
    ///
    /// Os `#include` de cada arquivo vêm do cache de identificadores; o grafo
    /// é refeito a cada consulta, com cada arquivo resolvido uma vez só.
    pub(crate) fn unit_files(
        &self,
        file: &Path,
        open: &HashMap<PathBuf, String>,
    ) -> HashSet<PathBuf> {
        let me = canonical(file);
        let mut children: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let closure = |start: PathBuf, children: &mut HashMap<PathBuf, Vec<PathBuf>>| {
            let mut seen = HashSet::new();
            let mut queue = vec![start];
            while let Some(node) = queue.pop() {
                if !seen.insert(node.clone()) {
                    continue;
                }
                let next = children
                    .entry(node.clone())
                    .or_insert_with(|| self.direct_includes(&node, open));
                queue.extend(next.iter().filter(|c| !seen.contains(*c)).cloned());
            }
            seen
        };

        let mut unit = HashSet::new();
        if let Some(root) = self.workspace_root.as_deref() {
            let closures: Vec<(PathBuf, HashSet<PathBuf>)> =
                unused::workspace_files(root, Path::new(""))
                    .filter(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case("pwn"))
                    })
                    .map(|p| {
                        let p = canonical(&p);
                        let files = closure(p.clone(), &mut children);
                        (p, files)
                    })
                    .collect();
            let included: HashSet<&PathBuf> = children.values().flatten().collect();
            for (program, files) in &closures {
                if !included.contains(program) && files.contains(&me) {
                    unit.extend(files.iter().cloned());
                }
            }
        }
        if unit.is_empty() {
            unit = closure(me, &mut children);
        }
        unit
    }

    /// Os arquivos que `file` inclui diretamente, resolvidos e canônicos.
    fn direct_includes(&self, file: &Path, open: &HashMap<PathBuf, String>) -> Vec<PathBuf> {
        let Some(idents) = self.idents_of(file, open) else {
            return Vec::new();
        };
        let dir = file.parent().unwrap_or(Path::new("."));
        idents
            .includes()
            .iter()
            .filter_map(|d| resolve_include(d, dir, &self.include_paths))
            .map(|p| canonical(&p))
            .collect()
    }

    /// Os textos da unidade de compilação de `file` que interessam a `keep`,
    /// com a URI de cada um — a do editor, para os abertos —, em ordem. O
    /// cache de identificadores filtra antes, e só os que interessam são lidos
    /// inteiros.
    pub(crate) fn unit_texts(
        &self,
        file: &Path,
        keep: impl Fn(&unused::FileIdents) -> bool,
    ) -> Vec<(String, String)> {
        let open = self.open_paths();
        let mut out: Vec<(String, String)> = self
            .unit_files(file, &open)
            .into_iter()
            .filter_map(|path| {
                let idents = self.idents_of(&path, &open)?;
                if !keep(&idents) {
                    return None;
                }
                let uri = uri_for(&open, &path)?;
                Some((uri, self.text_at(&open, &path)?))
            })
            .collect();
        out.sort();
        out
    }

    /// Os identificadores dos outros arquivos da unidade de compilação, para o
    /// `unused`: só quem é compilado junto pode usar o que o arquivo declara.
    /// O conteúdo de cada um vem do cache: o do editor para os abertos, o do
    /// disco enquanto a data não mudar para os demais.
    pub(crate) fn other_idents(
        &self,
        file: &Path,
        open: &HashMap<PathBuf, String>,
    ) -> Vec<Arc<unused::FileIdents>> {
        let me = canonical(file);
        self.unit_files(file, open)
            .into_iter()
            .filter(|path| *path != me)
            .filter_map(|path| self.idents_of(&path, open))
            .collect()
    }

    /// Os identificadores de um arquivo. Aberto no editor, vêm do texto dele;
    /// fechado, do disco, relidos só quando a data de modificação muda. Sem
    /// data legível, não se confia no que está guardado.
    pub(crate) fn idents_of(
        &self,
        path: &Path,
        open: &HashMap<PathBuf, String>,
    ) -> Option<Arc<unused::FileIdents>> {
        if let Some(uri) = open.get(path)
            && let Some(doc) = self.open_docs.get(uri)
        {
            return Some(doc.idents());
        }
        let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if stamp.is_some()
            && let Some(hit) = self.ident_cache.get(path)
            && hit.0 == stamp
        {
            return Some(Arc::clone(&hit.1));
        }
        let bytes = std::fs::read(path).ok()?;
        let idents = Arc::new(unused::FileIdents::of(&decode_bytes(&bytes)));
        self.ident_cache
            .insert(path.to_path_buf(), (stamp, Arc::clone(&idents)));
        Some(idents)
    }

    fn evict_dependents(&self, changed_include: &Path) {
        let canon = changed_include
            .canonicalize()
            .unwrap_or_else(|_| changed_include.to_path_buf());

        let mut visited: HashSet<PathBuf> = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(canon);

        while let Some(node) = queue.pop_front() {
            if !visited.insert(node.clone()) {
                continue;
            }
            self.parsed_cache.remove(&node);
            if let Some(dependents) = self.dep_graph.get(&node) {
                for parent in dependents.value() {
                    if !visited.contains(parent) {
                        queue.push_back(parent.clone());
                    }
                }
            }
        }
    }

    fn record_dependencies(
        &self,
        reverse_deps: &std::collections::HashMap<PathBuf, HashSet<PathBuf>>,
    ) {
        for (include_path, parents) in reverse_deps {
            let mut entry = self.dep_graph.entry(include_path.clone()).or_default();
            for parent in parents {
                entry.insert(parent.clone());
            }
        }
    }

    fn cached_tabsize(&self, inc_paths: &[PathBuf]) -> Option<u32> {
        let mut cache = self.tabsize_cache.lock().unwrap();
        *cache.get_or_insert_with(|| find_global_tabsize(inc_paths))
    }
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self::new()
    }
}

/// A URI de um arquivo: a com que o editor o abriu, se aberto; senão, a do
/// caminho.
pub(crate) fn uri_for(open: &HashMap<PathBuf, String>, path: &Path) -> Option<String> {
    match open.get(path) {
        Some(uri) => Some(uri.clone()),
        None => Url::from_file_path(path).ok().map(|u| u.to_string()),
    }
}

/// O caminho canônico, ou o próprio se não existir em disco.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?;
    #[cfg(target_os = "windows")]
    let path = path.trim_start_matches('/');
    let decoded = percent_decode(path);
    let p = PathBuf::from(decoded);
    if p.components().any(|c| c == std::path::Component::ParentDir) {
        return None;
    }
    Some(p)
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte as char);
            i += 3;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn is_include_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("inc" | "p" | "pawn")
    )
}

// tabsize is compiler-global — an include that defines tabsize=4 affects all files
// compiled after it. We scan include dirs once and cache the result.
fn find_global_tabsize(inc_paths: &[PathBuf]) -> Option<u32> {
    let mut result = None;
    for dir in inc_paths {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "inc" | "p" | "pwn") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text = decode_bytes(&bytes);
            for line in text.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("#pragma")
                    && let Some(rest) = rest.trim().strip_prefix("tabsize")
                    && let Ok(n) = rest.trim().parse::<u32>()
                {
                    result = Some(n);
                }
            }
        }
    }
    result
}

fn parse_sdk(path: &PathBuf) -> Option<ParsedFile> {
    let bytes = std::fs::read(path).ok()?;
    let text = decode_bytes(&bytes);
    let mut root = parse_file(&text);

    // open.mp.inc itself has almost no symbols — they live in _open_mp and sub-includes.
    // Resolve transitively so all SDK symbols are visible.
    let inc_paths: Vec<PathBuf> = path
        .parent()
        .map(|p| vec![p.to_path_buf()])
        .unwrap_or_default();
    let resolved = collect_included_files(path, &inc_paths, &root.includes, 16, 1000);

    for inc_path in &resolved.paths {
        if let Some(entry) = resolved.files.get(inc_path) {
            root.symbols.extend(entry.parsed.symbols.clone());
            root.macro_names.extend(entry.parsed.macro_names.clone());
            root.func_macro_prefixes
                .extend(entry.parsed.func_macro_prefixes.clone());
        }
    }

    Some(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_offers_the_sibling_include_but_not_another_program() {
        // `a.inc` e `b.inc` são incluídos pelo mesmo `main.pwn`: o que um
        // declara, o outro enxerga. O `fs.pwn` é outro programa.
        use tower_lsp::lsp_types::Position;

        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-ws-completion-{nanos}"));
        std::fs::create_dir_all(&root).expect("criar projeto");
        std::fs::write(root.join("b.inc"), "stock SiblingHelper()\n{\n}\n").expect("escrever b");
        std::fs::write(root.join("fs.pwn"), "stock OtherProgram()\n{\n}\n").expect("escrever fs");
        std::fs::write(
            root.join("main.pwn"),
            "#include \"a\"\n#include \"b\"\n\nmain()\n{\n}\n",
        )
        .expect("escrever main");
        let a = root.join("a.inc");
        let a_text = "Other()\n{\n\t\n}\n";
        std::fs::write(&a, a_text).expect("escrever a");

        let mut state = WorkspaceState::new();
        state.set_workspace_root(root.clone());
        let a_uri = Url::from_file_path(&a).expect("uri").to_string();
        state.open_document(a_uri.clone(), a_text.into(), 1);
        let labels: Vec<String> = crate::intellisense::get_completions(
            &state,
            &a_uri,
            Position {
                line: 2,
                character: 1,
            },
        )
        .into_iter()
        .map(|item| item.label)
        .collect();
        let _ = std::fs::remove_dir_all(&root);

        assert!(labels.iter().any(|l| l == "SiblingHelper"), "{labels:?}");
        assert!(!labels.iter().any(|l| l == "OtherProgram"), "{labels:?}");
    }

    #[test]
    fn a_call_in_a_closed_file_is_a_reference() {
        // A chamada fica num arquivo que não está aberto: a lista e o contador
        // têm de enxergá-la mesmo assim.
        use crate::messages::{MsgKey, msg};
        use tower_lsp::lsp_types::Position;

        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-ws-refs-{nanos}"));
        std::fs::create_dir_all(root.join("include")).expect("criar projeto");
        let lib = root.join("include").join("lib.inc");
        let lib_text = "stock Helper()\n{\n}\n";
        std::fs::write(&lib, lib_text).expect("escrever lib");
        let other = root.join("other.pwn");
        // A chamada fica numa declaração com `new`, e o nome também aparece
        // numa string: a primeira conta, a segunda não.
        std::fs::write(
            &other,
            "#include <lib>\n\nmain()\n{\n\tprint(\"Helper(\");\n\tnew x = Helper();\n}\n",
        )
        .expect("escrever other");
        // Outro programa, com uma função de mesmo nome: não é compilado junto,
        // e nada dele pode entrar na lista ou no contador.
        std::fs::write(
            root.join("fs.pwn"),
            "stock Helper()\n{\n}\n\nmain()\n{\n\tHelper();\n}\n",
        )
        .expect("escrever fs");

        let mut state = WorkspaceState::new();
        state.set_workspace_root(root.clone());
        state.include_paths = vec![root.join("include")];
        let lib_uri = Url::from_file_path(&lib).expect("uri").to_string();
        state.open_document(lib_uri.clone(), lib_text.into(), 1);

        let refs = crate::intellisense::get_references(
            &state,
            &lib_uri,
            Position {
                line: 0,
                character: 6,
            },
        );
        let lens = crate::intellisense::get_code_lens(&state, &lib_uri);
        let _ = std::fs::remove_dir_all(&root);

        let in_other = refs
            .iter()
            .filter(|l| l.uri.path().ends_with("other.pwn"))
            .count();
        assert_eq!(refs.len(), 2, "a declaração e a chamada: {refs:?}");
        assert_eq!(in_other, 1);
        assert!(!refs.iter().any(|l| l.uri.path().ends_with("fs.pwn")));
        let title = lens[0].command.as_ref().map(|c| c.title.clone());
        assert_eq!(title.as_deref(), Some(msg(state.locale, MsgKey::RefsOne)));
    }

    #[test]
    fn the_analysis_says_which_version_it_read() {
        let state = WorkspaceState::new();
        let uri = "file:///nao/existe/main.pwn";
        state.open_document(uri.to_string(), "main() {}\n".into(), 3);
        assert_eq!(state.analyze_versioned(uri).0, Some(3));
        state.change_document(uri, "main() {}\n".into(), 4);
        assert_eq!(state.analyze_versioned(uri).0, Some(4));
        assert_eq!(state.current_version(uri), Some(4));
        state.close_document(uri);
        assert_eq!(state.current_version(uri), None);
    }

    #[test]
    fn unsaved_text_of_an_open_include_reaches_the_analysis() {
        // Uma `stock` nova no include aberto, ainda não salva: a chamada no
        // `.pwn` não pode ser acusada de não declarada, e a função tem de contar
        // como usada. Fechado o include sem salvar, vale o disco de novo.
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-ws-unsaved-{nanos}"));
        std::fs::create_dir_all(root.join("include")).expect("criar projeto");
        let lib = root.join("include").join("lib.inc");
        std::fs::write(&lib, "").expect("escrever lib");
        let main = root.join("main.pwn");
        let main_text = "#include <lib>\n\nmain()\n{\n\tNewHelper();\n}\n";
        std::fs::write(&main, main_text).expect("escrever main");

        let mut state = WorkspaceState::new();
        state.set_workspace_root(root.clone());
        state.include_paths = vec![root.join("include")];
        state.config.analysis.warn_unused_in_inc = true;
        let lib_uri = format!("file://{}", lib.display());
        let main_uri = format!("file://{}", main.display());
        state.open_document(lib_uri.clone(), "stock NewHelper()\n{\n}\n".into(), 1);
        state.open_document(main_uri.clone(), main_text.into(), 1);
        let has = |uri: &str, code: &str| {
            state
                .analyze(uri)
                .iter()
                .any(|d| d.code == code && d.message.contains("NewHelper"))
        };

        let undeclared_while_open = has(&main_uri, "PP0010");
        let unused_while_open = has(&lib_uri, "PP0006");
        state.close_document(&lib_uri);
        let undeclared_after_close = has(&main_uri, "PP0010");
        let _ = std::fs::remove_dir_all(&root);

        assert!(
            !undeclared_while_open,
            "a declaração não salva não foi vista"
        );
        assert!(
            !unused_while_open,
            "a chamada no outro arquivo não foi vista"
        );
        assert!(undeclared_after_close, "fechado sem salvar, vale o disco");
    }

    #[test]
    fn an_edit_in_another_file_reaches_the_unused_check() {
        // O cache dos identificadores do workspace não pode esconder uma edição:
        // a chamada some do outro arquivo e a função passa a ser acusada. Um
        // cache que nunca se renovasse passaria em todos os outros testes.
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pawnpro-ws-cache-{nanos}"));
        std::fs::create_dir_all(root.join("include")).expect("criar projeto");
        let lib = root.join("include").join("lib.inc");
        std::fs::write(&lib, "stock SharedHelper()\n{\n}\n").expect("escrever lib");
        let other = root.join("other.pwn");
        std::fs::write(
            &other,
            "#include <lib>\n\nmain()\n{\n\tSharedHelper();\n}\n",
        )
        .expect("escrever other");

        let mut state = WorkspaceState::new();
        state.set_workspace_root(root.clone());
        state.include_paths = vec![root.join("include")];
        state.config.analysis.warn_unused_in_inc = true;
        let uri = format!("file://{}", lib.display());
        state.open_document(uri.clone(), std::fs::read_to_string(&lib).expect("ler"), 1);
        let flagged = |state: &WorkspaceState| {
            state
                .analyze(&uri)
                .iter()
                .any(|d| d.code == "PP0006" && d.message.contains("SharedHelper"))
        };

        let used_before = !flagged(&state);

        // A data avança de propósito: a resolução do carimbo pode ser grossa, e
        // o que se testa aqui é o cache seguir a data — sem ajuda do editor.
        std::fs::write(&other, "main()\n{\n}\n").expect("reescrever other");
        std::fs::File::options()
            .write(true)
            .open(&other)
            .and_then(|f| f.set_modified(SystemTime::now() + std::time::Duration::from_secs(5)))
            .expect("avançar a data");
        let flagged_after = flagged(&state);
        let _ = std::fs::remove_dir_all(&root);

        assert!(
            used_before,
            "a chamada em other.pwn deveria contar como uso"
        );
        assert!(flagged_after, "a edição em other.pwn não chegou à análise");
    }
}
