//! Os métodos dos includes: onde procurar, o que há lá e o SDK.
//!
//! As raízes e o SDK saem da configuração do projeto aberto, pelas mesmas
//! funções que alimentam a engine. A extensão montava a própria lista e
//! resolvia o próprio SDK — e o aviso de "SDK não encontrado" podia dizer uma
//! coisa enquanto a análise usava outra.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::service::ConfigService;
use crate::project::includes::{include_paths_for, list_inc_files_recursive, list_natives};
use crate::supervisor::engine::resolve_sdk_file_path;

use super::protocol::ResponseError;

/// Os métodos deste módulo.
pub const METHODS: [&str; 4] = [
    "includes.paths",
    "includes.listFiles",
    "includes.listNatives",
    "includes.resolveSdk",
];

/// Profundidade padrão da varredura: basta para qualquer árvore de includes
/// real e ainda corta um symlink que gire em círculo.
const DEFAULT_MAX_DEPTH: usize = 20;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathsParams {
    /// A pasta do arquivo aberto: serve quando o projeto não tem include na
    /// raiz, mas um subprojeto tem.
    #[serde(default)]
    file_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFilesParams {
    root: PathBuf,
    #[serde(default = "default_max_depth")]
    max_depth: usize,
}

const fn default_max_depth() -> usize {
    DEFAULT_MAX_DEPTH
}

#[derive(Debug, Deserialize)]
struct NativesParams {
    file: PathBuf,
}

/// Executa um método dos includes.
///
/// # Errors
/// [`ResponseError`] quando o método não é daqui, os parâmetros não servem ou
/// nenhum projeto foi aberto.
pub fn dispatch(
    method: &str,
    params: &Value,
    config: &ConfigService,
) -> Result<Value, ResponseError> {
    match method {
        "includes.paths" => {
            let PathsParams { file_dir } = parse(params)?;
            let paths = config
                .read(|m| include_paths_for(m.get_all(), m.project_root(), file_dir.as_deref()))
                .ok_or_else(not_open)?;
            Ok(json!(paths))
        }
        "includes.listFiles" => {
            let ListFilesParams { root, max_depth } = parse(params)?;
            Ok(json!(list_inc_files_recursive(&root, max_depth)))
        }
        "includes.listNatives" => {
            let NativesParams { file } = parse(params)?;
            Ok(json!(list_natives(&file)))
        }
        // O mesmo cálculo que entrega o SDK à engine, com os mesmos includes:
        // o aviso passa a dizer o que a análise de fato usa.
        "includes.resolveSdk" => {
            let found = config
                .read(|m| {
                    let cfg = m.get_all();
                    let includes = include_paths_for(cfg, m.project_root(), None);
                    resolve_sdk_file_path(
                        cfg.analysis.sdk.platform,
                        &cfg.analysis.sdk.file_path,
                        &includes,
                        m.project_root(),
                    )
                })
                .ok_or_else(not_open)?;
            Ok(json!(found))
        }
        _ => Err(ResponseError::method_not_found(method)),
    }
}

fn parse<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, ResponseError> {
    serde_json::from_value(params.clone())
        .map_err(|e| ResponseError::invalid_params(&e.to_string()))
}

fn not_open() -> ResponseError {
    ResponseError::invalid_params("nenhum projeto aberto — chame `config.open` antes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Sandbox(PathBuf);

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("pawnpro-rpc-inc-{tag}-{nanos}"));
            std::fs::create_dir_all(dir.join("proj").join(".pawnpro")).expect("criar");
            Self(dir)
        }
        fn project(&self) -> PathBuf {
            self.0.join("proj")
        }
        fn open(&self, config: &serde_json::Value) -> Arc<ConfigService> {
            std::fs::write(
                self.project().join(".pawnpro").join("config.json"),
                config.to_string(),
            )
            .expect("escrever config");
            let service = ConfigService::with_home(Some(self.0.join("home")));
            service.open(&self.project());
            service
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn paths_follow_the_project_config() {
        let sandbox = Sandbox::new("paths");
        let libs = sandbox.project().join("libs");
        std::fs::create_dir_all(&libs).expect("criar libs");
        std::fs::create_dir_all(sandbox.project().join("pawno").join("include")).expect("criar");
        let service = sandbox.open(&json!({ "includePaths": [libs] }));

        let paths = dispatch("includes.paths", &json!({}), &service).expect("listar");
        // O configurado primeiro, o descoberto depois.
        assert_eq!(
            paths,
            json!([libs, sandbox.project().join("pawno").join("include")])
        );
    }

    #[test]
    fn the_sdk_is_the_one_the_engine_would_get() {
        let sandbox = Sandbox::new("sdk");
        let include = sandbox.project().join("qawno").join("include");
        std::fs::create_dir_all(&include).expect("criar");
        std::fs::write(include.join("open.mp.inc"), "").expect("escrever sdk");
        let service = sandbox.open(&json!({ "analysis": { "sdk": { "platform": "omp" } } }));

        let found = dispatch("includes.resolveSdk", &json!({}), &service).expect("resolver");
        assert_eq!(found, json!(include.join("open.mp.inc")));
    }

    #[test]
    fn a_missing_sdk_comes_back_as_null() {
        // É o `null` que faz a extensão avisar.
        let sandbox = Sandbox::new("no-sdk");
        let service = sandbox.open(&json!({ "analysis": { "sdk": { "platform": "omp" } } }));
        let found = dispatch("includes.resolveSdk", &json!({}), &service).expect("resolver");
        assert_eq!(found, Value::Null);
    }

    #[test]
    fn the_listing_skips_the_pawnpro_folder() {
        let sandbox = Sandbox::new("list");
        std::fs::write(sandbox.project().join("a.inc"), "").expect("escrever");
        std::fs::write(sandbox.project().join(".pawnpro").join("b.inc"), "").expect("escrever");
        let service = sandbox.open(&json!({}));

        let files = dispatch(
            "includes.listFiles",
            &json!({ "root": sandbox.project() }),
            &service,
        )
        .expect("listar");
        assert_eq!(files, json!([sandbox.project().join("a.inc")]));
    }

    #[test]
    fn project_methods_refuse_before_a_project_opens() {
        let service = ConfigService::with_home(None);
        for method in ["includes.paths", "includes.resolveSdk"] {
            let out = dispatch(method, &json!({}), &service);
            assert_eq!(out.err().map(|e| e.code), Some(-32602), "{method}");
        }
    }
}
