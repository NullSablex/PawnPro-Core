//! Os métodos do compilador.
//!
//! A linha de comando do `pawncc` é montada aqui, a partir da configuração do
//! projeto aberto — a mesma que a engine recebe. Montá-la na extensão, com a
//! cópia da configuração que ela guardava, abria espaço para as duas
//! discordarem.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::compiler::build::build_compile_args;
use crate::compiler::detect::detect_pawncc;
use crate::compiler::flags::{compute_minimal_args, detect_supported_flags};
use crate::config::service::ConfigService;
use crate::project::includes::include_paths_for;

use super::protocol::ResponseError;

/// Os métodos deste módulo.
pub const METHODS: [&str; 2] = ["compiler.detect", "compiler.buildArgs"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildParams {
    file_path: PathBuf,
    #[serde(default)]
    force_debug: bool,
}

/// Executa um método do compilador.
///
/// # Errors
/// [`ResponseError`] quando o método não é daqui, os parâmetros não servem,
/// nenhum projeto foi aberto ou não há compilador.
pub fn dispatch(
    method: &str,
    params: &Value,
    config: &ConfigService,
) -> Result<Value, ResponseError> {
    match method {
        "compiler.detect" => {
            let configured = params.get("path").and_then(Value::as_str);
            let auto = params
                .get("autoDetect")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let root = params
                .get("workspaceRoot")
                .and_then(Value::as_str)
                .map(Path::new);
            detect_pawncc(configured, auto, root)
                .map(|p| json!(p))
                .map_err(|e| ResponseError::internal(&e.to_string()))
        }
        "compiler.buildArgs" => build_args(params, config),
        _ => Err(ResponseError::method_not_found(method)),
    }
}

/// Monta os argumentos para compilar um arquivo.
///
/// Quando a configuração não traz argumentos, a resposta leva também o preset
/// mínimo que foi usado: a extensão o grava, para o usuário ver o que passou a
/// valer.
fn build_args(params: &Value, config: &ConfigService) -> Result<Value, ResponseError> {
    let BuildParams {
        file_path,
        force_debug,
    } = serde_json::from_value(params.clone())
        .map_err(|e| ResponseError::invalid_params(&e.to_string()))?;

    // Só a leitura fica sob a trava: detectar o compilador executa um
    // processo, e segurar a configuração nesse tempo pararia a engine e o
    // observador, que também a esperam.
    let (compiler, include_paths, root) = config
        .read(|m| {
            let c = m.get_all();
            let includes = include_paths_for(c, m.project_root(), file_path.parent());
            (c.compiler.clone(), includes, m.project_root().to_path_buf())
        })
        .ok_or_else(|| {
            ResponseError::invalid_params("nenhum projeto aberto — chame `config.open` antes")
        })?;

    let configured_exe = (!compiler.path.is_empty()).then_some(compiler.path.as_str());
    let search_root = Some(root.as_path()).filter(|p| !p.as_os_str().is_empty());
    let exe = detect_pawncc(configured_exe, compiler.auto_detect, search_root)
        .map_err(|e| ResponseError::internal(&e.to_string()))?;

    let supported = detect_supported_flags(&exe.to_string_lossy());
    let preset = compiler
        .args
        .is_empty()
        .then(|| compute_minimal_args(&supported));
    let built = build_compile_args(
        exe,
        &supported,
        &compiler.args,
        &include_paths,
        &file_path,
        force_debug,
    );
    Ok(json!({ "args": built, "presetArgs": preset }))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn build_args_follow_the_project_config_and_report_the_preset() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("relógio")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("pawnpro-rpc-compiler-{nanos}"));
        let project = dir.join("proj");
        std::fs::create_dir_all(project.join(".pawnpro")).expect("criar projeto");

        // Um `pawncc` de mentira: só imprime a ajuda, que é de onde saem as
        // flags aceitas.
        let fake = dir.join("pawncc");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho '        -d<num>  debugging level'\necho '        -O<num>  optimization'\n",
        )
        .expect("escrever pawncc falso");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(
            project.join(".pawnpro").join("config.json"),
            json!({ "compiler": { "path": fake, "autoDetect": false } }).to_string(),
        )
        .expect("escrever config");

        let config = ConfigService::with_home(Some(dir.join("home")));
        config.open(&project);
        let source = project.join("gamemodes").join("main.pwn");
        let answer = dispatch(
            "compiler.buildArgs",
            &json!({ "filePath": source }),
            &config,
        );
        let _ = std::fs::remove_dir_all(&dir);
        let answer = answer.expect("montar");

        // Sem argumentos na configuração: vale o preset, e só com o que esta
        // build aceita.
        assert_eq!(answer["presetArgs"], json!(["-d1", "-O1"]));
        assert_eq!(answer["args"]["exe"], json!(fake));
        let args = answer["args"]["args"].as_array().expect("lista");
        assert_eq!(
            args.last(),
            Some(&json!(source)),
            "o arquivo vem por último"
        );
        let amx = format!("-o{}", project.join("gamemodes").join("main.amx").display());
        assert!(args.contains(&json!(amx)), "{args:?}");
        // A extensão lê `removedFlags`, não `removed_flags`.
        assert!(answer["args"].get("removedFlags").is_some());
    }

    #[test]
    fn build_args_refuse_before_a_project_opens() {
        let config = ConfigService::with_home(None);
        let out = dispatch(
            "compiler.buildArgs",
            &json!({ "filePath": "/p/a.pwn" }),
            &config,
        );
        assert_eq!(out.err().map(|e| e.code), Some(-32602));
    }
}
