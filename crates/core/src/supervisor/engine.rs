//! O que a engine recebe na inicialização do LSP: caminhos de include, SDK e
//! estilo de formatação.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::config::types::{FormatPreset, PawnProConfig, SdkPlatform};

/// Nome do arquivo do SDK do open.mp.
const OMP_SDK_FILE: &str = "open.mp.inc";

/// Localiza o arquivo de SDK, que descreve as nativas da plataforma.
///
/// O configurado vence, mas só se existir: apontar para um arquivo ausente é
/// engano do usuário, e um palpite esconderia isso.
#[must_use]
pub fn resolve_sdk_file_path(
    platform: SdkPlatform,
    configured: &str,
    include_paths: &[PathBuf],
    workspace_root: &Path,
) -> Option<PathBuf> {
    if platform == SdkPlatform::None {
        return None;
    }

    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        return path.exists().then_some(path);
    }

    // Só o open.mp tem arquivo de SDK; no SA-MP as nativas vêm dos includes.
    if !matches!(platform, SdkPlatform::Omp | SdkPlatform::Auto) {
        return None;
    }

    // O local padrão do open.mp vem antes dos includes configurados: é onde o
    // servidor o instala, e um homônimo num include do usuário não deve vencer.
    let ws_default = workspace_root
        .join("qawno")
        .join("include")
        .join(OMP_SDK_FILE);
    if ws_default.exists() {
        return Some(ws_default);
    }

    include_paths
        .iter()
        .map(|dir| dir.join(OMP_SDK_FILE))
        .find(|p| p.exists())
}

/// O que a engine recebe na inicialização.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineSettings {
    /// Raízes de include já resolvidas e existentes.
    pub resolved_paths: Vec<PathBuf>,
    /// Vazio quando não há SDK — a engine trata como ausência.
    pub sdk_file_path: String,
}

/// Monta o que a engine precisa a partir da configuração.
#[must_use]
pub fn build_engine_settings(cfg: &PawnProConfig, workspace_root: &Path) -> EngineSettings {
    let resolved_paths = crate::project::includes::build_include_paths(
        &cfg.include_paths
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>(),
        &cfg.compiler.args,
        workspace_root,
        None,
    );
    let sdk_file_path = resolve_sdk_file_path(
        cfg.analysis.sdk.platform,
        &cfg.analysis.sdk.file_path,
        &resolved_paths,
        workspace_root,
    )
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_default();

    EngineSettings {
        resolved_paths,
        sdk_file_path,
    }
}

/// Opções de formatação enviadas à engine.
///
/// Os ajustes finos só acompanham o preset `custom`: os prontos definem os
/// próprios valores, e mandar os do usuário junto sobrescreveria o preset.
#[must_use]
pub fn build_format_options(cfg: &PawnProConfig) -> Map<String, Value> {
    let fmt = &cfg.format;
    let mut opts = Map::new();
    opts.insert("formatPreset".into(), json!(fmt.preset));

    if fmt.preset == FormatPreset::Custom {
        opts.insert("formatBraceStyle".into(), json!(fmt.brace_style));
        opts.insert(
            "formatSpaceAroundOperators".into(),
            json!(fmt.space_around_operators),
        );
        opts.insert(
            "formatEmptyBlockSameLine".into(),
            json!(fmt.empty_block_same_line),
        );
    }

    // Ortogonal ao preset: vale para Allman, K&R, Compacto e Custom.
    opts.insert(
        "formatPreserveArrayAlignment".into(),
        json!(fmt.preserve_array_alignment),
    );
    opts
}

/// Vai aninhada em `naming`: a engine desserializa direto na `NamingConfig`
/// dela.
#[must_use]
pub fn build_naming_options(cfg: &PawnProConfig) -> Map<String, Value> {
    let mut opts = Map::new();
    opts.insert("naming".into(), json!(cfg.analysis.naming));
    opts
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
            p.push(format!("pawnpro-engine-{tag}-{nanos}"));
            std::fs::create_dir_all(&p).expect("criar temp");
            Self(p)
        }
        fn file(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("criar dir");
            }
            std::fs::write(&path, "").expect("escrever");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_none_platform_has_no_sdk() {
        let tmp = TempDir::new("none");
        assert_eq!(
            resolve_sdk_file_path(SdkPlatform::None, "", &[], &tmp.0),
            None
        );
    }

    #[test]
    fn a_configured_path_wins() {
        let tmp = TempDir::new("configured");
        let custom = tmp.file("meu/sdk.inc");
        tmp.file("qawno/include/open.mp.inc");
        let found =
            resolve_sdk_file_path(SdkPlatform::Auto, &custom.to_string_lossy(), &[], &tmp.0);
        assert_eq!(found, Some(custom));
    }

    #[test]
    fn a_configured_path_that_does_not_exist_is_not_replaced_by_a_guess() {
        // Apontar para um arquivo ausente é engano do usuário; cair num palpite
        // esconderia isso e a engine analisaria com o SDK errado.
        let tmp = TempDir::new("bad-config");
        tmp.file("qawno/include/open.mp.inc");
        assert_eq!(
            resolve_sdk_file_path(SdkPlatform::Auto, "/nao/existe.inc", &[], &tmp.0),
            None
        );
    }

    #[test]
    fn the_workspace_default_comes_before_the_include_paths() {
        // É onde o servidor instala o SDK; um homônimo num include do usuário
        // não deve vencer.
        let tmp = TempDir::new("order");
        let default = tmp.file("qawno/include/open.mp.inc");
        let other = tmp.file("outro/open.mp.inc");
        let found = resolve_sdk_file_path(
            SdkPlatform::Omp,
            "",
            std::slice::from_ref(&other.parent().expect("pai").to_path_buf()),
            &tmp.0,
        );
        assert_eq!(found, Some(default));
    }

    #[test]
    fn the_include_paths_are_the_fallback() {
        let tmp = TempDir::new("fallback");
        let sdk = tmp.file("libs/open.mp.inc");
        let dir = sdk.parent().expect("pai").to_path_buf();
        let found =
            resolve_sdk_file_path(SdkPlatform::Auto, "", std::slice::from_ref(&dir), &tmp.0);
        assert_eq!(found, Some(sdk));
    }

    #[test]
    fn samp_has_no_sdk_file() {
        // No SA-MP as nativas vêm dos includes, sem arquivo de SDK.
        let tmp = TempDir::new("samp");
        tmp.file("qawno/include/open.mp.inc");
        assert_eq!(
            resolve_sdk_file_path(SdkPlatform::Samp, "", &[], &tmp.0),
            None
        );
    }

    #[test]
    fn a_ready_preset_does_not_carry_the_fine_tuning() {
        // Mandar os valores do usuário junto sobrescreveria o que o preset
        // promete do lado da engine.
        let cfg = PawnProConfig::default();
        let opts = build_format_options(&cfg);
        assert_eq!(opts["formatPreset"], "allman");
        assert!(!opts.contains_key("formatBraceStyle"));
        assert!(!opts.contains_key("formatSpaceAroundOperators"));
    }

    #[test]
    fn the_custom_preset_carries_the_fine_tuning() {
        let mut cfg = PawnProConfig::default();
        cfg.format.preset = FormatPreset::Custom;
        let opts = build_format_options(&cfg);
        assert_eq!(opts["formatPreset"], "custom");
        assert!(opts.contains_key("formatBraceStyle"));
        assert!(opts.contains_key("formatSpaceAroundOperators"));
        assert!(opts.contains_key("formatEmptyBlockSameLine"));
    }

    #[test]
    fn array_alignment_is_sent_with_every_preset() {
        // Ortogonal ao preset, por decisão de desenho.
        for preset in [
            FormatPreset::Allman,
            FormatPreset::Knr,
            FormatPreset::Custom,
        ] {
            let mut cfg = PawnProConfig::default();
            cfg.format.preset = preset;
            let opts = build_format_options(&cfg);
            assert!(
                opts.contains_key("formatPreserveArrayAlignment"),
                "{preset:?}"
            );
        }
    }

    #[test]
    fn naming_goes_nested_as_the_engine_expects() {
        let cfg = PawnProConfig::default();
        let opts = build_naming_options(&cfg);
        assert!(opts["naming"].get("minLength").is_some());
        assert!(opts["naming"].get("allowShortInLoops").is_some());
    }

    #[test]
    fn settings_without_an_sdk_send_an_empty_path() {
        // A engine trata vazio como ausência; um `null` exigiria tratamento
        // extra do outro lado.
        let tmp = TempDir::new("nosdk");
        let cfg = PawnProConfig::default();
        let settings = build_engine_settings(&cfg, &tmp.0);
        assert_eq!(settings.sdk_file_path, "");
    }
}
