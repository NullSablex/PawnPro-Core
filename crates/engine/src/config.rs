//! O que a engine sabe da configuração.
//!
//! Nada aqui lê arquivo: quem lê `config.json` e as listas `.ban`/`.allow` é o
//! core, que entrega tudo resolvido. Ter dois leitores da mesma configuração
//! no mesmo binário deixava os dois livres para discordar.

#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    pub analysis: AnalysisConfig,
}

#[derive(Debug, Clone, Default)]
pub struct AnalysisConfig {
    pub warn_unused_in_inc: bool,
    pub suppress_diagnostics_in_inc: bool,
    pub naming: NamingConfig,
}

/// Configuração do assistente de nomes (PP0018). Conservadora por padrão:
/// desligada, e mesmo ligada só sinaliza nomes claramente pobres.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamingConfig {
    /// Liga o diagnóstico de nomes. Padrão `false` — quem não pediu não é incomodado.
    pub enabled: bool,
    /// Comprimento mínimo de identificador antes de sinalizar (exceto loops).
    pub min_length: u32,
    /// Nomes de 1 letra tolerados em cabeçalho de `for` (índices clássicos).
    /// Já resolvidos pelo core: do arquivo `.allow`, se houver, ou da lista
    /// escrita na configuração.
    pub allow_short_in_loops: Vec<String>,
    /// Identificadores genéricos sempre sinalizados (placeholders). Já
    /// resolvidos pelo core, do arquivo `.ban` ou da configuração.
    pub blocklist: Vec<String>,
    /// Estilo de caixa esperado por categoria. Vazio (`"off"`) = não checa.
    pub style: StyleConfig,
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_length: 2,
            allow_short_in_loops: ["i", "j", "k"].iter().map(|s| (*s).to_string()).collect(),
            blocklist: ["tmp", "temp", "aux", "foo", "bar", "data", "var"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            style: StyleConfig::default(),
        }
    }
}

/// Estilos de caixa aceitos por categoria de identificador.
///
/// Cada campo é uma lista de `"camelCase" | "snake_case" | "PascalCase" | "UPPER_CASE"`; lista
/// vazia = sem checagem. Um nome é aceito se casar com QUALQUER estilo da lista.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StyleConfig {
    pub functions: Vec<String>,
    pub globals: Vec<String>,
    pub locals: Vec<String>,
    pub constants: Vec<String>,
    pub macros: Vec<String>,
    pub parameters: Vec<String>,
}
