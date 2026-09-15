//! O que o core entrega à engine: includes, SDK, formatação e nomenclatura.
//!
//! Tudo já resolvido — arquivos de lista lidos, `${workspaceFolder}` expandido,
//! SDK localizado. A engine não lê configuração do disco; se algo não vier
//! daqui, ela fica no padrão dela.
//!
//! O tipo entregue é o da própria engine, não um objeto JSON com chaves
//! acordadas à mão: as duas compilam juntas, e quem garante o acordo é o
//! compilador.

use std::path::{Path, PathBuf};

use pawnpro_engine::{FormatStyle, Locale, Preset, Settings};

use crate::config::naming_lists::read_list_file;
use crate::config::types::{
    FormatBraceStyle, FormatPreset, NamingConfig, PawnProConfig, SdkPlatform,
};

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

/// Monta o que a engine precisa a partir da configuração do projeto.
///
/// `editor_language` é o idioma do editor, que só a extensão conhece; vale
/// quando `locale` está vazio na configuração — "seguir o editor".
#[must_use]
pub fn build_settings(
    cfg: &PawnProConfig,
    workspace_root: &Path,
    editor_language: &str,
) -> Settings {
    let include_paths = crate::project::includes::include_paths_for(cfg, workspace_root, None);
    let sdk_file = resolve_sdk_file_path(
        cfg.analysis.sdk.platform,
        &cfg.analysis.sdk.file_path,
        &include_paths,
        workspace_root,
    );
    let locale = if cfg.locale.is_empty() {
        editor_language
    } else {
        &cfg.locale
    };

    Settings {
        include_paths: Some(include_paths),
        warn_unused_in_inc: Some(cfg.analysis.warn_unused_in_inc),
        suppress_diagnostics_in_inc: Some(cfg.analysis.suppress_diagnostics_in_inc),
        sdk_file: Some(sdk_file),
        locale: Some(Locale::from_tag(locale)),
        format_style: Some(build_format_style(cfg)),
        naming: Some(build_naming(&cfg.analysis.naming)),
    }
}

/// O estilo de formatação a partir do preset e dos ajustes finos.
///
/// Os ajustes só acompanham o preset `custom`: os prontos definem os próprios
/// valores, e aplicar os do usuário por cima desfaria o que o preset promete.
/// `preserve_array_alignment` é a exceção — vale para todos, por desenho.
fn build_format_style(cfg: &PawnProConfig) -> FormatStyle {
    let fmt = &cfg.format;
    let preset = match fmt.preset {
        FormatPreset::Allman => Preset::Allman,
        FormatPreset::Knr => Preset::Knr,
        FormatPreset::Compact => Preset::Compact,
        FormatPreset::Custom => Preset::Custom,
    };
    let mut style = FormatStyle::from_preset(preset);

    if fmt.preset == FormatPreset::Custom {
        style.brace = match fmt.brace_style {
            FormatBraceStyle::NextLine => pawnpro_engine::BracePlacement::NextLine,
            FormatBraceStyle::SameLine => pawnpro_engine::BracePlacement::SameLine,
        };
        style.space_around_operators = fmt.space_around_operators;
        style.empty_block_same_line = fmt.empty_block_same_line;
    }
    style.preserve_array_alignment = fmt.preserve_array_alignment;
    style
}

/// A configuração de nomes com as listas já resolvidas.
///
/// O arquivo vence a lista escrita na configuração quando existe e tem algo
/// dentro: é ele que o desenvolvedor edita no dia a dia.
fn build_naming(cfg: &NamingConfig) -> pawnpro_engine::NamingConfig {
    let from_file = |path: &str| -> Option<Vec<String>> {
        if path.is_empty() {
            return None;
        }
        let terms = read_list_file(Path::new(path), cfg.max_list_file_bytes);
        (!terms.is_empty()).then_some(terms)
    };

    pawnpro_engine::NamingConfig {
        enabled: cfg.enabled,
        min_length: cfg.min_length,
        allow_short_in_loops: from_file(&cfg.loop_indices_file)
            .unwrap_or_else(|| cfg.allow_short_in_loops.clone()),
        blocklist: from_file(&cfg.blocklist_file).unwrap_or_else(|| cfg.blocklist.clone()),
        style: pawnpro_engine::StyleConfig {
            functions: styles(&cfg.style.functions),
            globals: styles(&cfg.style.globals),
            locals: styles(&cfg.style.locals),
            constants: styles(&cfg.style.constants),
            macros: styles(&cfg.style.macros),
            parameters: styles(&cfg.style.parameters),
        },
    }
}

/// Os estilos como a engine os lê: o nome do embutido, ou a regex do usuário.
fn styles(cases: &[crate::config::types::NameCase]) -> Vec<String> {
    cases.iter().map(|c| c.as_str().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{FormatConfig, NameCase, NameCaseBuiltin};

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
        // Aplicar os ajustes do usuário por cima desfaria o que o preset
        // promete do lado da engine.
        let cfg = PawnProConfig {
            format: FormatConfig {
                preset: FormatPreset::Knr,
                brace_style: FormatBraceStyle::NextLine,
                space_around_operators: false,
                ..FormatConfig::default()
            },
            ..PawnProConfig::default()
        };
        let style = build_format_style(&cfg);
        // O K&R manda chave na mesma linha; o `brace_style` do usuário diz o
        // contrário e é ignorado de propósito.
        assert_eq!(style.brace, pawnpro_engine::BracePlacement::SameLine);
        assert!(style.space_around_operators);
    }

    #[test]
    fn the_custom_preset_carries_the_fine_tuning() {
        let cfg = PawnProConfig {
            format: FormatConfig {
                preset: FormatPreset::Custom,
                brace_style: FormatBraceStyle::SameLine,
                space_around_operators: false,
                empty_block_same_line: true,
                ..FormatConfig::default()
            },
            ..PawnProConfig::default()
        };
        let style = build_format_style(&cfg);
        assert_eq!(style.brace, pawnpro_engine::BracePlacement::SameLine);
        assert!(!style.space_around_operators);
        assert!(style.empty_block_same_line);
    }

    #[test]
    fn array_alignment_is_sent_with_every_preset() {
        // Ortogonal ao preset, por decisão de desenho.
        for preset in [
            FormatPreset::Allman,
            FormatPreset::Knr,
            FormatPreset::Custom,
        ] {
            let cfg = PawnProConfig {
                format: FormatConfig {
                    preset,
                    preserve_array_alignment: true,
                    ..FormatConfig::default()
                },
                ..PawnProConfig::default()
            };
            assert!(
                build_format_style(&cfg).preserve_array_alignment,
                "{preset:?}"
            );
        }
    }

    #[test]
    fn the_list_file_beats_what_is_written_in_the_config() {
        // O arquivo é o que o desenvolvedor edita no dia a dia. Era a engine
        // quem o lia; agora é o core, e a engine recebe a lista pronta.
        let tmp = TempDir::new("blocklist");
        let ban = tmp.0.join("nomes.ban");
        std::fs::write(&ban, "# comentário\nproibido\n\noutro\n").expect("escrever");

        let naming = NamingConfig {
            blocklist: vec!["inline".to_string()],
            blocklist_file: ban.to_string_lossy().into_owned(),
            ..NamingConfig::default()
        };
        assert_eq!(build_naming(&naming).blocklist, ["proibido", "outro"]);
    }

    #[test]
    fn an_unreadable_list_file_falls_back_to_the_config() {
        // Apontar para um arquivo que não existe não pode zerar a lista: a
        // análise passaria a não sinalizar nada, sem dizer por quê.
        let naming = NamingConfig {
            blocklist: vec!["inline".to_string()],
            blocklist_file: "/nao/existe.ban".to_string(),
            ..NamingConfig::default()
        };
        assert_eq!(build_naming(&naming).blocklist, ["inline"]);
    }

    #[test]
    fn an_empty_list_file_falls_back_to_the_config() {
        // Arquivo criado e ainda vazio é o estado normal logo depois de semear.
        let tmp = TempDir::new("empty-list");
        let allow = tmp.0.join("indices.allow");
        std::fs::write(&allow, "# só o cabeçalho\n").expect("escrever");

        let naming = NamingConfig {
            allow_short_in_loops: vec!["i".to_string()],
            loop_indices_file: allow.to_string_lossy().into_owned(),
            ..NamingConfig::default()
        };
        assert_eq!(build_naming(&naming).allow_short_in_loops, ["i"]);
    }

    #[test]
    fn the_naming_style_travels_as_the_engine_reads_it() {
        let naming = NamingConfig {
            style: crate::config::types::NamingStyleConfig {
                functions: vec![
                    NameCase::Builtin(NameCaseBuiltin::CamelCase),
                    NameCase::Custom("/^g_.+$/".to_string()),
                ],
                ..crate::config::types::NamingStyleConfig::default()
            },
            ..NamingConfig::default()
        };
        assert_eq!(
            build_naming(&naming).style.functions,
            ["camelCase", "/^g_.+$/"]
        );
    }

    #[test]
    fn settings_without_an_sdk_send_no_path() {
        let tmp = TempDir::new("nosdk");
        let cfg = PawnProConfig::default();
        let settings = build_settings(&cfg, &tmp.0, "pt-br");
        assert_eq!(settings.sdk_file, Some(None));
    }

    #[test]
    fn the_settings_carry_every_group() {
        // Um campo em `None` é "não mexa nisso": a engine ficaria no padrão
        // dela, discordando da configuração do projeto em silêncio.
        let tmp = TempDir::new("groups");
        let settings = build_settings(&PawnProConfig::default(), &tmp.0, "pt-br");
        assert!(settings.include_paths.is_some());
        assert!(settings.warn_unused_in_inc.is_some());
        assert!(settings.suppress_diagnostics_in_inc.is_some());
        assert!(settings.sdk_file.is_some());
        assert!(settings.locale.is_some());
        assert!(settings.format_style.is_some());
        assert!(settings.naming.is_some());
    }

    #[test]
    fn an_empty_locale_follows_the_editor() {
        // Vazio na configuração significa "seguir o editor".
        let tmp = TempDir::new("locale-editor");
        let cfg = PawnProConfig::default();
        assert_eq!(cfg.locale, "");
        let settings = build_settings(&cfg, &tmp.0, "en-us");
        assert_eq!(settings.locale, Some(Locale::from_tag("en-us")));
    }

    #[test]
    fn a_configured_locale_beats_the_editor() {
        let tmp = TempDir::new("locale-config");
        let cfg = PawnProConfig {
            locale: "pt-br".to_string(),
            ..PawnProConfig::default()
        };
        let settings = build_settings(&cfg, &tmp.0, "en-us");
        assert_eq!(settings.locale, Some(Locale::from_tag("pt-br")));
    }
}
