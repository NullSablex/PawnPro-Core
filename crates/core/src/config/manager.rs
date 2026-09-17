//! Leitura, mesclagem e escrita da configuração.
//!
//! Duas camadas: `~/.pawnpro/config.json` (global) e `.pawnpro/config.json` (do
//! projeto), com o projeto sobrescrevendo o global e ambos sobre os padrões.
//!
//! A mesclagem acontece no JSON bruto, antes de virar [`PawnProConfig`]:
//! depois seria impossível distinguir um campo escrito de um preenchido pelo
//! padrão.
//!
//! Chaves como `__proto__` não têm tratamento especial: num `serde_json::Map`
//! são texto como qualquer outro.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::types::PawnProConfig;

/// Pasta de configuração do PawnPro, no projeto e no diretório do usuário.
pub const PAWNPRO_DIR: &str = ".pawnpro";

/// Acima disto o arquivo não é lido: parsear centenas de megabytes travaria a
/// extensão, e nenhuma configuração legítima chega perto.
const MAX_CONFIG_BYTES: u64 = 32 * 1024 * 1024;

/// De onde vem, ou para onde vai, um valor de configuração.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `~/.pawnpro/config.json` — vale para todos os projetos.
    Global,
    /// `.pawnpro/config.json` — sobrescreve o global.
    Project,
}

/// Erros de manipulação da configuração.
#[derive(Debug)]
pub enum ConfigError {
    /// Caminho de chave vazio ou com segmento vazio (`a..b`).
    InvalidKey {
        path: String,
    },
    Io(io::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKey { path } => write!(f, "chave de configuração inválida: {path:?}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Arquivo ausente, grande demais ou malformado resulta em vazio: a
/// configuração cai nos padrões em vez de impedir o projeto de abrir.
fn read_json_object(path: &Path) -> Map<String, Value> {
    let too_big = fs::metadata(path).is_ok_and(|m| m.len() > MAX_CONFIG_BYTES);
    if too_big {
        return Map::new();
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| match v {
            Value::Object(map) => Some(map),
            // Uma lista ou um número não são configuração.
            _ => None,
        })
        .unwrap_or_default()
}

/// Temporário e `rename`: uma escrita interrompida deixaria o `config.json`
/// do usuário truncado.
fn write_json_object(path: &Path, data: &Map<String, Value>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut json = serde_json::to_string_pretty(data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

/// Mescla `overlay` sobre `base`, descendo em objetos aninhados.
///
/// Uma lista substitui a lista inteira: concatenar tornaria impossível remover
/// um item herdado do global.
fn deep_merge(base: &mut Map<String, Value>, overlay: &Map<String, Value>) {
    for (key, ov) in overlay {
        match (base.get_mut(key), ov) {
            (Some(Value::Object(bv)), Value::Object(om)) => deep_merge(bv, om),
            _ => {
                base.insert(key.clone(), ov.clone());
            }
        }
    }
}

/// Substitui `${workspaceFolder}` em todo texto da árvore.
fn substitute_workspace(value: &mut Value, workspace_root: &str) {
    match value {
        Value::String(s) => {
            if s.contains("${workspaceFolder}") {
                *s = s.replace("${workspaceFolder}", workspace_root);
            }
        }
        Value::Array(items) => {
            for item in items {
                substitute_workspace(item, workspace_root);
            }
        }
        Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                substitute_workspace(v, workspace_root);
            }
        }
        _ => {}
    }
}

/// Grava `value` num caminho pontuado já validado, criando os objetos
/// intermediários.
///
/// Um segmento que não é objeto é substituído: o caminho pedido tem
/// precedência sobre um valor de tipo incompatível.
fn insert_at(root: &mut Map<String, Value>, dot_path: &str, value: Value) {
    let parts: Vec<&str> = dot_path.split('.').collect();
    let Some((leaf, branches)) = parts.split_last() else {
        return;
    };
    let mut cursor = root;
    for key in branches {
        let entry = cursor
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        let Some(next) = entry.as_object_mut() else {
            return;
        };
        cursor = next;
    }
    cursor.insert((*leaf).to_string(), value);
}

/// O caminho de uma chave como ponteiro JSON (RFC 6901).
fn json_pointer(path: &[String]) -> String {
    path.iter().fold(String::new(), |mut acc, key| {
        acc.push('/');
        acc.push_str(&key.replace('~', "~0").replace('/', "~1"));
        acc
    })
}

/// Aplica `overlay` sobre `root` folha por folha, recusando cada valor que
/// não caiba no tipo.
///
/// Desserializar a árvore inteira de uma vez faria um único `"locale": 5`
/// descartar toda a configuração — includes, SDK, naming — em silêncio. Aqui o
/// valor errado volta ao padrão e os outros continuam valendo.
fn merge_leniently(
    root: &mut Value,
    path: &mut Vec<String>,
    overlay: &Map<String, Value>,
    rejected: &mut Vec<String>,
) {
    for (key, incoming) in overlay {
        path.push(key.clone());
        let pointer = json_pointer(path);
        match (root.pointer(&pointer).cloned(), incoming) {
            (Some(Value::Object(_)), Value::Object(nested)) => {
                merge_leniently(root, path, nested, rejected);
            }
            (Some(previous), _) => {
                if let Some(slot) = root.pointer_mut(&pointer) {
                    *slot = incoming.clone();
                }
                if serde_json::from_value::<PawnProConfig>(root.clone()).is_err() {
                    if let Some(slot) = root.pointer_mut(&pointer) {
                        *slot = previous;
                    }
                    rejected.push(path.join("."));
                }
            }
            // Chave que a configuração não conhece: o `serde` a ignoraria de
            // qualquer forma.
            (None, _) => {}
        }
        path.pop();
    }
}

/// Monta a configuração a partir do JSON já mesclado, tolerando valores de
/// tipo errado. Devolve também as chaves recusadas.
fn lenient_config(
    merged: &Map<String, Value>,
    workspace_root: &str,
) -> (PawnProConfig, Vec<String>) {
    let Ok(mut root) = serde_json::to_value(PawnProConfig::default()) else {
        return (PawnProConfig::default(), Vec::new());
    };
    let mut rejected = Vec::new();
    merge_leniently(&mut root, &mut Vec::new(), merged, &mut rejected);
    // Depois da mescla, e não antes: os padrões também usam
    // `${workspaceFolder}`. Resolver só o que o usuário escreveu deixava
    // `server.cwd`, os includes e os arquivos de lista padrão com o texto
    // literal — e o `.ban` acabava criado num caminho relativo sem sentido.
    substitute_workspace(&mut root, workspace_root);
    let config = serde_json::from_value(root).unwrap_or_default();
    (config, rejected)
}

/// Lê, mescla e grava a configuração de um projeto.
#[derive(Debug)]
pub struct ConfigManager {
    project_root: PathBuf,
    global_path: PathBuf,
    project_path: PathBuf,
    /// O JSON como está no arquivo, sem defaults nem substituições.
    ///
    /// Gravar o merged congelaria os padrões daquela versão dentro do arquivo
    /// do usuário.
    raw_global: Map<String, Value>,
    raw_project: Map<String, Value>,
    merged: PawnProConfig,
    /// Chaves cujo valor tinha o tipo errado e ficaram no padrão.
    rejected: Vec<String>,
}

impl ConfigManager {
    /// Abre a configuração de um projeto.
    #[must_use]
    pub fn new(project_root: &Path, home: &Path) -> Self {
        let mut manager = Self {
            project_root: project_root.to_path_buf(),
            global_path: home.join(PAWNPRO_DIR).join("config.json"),
            project_path: project_root.join(PAWNPRO_DIR).join("config.json"),
            raw_global: Map::new(),
            raw_project: Map::new(),
            merged: PawnProConfig::default(),
            rejected: Vec::new(),
        };
        manager.reload();
        manager
    }

    /// A pasta do projeto, que resolve `${workspaceFolder}`.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    #[must_use]
    pub fn global_config_path(&self) -> &Path {
        &self.global_path
    }

    #[must_use]
    pub fn project_config_path(&self) -> &Path {
        &self.project_path
    }

    /// Relê os dois arquivos e refaz a mesclagem.
    pub fn reload(&mut self) {
        self.raw_global = read_json_object(&self.global_path);
        self.raw_project = read_json_object(&self.project_path);
        self.apply_merge();
    }

    fn apply_merge(&mut self) {
        let mut merged = self.raw_global.clone();
        deep_merge(&mut merged, &self.raw_project);

        let (config, rejected) = lenient_config(&merged, &self.project_root.to_string_lossy());
        if !rejected.is_empty() {
            crate::diag_warn!(
                "core/config",
                "valor de tipo inválido ignorado, ficou o padrão: {}",
                rejected.join(", ")
            );
        }
        self.merged = config;
        self.rejected = rejected;
    }

    #[must_use]
    pub const fn get_all(&self) -> &PawnProConfig {
        &self.merged
    }

    /// As chaves cujo valor tinha o tipo errado e ficaram no padrão.
    #[must_use]
    pub fn rejected_keys(&self) -> &[String] {
        &self.rejected
    }

    /// Uma lista de `analysis.naming` como o projeto a escreveu.
    ///
    /// A migração precisa do que o desenvolvedor colocou inline, não do
    /// resultado da mesclagem.
    #[must_use]
    pub fn raw_project_naming_list(&self, key: &str) -> Vec<String> {
        self.raw_project
            .get("analysis")
            .and_then(Value::as_object)
            .and_then(|a| a.get("naming"))
            .and_then(Value::as_object)
            .and_then(|n| n.get(key))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn path_for(&self, scope: Scope) -> &Path {
        match scope {
            Scope::Global => &self.global_path,
            Scope::Project => &self.project_path,
        }
    }

    /// Grava vários valores numa escrita só.
    ///
    /// Uma escrita por chave avisaria quem observa uma vez por chave, com
    /// estados intermediários que ninguém pediu.
    ///
    /// # Errors
    /// Algum caminho vazio ou com segmento vazio — e então nada é gravado —,
    /// ou falha de escrita.
    pub fn set_keys(
        &mut self,
        entries: &[(String, Value)],
        scope: Scope,
    ) -> Result<(), ConfigError> {
        if let Some((bad, _)) = entries
            .iter()
            .find(|(dot_path, _)| dot_path.split('.').any(str::is_empty))
        {
            return Err(ConfigError::InvalidKey { path: bad.clone() });
        }

        let path = self.path_for(scope).to_path_buf();
        let mut current = read_json_object(&path);
        for (dot_path, value) in entries {
            insert_at(&mut current, dot_path, value.clone());
        }

        write_json_object(&path, &current)?;
        self.reload();
        Ok(())
    }

    /// Remove uma chave do escopo. Ausente é no-op.
    ///
    /// # Errors
    /// Caminho vazio ou com segmento vazio, ou falha de escrita.
    pub fn delete_key(&mut self, dot_path: &str, scope: Scope) -> Result<(), ConfigError> {
        let parts: Vec<&str> = dot_path.split('.').collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(ConfigError::InvalidKey {
                path: dot_path.to_string(),
            });
        }

        let path = self.path_for(scope).to_path_buf();
        let mut current = read_json_object(&path);

        let Some((leaf, branches)) = parts.split_last() else {
            return Err(ConfigError::InvalidKey {
                path: dot_path.to_string(),
            });
        };
        let mut cursor = &mut current;
        for key in branches {
            match cursor.get_mut(*key).and_then(Value::as_object_mut) {
                Some(next) => cursor = next,
                // O caminho não existe: não há o que remover.
                None => return Ok(()),
            }
        }
        if cursor.remove(*leaf).is_none() {
            return Ok(());
        }

        write_json_object(&path, &current)?;
        self.reload();
        Ok(())
    }
}

/// Atalhos só dos testes: a produção grava por `set_keys` e lê por `get_all`.
#[cfg(test)]
impl ConfigManager {
    /// O JSON bruto de um escopo, sem defaults nem substituições.
    fn raw(&self, scope: Scope) -> &Map<String, Value> {
        match scope {
            Scope::Global => &self.raw_global,
            Scope::Project => &self.raw_project,
        }
    }

    /// Grava um valor só.
    pub(crate) fn set_key(
        &mut self,
        dot_path: &str,
        value: Value,
        scope: Scope,
    ) -> Result<(), ConfigError> {
        self.set_keys(&[(dot_path.to_string(), value)], scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            p.push(format!("pawnpro-cfg-{tag}-{nanos}"));
            fs::create_dir_all(&p).expect("criar temp");
            Self(p)
        }
        fn write_config(&self, sub: &str, body: &str) {
            let dir = self.0.join(sub).join(PAWNPRO_DIR);
            fs::create_dir_all(&dir).expect("criar dir");
            fs::write(dir.join("config.json"), body).expect("escrever");
        }
        fn manager(&self) -> ConfigManager {
            ConfigManager::new(&self.0.join("proj"), &self.0.join("home"))
        }
        /// Os padrões como o manager os entrega: com `${workspaceFolder}`
        /// resolvido para a pasta do projeto.
        fn resolved_defaults(&self) -> PawnProConfig {
            let mut value = serde_json::to_value(PawnProConfig::default()).expect("serializar");
            substitute_workspace(&mut value, &self.0.join("proj").to_string_lossy());
            serde_json::from_value(value).expect("desserializar")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_files_yields_the_defaults() {
        let tmp = TempDir::new("empty");
        assert_eq!(tmp.manager().get_all(), &tmp.resolved_defaults());
    }

    #[test]
    fn the_defaults_are_resolved_against_the_project() {
        // Sem arquivo nenhum, os padrões que citam a pasta do projeto chegam
        // com ela resolvida — um `cwd` literal `${workspaceFolder}` não existe
        // no disco.
        let tmp = TempDir::new("defaults-resolved");
        let m = tmp.manager();
        let root = tmp.0.join("proj").display().to_string();
        let c = m.get_all();
        assert_eq!(c.server.cwd, root);
        assert_eq!(c.include_paths, [format!("{root}/pawno/include")]);
        assert!(c.analysis.naming.blocklist_file.starts_with(&root));
        assert!(c.analysis.naming.loop_indices_file.starts_with(&root));
    }

    #[test]
    fn the_project_overrides_the_global() {
        let tmp = TempDir::new("override");
        tmp.write_config("home", r#"{"output":{"encoding":"utf-8"}}"#);
        tmp.write_config("proj", r#"{"output":{"encoding":"latin1"}}"#);
        assert_eq!(tmp.manager().get_all().output.encoding, "latin1");
    }

    #[test]
    fn a_field_the_project_omits_keeps_the_global_value() {
        // É o caso que o merge existe para resolver: mesclar depois de virar
        // struct não distinguiria "não escrito" de "igual ao padrão".
        let tmp = TempDir::new("partial");
        tmp.write_config(
            "home",
            r#"{"compiler":{"path":"/global/pawncc","autoDetect":false}}"#,
        );
        tmp.write_config("proj", r#"{"compiler":{"path":"/proj/pawncc"}}"#);
        let m = tmp.manager();
        assert_eq!(m.get_all().compiler.path, "/proj/pawncc");
        assert!(
            !m.get_all().compiler.auto_detect,
            "perdeu autoDetect do global"
        );
    }

    #[test]
    fn a_list_replaces_instead_of_appending() {
        // Acrescentar tornaria impossível remover um item herdado do global.
        let tmp = TempDir::new("lists");
        tmp.write_config("home", r#"{"includePaths":["/a","/b"]}"#);
        tmp.write_config("proj", r#"{"includePaths":["/c"]}"#);
        assert_eq!(tmp.manager().get_all().include_paths, ["/c"]);
    }

    #[test]
    fn workspace_folder_is_substituted() {
        let tmp = TempDir::new("subst");
        tmp.write_config("proj", r#"{"includePaths":["${workspaceFolder}/inc"]}"#);
        let m = tmp.manager();
        let expected = format!("{}/inc", tmp.0.join("proj").display());
        assert_eq!(m.get_all().include_paths, [expected]);
    }

    #[test]
    fn substitution_reaches_nested_values() {
        let tmp = TempDir::new("subst-deep");
        tmp.write_config("proj", r#"{"server":{"cwd":"${workspaceFolder}/srv"}}"#);
        let m = tmp.manager();
        assert!(m.get_all().server.cwd.ends_with("/srv"));
        assert!(!m.get_all().server.cwd.contains("${workspaceFolder}"));
    }

    #[test]
    fn a_malformed_file_falls_back_to_the_defaults() {
        // Um JSON truncado não pode impedir o projeto de abrir.
        let tmp = TempDir::new("broken");
        tmp.write_config("proj", "{ \"compiler\": ");
        assert_eq!(tmp.manager().get_all(), &tmp.resolved_defaults());
    }

    #[test]
    fn a_wrong_type_discards_only_that_field() {
        // Antes, um único valor errado jogava fora a configuração inteira — a
        // engine perdia os includes do projeto sem aviso nenhum.
        let tmp = TempDir::new("wrong-type");
        tmp.write_config("proj", r#"{"locale":5,"compiler":{"path":"/x"}}"#);
        let m = tmp.manager();
        assert_eq!(m.get_all().compiler.path, "/x");
        assert_eq!(m.get_all().locale, "");
        assert_eq!(m.rejected_keys(), ["locale"]);
    }

    #[test]
    fn an_unknown_enum_value_keeps_the_rest_of_the_block() {
        let tmp = TempDir::new("bad-enum");
        tmp.write_config(
            "proj",
            r#"{"server":{"output":{"follow":"sempre"},"path":"/srv"}}"#,
        );
        let m = tmp.manager();
        assert_eq!(m.get_all().server.path, "/srv");
        assert_eq!(
            m.get_all().server.output.follow,
            crate::config::types::FollowMode::Visible
        );
        assert_eq!(m.rejected_keys(), ["server.output.follow"]);
    }

    #[test]
    fn a_section_that_is_not_an_object_is_rejected() {
        let tmp = TempDir::new("bad-section");
        tmp.write_config("proj", r#"{"compiler":"x","includePaths":["/inc"]}"#);
        let m = tmp.manager();
        assert_eq!(
            m.get_all().compiler,
            crate::config::types::CompilerConfig::default()
        );
        assert_eq!(m.get_all().include_paths, ["/inc"]);
        assert_eq!(m.rejected_keys(), ["compiler"]);
    }

    #[test]
    fn a_list_with_a_bad_element_falls_back_whole() {
        // Aproveitar só os elementos bons daria uma lista que ninguém escreveu.
        let tmp = TempDir::new("bad-list");
        tmp.write_config("proj", r#"{"includePaths":["/a",1]}"#);
        let m = tmp.manager();
        assert_eq!(
            m.get_all().include_paths,
            tmp.resolved_defaults().include_paths
        );
        assert_eq!(m.rejected_keys(), ["includePaths"]);
    }

    #[test]
    fn a_valid_file_rejects_nothing() {
        let tmp = TempDir::new("valid");
        tmp.write_config(
            "proj",
            r#"{"locale":"ru","analysis":{"naming":{"style":{"functions":["camelCase","/^PP_/"]}}}}"#,
        );
        let m = tmp.manager();
        assert!(m.rejected_keys().is_empty());
        assert_eq!(m.get_all().analysis.naming.style.functions.len(), 2);
    }

    #[test]
    fn a_json_that_is_not_an_object_is_ignored() {
        let tmp = TempDir::new("notobj");
        tmp.write_config("proj", "[1,2,3]");
        assert_eq!(tmp.manager().get_all(), &tmp.resolved_defaults());
    }

    #[test]
    fn prototype_keys_are_plain_text_here() {
        // Num `serde_json::Map` a chave é texto comum e não afeta mais nada.
        let tmp = TempDir::new("proto");
        let mut m = tmp.manager();
        m.set_key("__proto__.x", json!(1), Scope::Project)
            .expect("gravar");
        assert_eq!(m.get_all(), &tmp.resolved_defaults());
        assert!(m.raw(Scope::Project).contains_key("__proto__"));
    }

    #[test]
    fn set_key_creates_the_intermediate_objects() {
        let tmp = TempDir::new("setkey");
        let mut m = tmp.manager();
        m.set_key("server.output.follow", json!("always"), Scope::Project)
            .expect("gravar");
        assert_eq!(
            m.get_all().server.output.follow,
            crate::config::types::FollowMode::Always
        );
    }

    #[test]
    fn set_key_persists_to_disk() {
        let tmp = TempDir::new("persist");
        {
            let mut m = tmp.manager();
            m.set_key("locale", json!("ru"), Scope::Project)
                .expect("gravar");
        }
        // Um manager novo lê o que o anterior gravou.
        assert_eq!(tmp.manager().get_all().locale, "ru");
    }

    #[test]
    fn set_key_writes_only_to_the_chosen_scope() {
        let tmp = TempDir::new("scope");
        let mut m = tmp.manager();
        m.set_key("locale", json!("es"), Scope::Global)
            .expect("gravar");
        assert!(m.raw(Scope::Global).contains_key("locale"));
        assert!(!m.raw(Scope::Project).contains_key("locale"));
    }

    #[test]
    fn an_empty_key_segment_is_rejected() {
        let tmp = TempDir::new("badkey");
        let mut m = tmp.manager();
        assert!(m.set_key("a..b", json!(1), Scope::Project).is_err());
        assert!(m.set_key("", json!(1), Scope::Project).is_err());
        assert!(m.delete_key("a..b", Scope::Project).is_err());
    }

    #[test]
    fn a_non_object_segment_is_replaced() {
        // O caminho pedido tem precedência sobre um valor de tipo incompatível.
        let tmp = TempDir::new("clash");
        let mut m = tmp.manager();
        m.set_key("server", json!("texto"), Scope::Project)
            .expect("gravar");
        m.set_key("server.path", json!("/x"), Scope::Project)
            .expect("gravar");
        assert_eq!(m.get_all().server.path, "/x");
    }

    #[test]
    fn set_keys_writes_everything_at_once() {
        let tmp = TempDir::new("setkeys");
        let mut m = tmp.manager();
        m.set_keys(
            &[
                ("syntax.scheme".to_string(), json!("classic_dark")),
                ("syntax.applyOnStartup".to_string(), json!(true)),
            ],
            Scope::Project,
        )
        .expect("gravar");
        assert!(m.get_all().syntax.apply_on_startup);
        assert_eq!(m.raw(Scope::Project)["syntax"]["scheme"], "classic_dark");
    }

    #[test]
    fn one_bad_key_writes_nothing() {
        // Metade gravada seria pior que nada: o usuário veria só parte do que
        // pediu, sem saber qual.
        let tmp = TempDir::new("setkeys-bad");
        let mut m = tmp.manager();
        let result = m.set_keys(
            &[
                ("locale".to_string(), json!("ru")),
                ("a..b".to_string(), json!(1)),
            ],
            Scope::Project,
        );
        assert!(result.is_err());
        assert!(!m.raw(Scope::Project).contains_key("locale"));
    }

    #[test]
    fn delete_key_removes_the_value() {
        let tmp = TempDir::new("del");
        let mut m = tmp.manager();
        m.set_key("locale", json!("ru"), Scope::Project)
            .expect("gravar");
        m.delete_key("locale", Scope::Project).expect("remover");
        assert_eq!(m.get_all().locale, "");
        assert!(!m.raw(Scope::Project).contains_key("locale"));
    }

    #[test]
    fn deleting_something_absent_is_not_an_error() {
        let tmp = TempDir::new("del-missing");
        let mut m = tmp.manager();
        assert!(m.delete_key("nao.existe.mesmo", Scope::Project).is_ok());
    }

    #[test]
    fn raw_naming_list_reads_what_the_project_wrote() {
        // A migração precisa do que o dev escreveu inline, não do merged.
        let tmp = TempDir::new("raw");
        tmp.write_config("proj", r#"{"analysis":{"naming":{"blocklist":["x","y"]}}}"#);
        let m = tmp.manager();
        assert_eq!(m.raw_project_naming_list("blocklist"), ["x", "y"]);
        // Ausente devolve vazio, não o padrão.
        assert!(m.raw_project_naming_list("allowShortInLoops").is_empty());
    }

    #[test]
    fn writing_leaves_no_temporary_file() {
        let tmp = TempDir::new("atomic");
        let mut m = tmp.manager();
        m.set_key("locale", json!("en"), Scope::Project)
            .expect("gravar");
        let dir = tmp.0.join("proj").join(PAWNPRO_DIR);
        let leftovers = fs::read_dir(&dir)
            .expect("listar")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn an_oversized_file_is_not_parsed() {
        // Parsear centenas de megabytes travaria a extensão, e nenhuma
        // configuração legítima chega perto do teto.
        let tmp = TempDir::new("huge");
        let dir = tmp.0.join("proj").join(PAWNPRO_DIR);
        fs::create_dir_all(&dir).expect("criar");
        let path = dir.join("config.json");
        let filler = " ".repeat(usize::try_from(MAX_CONFIG_BYTES).unwrap_or(usize::MAX) + 1);
        fs::write(&path, format!("{{\"locale\":\"ru\"}}{filler}")).expect("escrever");
        assert_eq!(tmp.manager().get_all().locale, "");
    }
}
