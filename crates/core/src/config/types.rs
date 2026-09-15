//! Os tipos da configuração do PawnPro.
//!
//! Cada campo tem um padrão, e o `serde` preenche o que faltar: um
//! `config.json` com uma chave só é válido, e o resto vem dos defaults.

use serde::{Deserialize, Serialize};

/// Cor de destaque das páginas da extensão.
///
/// Só a validação mora aqui: o CSS que a cor gera é da extensão, que desenha
/// as páginas. Fechado para um valor livre não virar CSS sem contraste.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentColor {
    /// Segue o tema do editor (o `''` da configuração).
    #[serde(rename = "")]
    #[default]
    Auto,
    Blue,
    Purple,
    Green,
    Amber,
    Pink,
    Teal,
}

/// Esquema de realce de sintaxe.
///
/// Só a validação mora aqui: aplicar o esquema é da extensão, que lê os
/// arquivos de tema que ela mesma distribui.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    /// Segue o tema do editor.
    Auto,
    ClassicWhite,
    ModernWhite,
    ClassicDark,
    ModernDark,
    /// Sem realce próprio. É o padrão: a extensão não mexe nas cores do
    /// editor sem o usuário pedir.
    #[default]
    None,
}

/// Codificação padrão da saída do `pawncc` e do log do servidor.
///
/// As duas escrevem em windows-1252 na maioria das builds.
fn default_encoding() -> String {
    "windows1252".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CompilerConfig {
    pub path: String,
    pub args: Vec<String>,
    pub auto_detect: bool,
}

impl Default for CompilerConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            args: Vec::new(),
            auto_detect: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OutputConfig {
    pub encoding: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            encoding: default_encoding(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BuildConfig {
    pub show_command: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SyntaxConfig {
    pub scheme: Scheme,
    pub apply_on_startup: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiConfig {
    pub show_include_paths: bool,
    /// Anima sutilmente o título "PawnPro" no topo das páginas.
    pub animate_title: bool,
    /// Idioma das páginas da extensão, independente do idioma da engine.
    ///
    /// Fica como texto, e não como `UiLocale`, porque o vazio — "segue o
    /// editor" — é um estado válido que o enum não representa.
    pub locale: String,
    /// Cor de destaque das páginas.
    pub accent: AccentColor,
}

/// Quando o painel acompanha a saída do servidor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowMode {
    /// Só enquanto o painel está à vista.
    #[default]
    Visible,
    Always,
    Off,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerOutputConfig {
    pub follow: FollowMode,
}

/// Qual servidor o projeto usa.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerType {
    /// Descobre pelo conteúdo da pasta.
    #[default]
    Auto,
    Samp,
    Omp,
}

/// Como o painel guarda os comandos enviados.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerHistoryConfig {
    /// `false` desliga o registro: nada é gravado em `.pawnpro/state.json`.
    pub enabled: bool,
    /// Comandos do gamemode que não devem ser guardados, além dos que a
    /// extensão já reconhece. Comparados pelo primeiro termo, sem diferenciar
    /// maiúsculas.
    pub sensitive_commands: Vec<String>,
}

impl Default for ServerHistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sensitive_commands: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerConfig {
    /// `type` no arquivo: é palavra reservada em Rust.
    #[serde(rename = "type")]
    pub server_type: ServerType,
    pub history: ServerHistoryConfig,
    pub path: String,
    pub cwd: String,
    pub args: Vec<String>,
    pub clear_on_start: bool,
    pub log_path: String,
    pub log_encoding: String,
    pub output: ServerOutputConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server_type: ServerType::Auto,
            history: ServerHistoryConfig::default(),
            path: String::new(),
            cwd: "${workspaceFolder}".to_string(),
            args: Vec::new(),
            clear_on_start: true,
            log_path: String::new(),
            log_encoding: default_encoding(),
            output: ServerOutputConfig::default(),
        }
    }
}

/// Plataforma do SDK usada na análise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdkPlatform {
    #[default]
    Auto,
    Omp,
    Samp,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AnalysisSdkConfig {
    pub platform: SdkPlatform,
    pub file_path: String,
}

/// Um dos estilos de caixa embutidos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NameCaseBuiltin {
    #[serde(rename = "camelCase")]
    CamelCase,
    #[serde(rename = "snake_case")]
    SnakeCase,
    #[serde(rename = "PascalCase")]
    PascalCase,
    #[serde(rename = "UPPER_CASE")]
    UpperCase,
    #[serde(rename = "Capitalized_Snake")]
    CapitalizedSnake,
}

/// Critério de nomenclatura de uma categoria.
///
/// Um dos estilos embutidos, ou um regex do usuário no formato `/padrão/`. A
/// engine âncora o padrão como `^(?:…)$` — ele descreve o nome inteiro — e
/// ignora um regex inválido sem invalidar a configuração inteira.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NameCase {
    Builtin(NameCaseBuiltin),
    Custom(String),
}

impl NameCaseBuiltin {
    /// O texto que a engine reconhece.
    ///
    /// É o mesmo do `serde(rename)` logo acima — duas listas que precisam
    /// concordar. `the_builtin_names_match_what_is_stored` guarda isso.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CamelCase => "camelCase",
            Self::SnakeCase => "snake_case",
            Self::PascalCase => "PascalCase",
            Self::UpperCase => "UPPER_CASE",
            Self::CapitalizedSnake => "Capitalized_Snake",
        }
    }
}

impl NameCase {
    /// O estilo como a engine o lê: o nome do embutido, ou a regex do usuário.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Builtin(builtin) => builtin.as_str(),
            Self::Custom(pattern) => pattern,
        }
    }
}

/// Estilos aceitos por categoria.
///
/// Um nome passa se casar com QUALQUER estilo da lista; lista vazia desliga a
/// checagem daquela categoria.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NamingStyleConfig {
    pub functions: Vec<NameCase>,
    pub globals: Vec<NameCase>,
    pub locals: Vec<NameCase>,
    /// Constantes tipadas: `const`, membros de enum.
    pub constants: Vec<NameCase>,
    /// Macros do preprocessador: `#define`.
    pub macros: Vec<NameCase>,
    pub parameters: Vec<NameCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NamingConfig {
    /// Liga o assistente de nomes (PP0018).
    pub enabled: bool,
    /// Comprimento mínimo antes de sinalizar, exceto índices de laço.
    pub min_length: u32,
    /// Nomes de uma letra tolerados em cabeçalho de `for`.
    pub allow_short_in_loops: Vec<String>,
    /// Identificadores genéricos sempre sinalizados.
    pub blocklist: Vec<String>,
    /// Arquivo `.ban` com os nomes proibidos — tem prioridade sobre a lista.
    pub blocklist_file: String,
    /// Arquivo `.allow` com os índices de laço tolerados.
    pub loop_indices_file: String,
    /// Teto de processamento de cada `.ban`/`.allow`, em bytes.
    ///
    /// Acima disto o arquivo não é processado, por segurança — não impede o
    /// desenvolvedor de escrevê-lo.
    pub max_list_file_bytes: u64,
    pub style: NamingStyleConfig,
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_length: 2,
            allow_short_in_loops: ["i", "j", "k"].iter().map(ToString::to_string).collect(),
            blocklist: ["tmp", "temp", "aux", "foo", "bar", "data", "var"]
                .iter()
                .map(ToString::to_string)
                .collect(),
            blocklist_file: "${workspaceFolder}/.pawnpro/naming-blocklist.ban".to_string(),
            loop_indices_file: "${workspaceFolder}/.pawnpro/naming-loop-indices.allow".to_string(),
            max_list_file_bytes: 32 * 1024 * 1024,
            style: NamingStyleConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AnalysisConfig {
    pub warn_unused_in_inc: bool,
    pub suppress_diagnostics_in_inc: bool,
    pub sdk: AnalysisSdkConfig,
    pub naming: NamingConfig,
}

/// Preset de formatação.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatPreset {
    #[default]
    Allman,
    Knr,
    Compact,
    /// Libera os ajustes finos abaixo.
    Custom,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FormatBraceStyle {
    #[default]
    NextLine,
    SameLine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FormatConfig {
    pub preset: FormatPreset,
    /// Só aplicado quando `preset` é `custom`.
    pub brace_style: FormatBraceStyle,
    /// Só aplicado quando `preset` é `custom`.
    pub space_around_operators: bool,
    /// Mantém blocos vazios colados (`if (a) {}`). Só com `preset` `custom`.
    pub empty_block_same_line: bool,
    /// Preserva o alinhamento manual em inicializadores de array multi-linha.
    ///
    /// Ortogonal ao preset — aplicado sempre.
    pub preserve_array_alignment: bool,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            preset: FormatPreset::Allman,
            brace_style: FormatBraceStyle::NextLine,
            space_around_operators: true,
            empty_block_same_line: true,
            preserve_array_alignment: false,
        }
    }
}

/// Toda a configuração do PawnPro.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PawnProConfig {
    pub compiler: CompilerConfig,
    pub include_paths: Vec<String>,
    pub output: OutputConfig,
    pub build: BuildConfig,
    pub syntax: SyntaxConfig,
    pub ui: UiConfig,
    pub server: ServerConfig,
    pub analysis: AnalysisConfig,
    pub format: FormatConfig,
    /// Idioma da engine e do depurador. Vazio segue o editor.
    pub locale: String,
    pub diagnostics: DiagnosticsConfig,
}

/// Registro de diagnóstico em arquivo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DiagnosticsConfig {
    /// `off` (padrão), `error`, `warn` ou `info`.
    ///
    /// Fica como texto, e não como o `enum` do módulo, porque a configuração é
    /// escrita à mão e um valor inválido não pode impedir o resto de carregar.
    pub level: String,
}

impl Default for DiagnosticsConfig {
    /// `off` por extenso, e não o texto vazio do `derive`: é o valor que a
    /// página de configurações mostra e que a extensão sempre usou.
    fn default() -> Self {
        Self {
            level: "off".to_string(),
        }
    }
}

impl Default for PawnProConfig {
    fn default() -> Self {
        Self {
            compiler: CompilerConfig::default(),
            include_paths: vec!["${workspaceFolder}/pawno/include".to_string()],
            output: OutputConfig::default(),
            build: BuildConfig::default(),
            syntax: SyntaxConfig::default(),
            ui: UiConfig::default(),
            server: ServerConfig::default(),
            analysis: AnalysisConfig::default(),
            format: FormatConfig::default(),
            locale: String::new(),
            diagnostics: DiagnosticsConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_extension() {
        // Estes valores são contrato com quem já tem `.pawnpro/config.json`:
        // mudar um aqui muda o comportamento de projetos existentes.
        let c = PawnProConfig::default();
        assert!(c.compiler.auto_detect);
        assert_eq!(c.include_paths, ["${workspaceFolder}/pawno/include"]);
        assert_eq!(c.output.encoding, "windows1252");
        assert_eq!(c.server.cwd, "${workspaceFolder}");
        assert_eq!(c.server.log_encoding, "windows1252");
        assert!(c.server.clear_on_start);
        assert!(c.server.history.enabled);
        assert_eq!(c.server.server_type, ServerType::Auto);
        assert_eq!(c.server.output.follow, FollowMode::Visible);
        assert_eq!(c.syntax.scheme, Scheme::None);
        assert!(!c.syntax.apply_on_startup);
        assert_eq!(c.analysis.naming.min_length, 2);
        assert_eq!(c.analysis.naming.allow_short_in_loops, ["i", "j", "k"]);
        assert_eq!(c.analysis.naming.max_list_file_bytes, 32 * 1024 * 1024);
        assert_eq!(c.format.preset, FormatPreset::Allman);
        assert!(c.format.space_around_operators);
        assert!(c.format.empty_block_same_line);
        assert!(!c.format.preserve_array_alignment);
        assert_eq!(c.locale, "");
        assert_eq!(c.diagnostics.level, "off");
    }

    #[test]
    fn empty_json_yields_the_defaults() {
        // Um `config.json` vazio é válido: o resto vem do `serde(default)`.
        let c: PawnProConfig = serde_json::from_str("{}").expect("parse");
        assert_eq!(c, PawnProConfig::default());
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        // O caso que o `deepMerge` existia para resolver.
        let json = r#"{"compiler":{"path":"/usr/bin/pawncc"}}"#;
        let c: PawnProConfig = serde_json::from_str(json).expect("parse");
        assert_eq!(c.compiler.path, "/usr/bin/pawncc");
        // O resto do bloco não some.
        assert!(c.compiler.auto_detect);
        assert_eq!(c.output.encoding, "windows1252");
    }

    #[test]
    fn json_keys_stay_camel_case() {
        // O arquivo é do usuário: renomear uma chave quebraria projetos.
        let json = serde_json::to_value(PawnProConfig::default()).expect("ser");
        assert!(json.get("includePaths").is_some());
        assert!(json["compiler"].get("autoDetect").is_some());
        assert!(json["server"].get("clearOnStart").is_some());
        assert!(json["server"].get("logEncoding").is_some());
        assert!(json["analysis"].get("warnUnusedInInc").is_some());
        assert!(json["analysis"]["naming"].get("maxListFileBytes").is_some());
        assert!(json["format"].get("braceStyle").is_some());
        assert!(json["ui"].get("showIncludePaths").is_some());
    }

    #[test]
    fn server_type_keeps_its_reserved_name() {
        // `type` é palavra reservada em Rust, mas é o nome no arquivo.
        let json = serde_json::to_value(ServerConfig::default()).expect("ser");
        assert_eq!(json["type"], "auto");
        assert!(json.get("serverType").is_none());
    }

    #[test]
    fn accent_auto_serializes_as_empty() {
        let json = serde_json::to_value(UiConfig::default()).expect("ser");
        assert_eq!(json["accent"], "");
    }

    #[test]
    fn name_case_accepts_builtin_and_custom() {
        // A engine aceita um regex `/padrão/` além dos estilos embutidos.
        let json = r#"["camelCase","/^PP_/","PascalCase"]"#;
        let cases: Vec<NameCase> = serde_json::from_str(json).expect("parse");
        assert_eq!(cases[0], NameCase::Builtin(NameCaseBuiltin::CamelCase));
        assert_eq!(cases[1], NameCase::Custom("/^PP_/".to_string()));
        assert_eq!(cases[2], NameCase::Builtin(NameCaseBuiltin::PascalCase));
    }

    #[test]
    fn name_case_round_trips() {
        let cases = vec![
            NameCase::Builtin(NameCaseBuiltin::SnakeCase),
            NameCase::Custom("/x/".into()),
        ];
        let json = serde_json::to_string(&cases).expect("ser");
        assert_eq!(json, r#"["snake_case","/x/"]"#);
    }

    #[test]
    fn enum_values_match_the_configuration_strings() {
        assert_eq!(
            serde_json::to_value(FollowMode::Visible).unwrap(),
            "visible"
        );
        assert_eq!(serde_json::to_value(ServerType::Omp).unwrap(), "omp");
        assert_eq!(serde_json::to_value(SdkPlatform::None).unwrap(), "none");
        assert_eq!(serde_json::to_value(FormatPreset::Knr).unwrap(), "knr");
        assert_eq!(
            serde_json::to_value(FormatBraceStyle::SameLine).unwrap(),
            "sameLine"
        );
        assert_eq!(
            serde_json::to_value(Scheme::ClassicDark).unwrap(),
            "classic_dark"
        );
    }

    #[test]
    fn an_unknown_key_does_not_break_the_file() {
        // Configuração de uma versão mais nova, ou um erro de digitação: o
        // resto precisa continuar valendo.
        let json = r#"{"compiler":{"path":"/x"},"chaveDesconhecida":123}"#;
        let c: PawnProConfig = serde_json::from_str(json).expect("parse");
        assert_eq!(c.compiler.path, "/x");
    }
}

/// Testes contra a configuração real do usuário.
///
/// Rodam só quando `PAWNPRO_CONFIG_FILE` aponta para um `config.json` de
/// verdade. Não é obrigatório para compilar; quando existe, confirma que um
/// arquivo escrito por uma versão anterior continua sendo lido.
#[cfg(test)]
mod real_files {
    use super::*;

    #[test]
    fn a_real_config_file_parses() {
        let Ok(path) = std::env::var("PAWNPRO_CONFIG_FILE") else {
            return;
        };
        let raw = std::fs::read_to_string(&path).expect("ler config");
        let parsed: Result<PawnProConfig, _> = serde_json::from_str(&raw);
        let cfg = parsed.unwrap_or_else(|e| panic!("{path} não parseou: {e}"));
        // Uma chave que o arquivo define precisa sobreviver ao merge com os
        // defaults; as que ele não define vêm do padrão.
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        if value.get("ui").and_then(|u| u.get("showIncludePaths")) == Some(&true.into()) {
            assert!(cfg.ui.show_include_paths, "perdeu ui.showIncludePaths");
        }
        assert_eq!(cfg.output.encoding, "windows1252", "perdeu o default");
    }

    #[test]
    fn the_builtin_names_match_what_is_stored() {
        // O `as_str` alimenta a engine e o `serde(rename)` alimenta o arquivo:
        // divergirem faria a engine checar um estilo diferente do configurado.
        for builtin in [
            NameCaseBuiltin::CamelCase,
            NameCaseBuiltin::SnakeCase,
            NameCaseBuiltin::PascalCase,
            NameCaseBuiltin::UpperCase,
            NameCaseBuiltin::CapitalizedSnake,
        ] {
            let stored = serde_json::to_value(builtin).expect("serializar");
            assert_eq!(stored, builtin.as_str(), "{builtin:?}");
        }
    }
}
