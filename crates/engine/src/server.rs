use std::path::PathBuf;
use std::sync::Arc;

use futures::future::join_all;
use tokio::sync::{RwLock, watch};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionResponse, CodeLens, CodeLensOptions, CodeLensParams,
    CompletionItem, CompletionList, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DiagnosticTag, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, DocumentFormattingParams, DocumentRangeFormattingParams,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, InitializedParams, Location, MessageType, NumberOrString,
    OneOf, Position, PrepareRenameResponse, Range, ReferenceParams, RenameOptions, RenameParams,
    SaveOptions, SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    SignatureHelp, SignatureHelpOptions, SignatureHelpParams, TextDocumentPositionParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, TextEdit, Url, WorkspaceEdit, WorkspaceServerCapabilities,
};
use tower_lsp::{Client, LanguageServer};

use crate::analyzer::diagnostic::Severity;
use crate::intellisense;
use crate::messages::{Locale, MsgKey, msg};
use crate::util::to_u32;
use crate::workspace::{WorkspaceState, uri_to_path};

pub struct PawnProServer {
    client: Client,
    state: Arc<RwLock<WorkspaceState>>,
    /// O que o core entrega. A engine nunca lê configuração do disco: se nada
    /// chegar por aqui, ela fica no padrão dela.
    settings: watch::Receiver<Settings>,
}

impl PawnProServer {
    pub fn new(client: Client, settings: watch::Receiver<Settings>) -> Self {
        let server = Self {
            client,
            state: Arc::new(RwLock::new(WorkspaceState::new())),
            settings,
        };
        server.follow_the_core();
        server
    }

    /// Acompanha o canal: toda entrega do core é aplicada, e o que muda
    /// diagnóstico é republicado sem o editor pedir.
    fn follow_the_core(&self) {
        let mut settings = self.settings.clone();
        let state = Arc::clone(&self.state);
        let client = self.client.clone();
        tokio::spawn(async move {
            while settings.changed().await.is_ok() {
                let update = settings.borrow_and_update().clone();
                let changed = {
                    let mut state = state.write().await;
                    update.apply_change(&mut state)
                };
                crate::log(
                    "info",
                    if changed {
                        "configuração nova aplicada; republicando diagnósticos"
                    } else {
                        "configuração nova sem efeito: igual à que já valia"
                    },
                );
                if changed {
                    republish_all(&client, &state).await;
                }
            }
        });
    }

    async fn publish_diagnostics_for(&self, uri: Url) {
        publish_diagnostics_for(&self.client, &self.state, uri).await;
    }
}

/// Republica os diagnósticos de tudo que está aberto.
///
/// Livre, e não método: a tarefa que acompanha o core tem o cliente e o
/// estado, mas não o servidor.
async fn republish_all(client: &Client, state: &Arc<RwLock<WorkspaceState>>) {
    let uris: Vec<Url> = {
        state
            .read()
            .await
            .open_docs
            .iter()
            .filter_map(|e| Url::parse(e.key()).ok())
            .collect()
    };
    join_all(
        uris.into_iter()
            .map(|uri| publish_diagnostics_for(client, state, uri)),
    )
    .await;
}

/// Analisa e publica os diagnósticos de um documento.
async fn publish_diagnostics_for(client: &Client, state: &Arc<RwLock<WorkspaceState>>, uri: Url) {
    let owned = Arc::clone(state);
    let uri_str = uri.to_string();

    let Ok((version, raw_diags)) =
        tokio::task::spawn_blocking(move || owned.blocking_read().analyze_versioned(&uri_str))
            .await
    else {
        return;
    };
    // Uma edição chegou enquanto esta análise corria: a análise dela publica.
    // Publicar esta mostraria, por um instante, avisos de um texto que já não
    // existe.
    if version.is_some() && state.read().await.current_version(uri.as_str()) != version {
        return;
    }

    let diagnostics = raw_diags.into_iter().map(lsp_diagnostic_from).collect();
    client.publish_diagnostics(uri, diagnostics, version).await;
}

#[tower_lsp::async_trait]
impl LanguageServer for PawnProServer {
    #[allow(
        clippy::significant_drop_tightening,
        reason = "as duas escritas precisam do estado; a trava sai logo depois delas"
    )]
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let root = resolve_workspace_root(&params);
        // A configuração vem do core, não do cliente: `initializationOptions` é
        // ignorado de propósito, para não haver duas fontes.
        let config = self.settings.borrow().clone();

        let raiz = root
            .as_ref()
            .map_or_else(|| "(nenhuma)".to_string(), |r| r.display().to_string());
        {
            let mut state = self.state.write().await;
            if let Some(root) = root {
                state.set_workspace_root(root);
            }
            config.apply_init(&mut state);
        }

        crate::log("info", &format!("sessão iniciada; raiz={raiz}"));

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "pawnpro-engine".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: server_capabilities(),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "PawnPro engine initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.state.read().await.open_document(
            uri.to_string(),
            params.text_document.text,
            params.text_document.version,
        );
        self.publish_diagnostics_for(uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        if let Some(change) = params.content_changes.into_iter().last() {
            self.state.read().await.change_document(
                uri.as_str(),
                change.text,
                params.text_document.version,
            );
        }

        let dependents = self.state.read().await.open_dependents(uri.as_str());
        let mut targets: Vec<Url> = dependents
            .into_iter()
            .filter_map(|u| Url::parse(&u).ok())
            .collect();
        if targets.is_empty() {
            targets.push(uri);
        }
        join_all(targets.into_iter().map(|u| self.publish_diagnostics_for(u))).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let dependents = self.state.read().await.open_dependents(uri.as_str());
        let mut targets: Vec<Url> = dependents
            .into_iter()
            .filter_map(|u| Url::parse(&u).ok())
            .collect();
        if targets.is_empty() {
            targets.push(uri);
        }
        join_all(targets.into_iter().map(|u| self.publish_diagnostics_for(u))).await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let changed_paths: Vec<Url> = params.changes.into_iter().map(|c| c.uri).collect();

        let mut to_republish: std::collections::HashSet<String> = std::collections::HashSet::new();

        for uri in &changed_paths {
            let dependents = {
                let state = self.state.read().await;
                if let Some(path) = uri_to_path(uri.as_str()) {
                    state.evict_path_from_cache(&path);
                }
                state.open_dependents(uri.as_str())
            };
            if dependents.is_empty() {
                to_republish.insert(uri.to_string());
            } else {
                to_republish.extend(dependents);
            }
        }

        let targets = to_republish
            .into_iter()
            .filter_map(|u| Url::parse(&u).ok())
            .map(|u| self.publish_diagnostics_for(u));
        join_all(targets).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        self.state.read().await.close_document(uri.as_str());
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let trigger = params
            .context
            .as_ref()
            .and_then(|c| c.trigger_character.as_deref())
            .unwrap_or("");

        if trigger == "@" {
            return Ok(Some(CompletionResponse::Array(
                self.at_completions(&params).await,
            )));
        }

        let uri_str = params.text_document_position.text_document.uri.to_string();
        let position = params.text_document_position.position;
        let state = Arc::clone(&self.state);

        let mut items = tokio::task::spawn_blocking(move || {
            intellisense::get_completions(&state.blocking_read(), &uri_str, position)
        })
        .await
        .unwrap_or_default();

        if items.is_empty() {
            return Ok(None);
        }
        // Já vêm ordenados por proximidade do cursor, então o corte descarta os
        // menos relevantes; `is_incomplete` faz o editor pedir de novo enquanto
        // o prefixo cresce.
        let is_incomplete = items.len() > intellisense::MAX_COMPLETION_ITEMS;
        items.truncate(intellisense::MAX_COMPLETION_ITEMS);
        Ok(Some(CompletionResponse::List(CompletionList {
            is_incomplete,
            items,
        })))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri_str = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;
        let state = Arc::clone(&self.state);

        let location = tokio::task::spawn_blocking(move || {
            intellisense::get_definition(&state.blocking_read(), &uri_str, position)
        })
        .await
        .unwrap_or(None);

        Ok(location.map(GotoDefinitionResponse::Scalar))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri_str = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;
        let state = Arc::clone(&self.state);

        let result = tokio::task::spawn_blocking(move || {
            intellisense::get_hover(&state.blocking_read(), &uri_str, position)
        })
        .await
        .unwrap_or(None);

        Ok(result)
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri_str = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;
        let state = Arc::clone(&self.state);

        let result = tokio::task::spawn_blocking(move || {
            intellisense::get_signature_help(&state.blocking_read(), &uri_str, position)
        })
        .await
        .unwrap_or(None);

        Ok(result)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri_str = params.text_document_position.text_document.uri.to_string();
        let position = params.text_document_position.position;
        let state = Arc::clone(&self.state);

        let locations = tokio::task::spawn_blocking(move || {
            intellisense::get_references(&state.blocking_read(), &uri_str, position)
        })
        .await
        .unwrap_or_default();

        Ok((!locations.is_empty()).then_some(locations))
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri_str = params.text_document.uri.to_string();
        let state = Arc::clone(&self.state);

        let lenses = tokio::task::spawn_blocking(move || {
            intellisense::get_code_lens(&state.blocking_read(), &uri_str)
        })
        .await
        .unwrap_or_default();

        Ok((!lenses.is_empty()).then_some(lenses))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri_str = params.text_document.uri.to_string();
        let state = Arc::clone(&self.state);

        let result = tokio::task::spawn_blocking(move || {
            intellisense::get_semantic_tokens(&state.blocking_read(), &uri_str)
        })
        .await
        .unwrap_or(None);

        Ok(result.map(SemanticTokensResult::Tokens))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri_str = params.text_document.uri.to_string();
        let lsp = params.options;
        let state = Arc::clone(&self.state);

        let edits = tokio::task::spawn_blocking(move || {
            // A trava só enquanto lê: formatar é o trabalho pesado, e segurá-la
            // nesse tempo atrasaria as edições que chegam.
            let (text, style) = {
                let guard = state.blocking_read();
                (guard.get_text(&uri_str)?, style_from(&guard, &lsp))
            };
            Some(intellisense::format_document(&text, style))
        })
        .await
        .unwrap_or_default()
        .unwrap_or_default();

        Ok((!edits.is_empty()).then_some(edits))
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri_str = params.text_document.uri.to_string();
        let lsp = params.options;
        let range = params.range;
        let state = Arc::clone(&self.state);

        let edits = tokio::task::spawn_blocking(move || {
            // A trava só enquanto lê: formatar é o trabalho pesado, e segurá-la
            // nesse tempo atrasaria as edições que chegam.
            let (text, style) = {
                let guard = state.blocking_read();
                (guard.get_text(&uri_str)?, style_from(&guard, &lsp))
            };
            Some(intellisense::format_range(&text, range, style))
        })
        .await
        .unwrap_or_default()
        .unwrap_or_default();

        Ok((!edits.is_empty()).then_some(edits))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri_str = params.text_document.uri.to_string();
        let position = params.position;
        let state = Arc::clone(&self.state);

        let range = tokio::task::spawn_blocking(move || {
            intellisense::prepare_rename(&state.blocking_read(), &uri_str, position)
        })
        .await
        .unwrap_or_default();

        Ok(range.map(PrepareRenameResponse::Range))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri_str = params.text_document_position.text_document.uri.to_string();
        let position = params.text_document_position.position;
        let new_name = params.new_name;
        let state = Arc::clone(&self.state);

        let result = tokio::task::spawn_blocking(move || {
            intellisense::get_rename(&state.blocking_read(), &uri_str, position, &new_name)
        })
        .await;

        // A recusa volta como erro: o editor mostra o motivo em vez de aplicar
        // um rename que alteraria outra variável.
        match result {
            Ok(Ok(edit)) => Ok(edit),
            Ok(Err(reason)) => Err(tower_lsp::jsonrpc::Error::invalid_params(reason)),
            Err(_) => Ok(None),
        }
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri_str = params.text_document.uri.to_string();
        let state = Arc::clone(&self.state);

        let actions = tokio::task::spawn_blocking(move || {
            code_actions_for(&state.blocking_read(), &uri_str, &params)
        })
        .await
        .unwrap_or_default();

        Ok((!actions.is_empty()).then_some(actions))
    }
}

impl PawnProServer {
    async fn at_completions(&self, params: &CompletionParams) -> Vec<CompletionItem> {
        let uri_str = params.text_document_position.text_document.uri.to_string();
        let pos = params.text_document_position.position;
        let state = Arc::clone(&self.state);

        tokio::task::spawn_blocking(move || {
            let state = state.blocking_read();
            let at_col = pos.character.saturating_sub(1);
            let in_comment = state.open_docs.get(&uri_str).is_some_and(|doc| {
                let line = doc.text.lines().nth(pos.line as usize).unwrap_or("");
                let col_bytes = (at_col as usize).min(line.len());
                let before = &line[..col_bytes];
                before.contains("//") || before.contains("/*") || line.trim_start().starts_with('*')
            });
            intellisense::get_at_completions(in_comment, state.locale)
        })
        .await
        .unwrap_or_default()
    }
}

// --- Configuration update ---

/// O que o core entrega à engine.
///
/// Cada campo é `Option`: `None` é "não mexa nisso". Antes isto vinha em JSON,
/// com as chaves acordadas à mão dos dois lados — um nome trocado passava
/// despercebido e a engine analisava com o padrão dela. Sendo os dois o mesmo
/// binário, quem garante o acordo agora é o compilador.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Raízes de include já resolvidas. Vazio limpa a substituição.
    pub include_paths: Option<Vec<PathBuf>>,
    pub warn_unused_in_inc: Option<bool>,
    pub suppress_diagnostics_in_inc: Option<bool>,
    /// Externo = o campo veio na atualização; interno = definir ou limpar.
    #[allow(clippy::option_option)]
    pub sdk_file: Option<Option<PathBuf>>,
    pub locale: Option<Locale>,
    pub format_style: Option<intellisense::FormatStyle>,
    pub naming: Option<crate::config::NamingConfig>,
}

impl Settings {
    /// Aplica no `initialize`, sem republicar: ainda não há documento aberto.
    fn apply_init(self, state: &mut WorkspaceState) {
        if let Some(paths) = self.include_paths {
            state.include_paths = paths;
            state.invalidate_tabsize_cache();
        }
        if let Some(warn) = self.warn_unused_in_inc {
            state.config.analysis.warn_unused_in_inc = warn;
        }
        if let Some(suppress) = self.suppress_diagnostics_in_inc {
            state.config.analysis.suppress_diagnostics_in_inc = suppress;
        }
        if let Some(sdk) = self.sdk_file {
            state.set_sdk_file_opt(sdk);
        }
        if let Some(locale) = self.locale {
            state.locale = locale;
        }
        if let Some(style) = self.format_style {
            state.format_style = style;
        }
        if let Some(naming) = self.naming {
            state.config.analysis.naming = naming;
        }
    }

    /// `true` se algum campo mudou de fato — é o que decide republicar.
    #[allow(
        clippy::useless_let_if_seq,
        reason = "cada campo liga o mesmo `changed`; o primeiro como expressão quebraria a simetria"
    )]
    fn apply_change(self, state: &mut WorkspaceState) -> bool {
        let mut changed = false;

        if let Some(paths) = self.include_paths
            && state.include_paths != paths
        {
            state.include_paths = paths;
            state.invalidate_tabsize_cache();
            changed = true;
        }
        if let Some(warn) = self.warn_unused_in_inc
            && state.config.analysis.warn_unused_in_inc != warn
        {
            state.config.analysis.warn_unused_in_inc = warn;
            changed = true;
        }
        if let Some(suppress) = self.suppress_diagnostics_in_inc
            && state.config.analysis.suppress_diagnostics_in_inc != suppress
        {
            state.config.analysis.suppress_diagnostics_in_inc = suppress;
            changed = true;
        }
        if let Some(sdk_path) = self.sdk_file
            && state.sdk_file.as_deref() != sdk_path.as_deref()
        {
            state.set_sdk_file_opt(sdk_path);
            changed = true;
        }
        if let Some(locale) = self.locale
            && state.locale != locale
        {
            state.locale = locale;
            changed = true;
        }
        // Estilo de formatação não afeta diagnósticos — atualiza sem republicar.
        if let Some(style) = self.format_style {
            state.format_style = style;
        }
        // Naming afeta os diagnósticos PP0018 — republica se mudou.
        if let Some(naming) = self.naming
            && state.config.analysis.naming != naming
        {
            state.config.analysis.naming = naming;
            changed = true;
        }

        changed
    }
}

// --- Helpers ---

/// Combina o estilo configurado no workspace (preset/chaves) com a indentação
/// que o editor envia por chamada (`tab_size`/`insert_spaces` em `FormattingOptions`).
const fn style_from(
    state: &WorkspaceState,
    lsp: &tower_lsp::lsp_types::FormattingOptions,
) -> intellisense::FormatStyle {
    let mut style = state.format_style;
    style.tab_size = lsp.tab_size;
    style.insert_spaces = lsp.insert_spaces;
    style
}

fn resolve_workspace_root(params: &InitializeParams) -> Option<PathBuf> {
    params
        .workspace_folders
        .as_deref()
        .and_then(|f| f.first())
        .and_then(|f| uri_to_path(f.uri.as_str()))
        .or_else(|| {
            #[allow(deprecated)]
            params
                .root_uri
                .as_ref()
                .and_then(|u| uri_to_path(u.as_str()))
        })
        .or_else(|| {
            #[allow(deprecated)]
            params.root_path.as_deref().map(PathBuf::from)
        })
}

fn lsp_diagnostic_from(d: crate::analyzer::PawnDiagnostic) -> Diagnostic {
    let range = Range {
        start: Position {
            line: d.line,
            character: d.col_start,
        },
        end: Position {
            line: d.line,
            character: d.col_end,
        },
    };
    let severity = match d.severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Hint => DiagnosticSeverity::HINT,
    };
    let tags: Vec<DiagnosticTag> = [
        d.unnecessary.then_some(DiagnosticTag::UNNECESSARY),
        d.deprecated.then_some(DiagnosticTag::DEPRECATED),
    ]
    .into_iter()
    .flatten()
    .collect();

    Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(d.code.to_string())),
        source: Some("pawnpro".to_string()),
        message: d.message,
        tags: (!tags.is_empty()).then_some(tags),
        ..Default::default()
    }
}

/// Code actions de renomeação do assistente de nomes: para o identificador sob a
/// seleção, oferece converter para os estilos configurados em `naming.style`.
/// Cada ação carrega o `WorkspaceEdit` que renomeia todas as ocorrências.
fn code_actions_for(
    state: &crate::workspace::WorkspaceState,
    uri: &str,
    params: &CodeActionParams,
) -> CodeActionResponse {
    let Some(text) = state.get_text(uri) else {
        return Vec::new();
    };
    let mut actions: CodeActionResponse = Vec::new();
    naming_actions(state, uri, params, &text, &mut actions);
    pragma_actions(state.locale, uri, params, &text, &mut actions);
    missing_body_actions(state.locale, uri, params, &text, &mut actions);
    undeclared_actions(state, uri, params, &text, &mut actions);
    indent_actions(state, uri, params, &text, &mut actions);
    removal_actions(state.locale, uri, params, &text, &mut actions);
    actions
}

/// Quick fixes do assistente de nomes: renomear para o estilo configurado.
fn naming_actions(
    state: &crate::workspace::WorkspaceState,
    uri: &str,
    params: &CodeActionParams,
    text: &str,
    actions: &mut CodeActionResponse,
) {
    let cfg = &state.config.analysis.naming;
    if !cfg.enabled {
        return;
    }
    // Ancorar no diagnóstico PP0018 (não na palavra crua sob o cursor): o
    // analyzer já posiciona o diagnóstico no identificador do símbolo. Isso
    // evita oferecer renomeação para keywords (`stock`/`public`/`new`...) e para
    // tokens dentro de comentários — que nunca geram PP0018.
    for diag in diagnostics_with_code(params, "PP0018") {
        let pos = diag.range.start;
        let Some(name) = crate::text::word_at(text, pos) else {
            continue;
        };
        for suggestion in crate::naming::suggestions_for(&name, cfg) {
            let Ok(Some(edit)) = intellisense::get_rename(state, uri, pos, &suggestion) else {
                continue;
            };
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: msg(state.locale, MsgKey::FixRenameTo).replace("{}", &suggestion),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diag.clone()]),
                edit: Some(edit),
                is_preferred: Some(true),
                ..Default::default()
            }));
        }
    }
}

/// Quick fixes das diretivas `#pragma` malformadas (PP0019): corrigir o nome
/// da diretiva ou tirar as aspas da mensagem de `deprecated`.
fn pragma_actions(
    locale: Locale,
    uri: &str,
    params: &CodeActionParams,
    text: &str,
    actions: &mut CodeActionResponse,
) {
    use crate::analyzer::pragmas::{PragmaFix, collect_issues};

    let diags = diagnostics_with_code(params, "PP0019");
    if diags.is_empty() {
        return;
    }
    let issues = collect_issues(text);
    for diag in diags {
        // Casa pela posição: um arquivo pode ter várias diretivas com problema.
        let Some(issue) = issues
            .iter()
            .find(|i| i.line == diag.range.start.line && i.col == diag.range.start.character)
        else {
            continue;
        };
        let Some(fix) = &issue.fix else { continue };
        let (title, new_text) = match fix {
            PragmaFix::Rename(s) => (
                msg(locale, MsgKey::FixUsePragma).replace("{}", s),
                s.clone(),
            ),
            PragmaFix::Unquote(inner) => (
                msg(locale, MsgKey::FixRemoveQuotes).to_string(),
                inner.clone(),
            ),
        };
        let range = Range {
            start: Position {
                line: issue.line,
                character: issue.col,
            },
            end: Position {
                line: issue.line,
                character: issue.col_end,
            },
        };
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title,
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag.clone()]),
            edit: Some(replacement_edit(uri, range, new_text)),
            is_preferred: Some(true),
            ..Default::default()
        }));
    }
}

/// `WorkspaceEdit` que substitui `range` por `new_text` no arquivo `uri`.
fn replacement_edit(uri: &str, range: Range, new_text: String) -> WorkspaceEdit {
    let mut changes = std::collections::HashMap::new();
    if let Ok(parsed) = uri.parse::<Url>() {
        changes.insert(parsed, vec![TextEdit { range, new_text }]);
    }
    WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    }
}

/// Quick fixes do PP0004 (`public`/`stock` sem corpo): dar um corpo vazio, ou
/// converter em `forward` — que é a forma de declarar sem corpo.
fn missing_body_actions(
    locale: Locale,
    uri: &str,
    params: &CodeActionParams,
    text: &str,
    actions: &mut CodeActionResponse,
) {
    let lines: Vec<&str> = text.lines().collect();
    for diag in diagnostics_with_code(params, "PP0004") {
        let idx = diag.range.start.line as usize;
        let Some(cur) = lines.get(idx) else { continue };
        let trimmed_end = cur.trim_end();
        // Só age na forma canônica `… );` — variações ficam para o usuário.
        if !trimmed_end.ends_with(';') {
            continue;
        }
        let semi = trimmed_end.len() - 1;
        let line = diag.range.start.line;

        // 1. Trocar o `;` por um corpo vazio.
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: msg(locale, MsgKey::FixAddEmptyBody).to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag.clone()]),
            edit: Some(replacement_edit(
                uri,
                Range {
                    start: Position {
                        line,
                        character: to_u32(semi),
                    },
                    end: Position {
                        line,
                        character: to_u32(trimmed_end.len()),
                    },
                },
                "\n{\n}".to_string(),
            )),
            is_preferred: Some(true),
            ..Default::default()
        }));

        // 2. Converter em `forward`, que declara sem corpo.
        let t = cur.trim_start();
        let indent = cur.len() - t.len();
        if let Some(kw) = ["public ", "stock "].iter().find(|k| t.starts_with(**k)) {
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: msg(locale, MsgKey::FixConvertToForward).replace("{}", kw.trim_end()),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diag.clone()]),
                edit: Some(replacement_edit(
                    uri,
                    Range {
                        start: Position {
                            line,
                            character: to_u32(indent),
                        },
                        end: Position {
                            line,
                            character: to_u32(indent + kw.len()),
                        },
                    },
                    "forward ".to_string(),
                )),
                ..Default::default()
            }));
        }
    }
}

/// Quick fix do PP0010: trocar a chamada por um símbolo conhecido de nome
/// parecido — o caso comum é um erro de digitação.
fn undeclared_actions(
    state: &crate::workspace::WorkspaceState,
    uri: &str,
    params: &CodeActionParams,
    text: &str,
    actions: &mut CodeActionResponse,
) {
    let diags = diagnostics_with_code(params, "PP0010");
    if diags.is_empty() {
        return;
    }
    let Some(file_path) = uri_to_path(uri) else {
        return;
    };
    let Some(parsed) = state.get_parsed(uri) else {
        return;
    };
    let inc_paths = state.include_paths.clone();

    for diag in diags {
        let Some(name) = crate::text::word_at(text, diag.range.start) else {
            continue;
        };
        let Some(suggestion) =
            intellisense::suggest_symbol(state, &file_path, &inc_paths, &parsed, &name)
        else {
            continue;
        };
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: msg(state.locale, MsgKey::FixReplaceWith).replace("{}", &suggestion),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag.clone()]),
            edit: Some(replacement_edit(uri, diag.range, suggestion)),
            is_preferred: Some(true),
            ..Default::default()
        }));
    }
}

/// Quick fix do PP0017: reindenta a linha usando o formatador, com o estilo
/// configurado no workspace — não uma indentação inventada aqui.
fn indent_actions(
    state: &crate::workspace::WorkspaceState,
    uri: &str,
    params: &CodeActionParams,
    text: &str,
    actions: &mut CodeActionResponse,
) {
    for diag in diagnostics_with_code(params, "PP0017") {
        let line = diag.range.start.line;
        let range = Range {
            start: Position { line, character: 0 },
            end: Position {
                line: line + 1,
                character: 0,
            },
        };
        // `format_range` aplica o preset e os overrides do workspace; o
        // `#pragma tabsize` do projeto já está refletido em `state.format_style`.
        let edits = intellisense::format_range(text, range, state.format_style);
        if edits.is_empty() {
            continue;
        }
        let mut changes = std::collections::HashMap::new();
        let Ok(parsed) = uri.parse::<Url>() else {
            continue;
        };
        changes.insert(parsed, edits);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: msg(state.locale, MsgKey::FixIndentation).to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag.clone()]),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            is_preferred: Some(true),
            ..Default::default()
        }));
    }
}

/// Quick fixes de remoção de código não usado, a partir dos diagnósticos nos
/// params. Não oferecido em arquivos `.inc` — onde um símbolo "não usado" pode
/// ser usado por quem consome a include (falso positivo ao desenvolvê-la).
fn removal_actions(
    locale: Locale,
    uri: &str,
    params: &CodeActionParams,
    text: &str,
    actions: &mut CodeActionResponse,
) {
    let is_inc = uri.to_ascii_lowercase().ends_with(".inc");
    for diag in &params.context.diagnostics {
        let Some(NumberOrString::String(code)) = &diag.code else {
            continue;
        };
        let Some(kind) = intellisense::removal_kind(code) else {
            continue;
        };
        // Em `.inc`, "não usado" é falso positivo (quem consome a include usa).
        // Corpo ilegal, porém, é erro de sintaxe em qualquer arquivo.
        if is_inc && kind != intellisense::RemovalKind::IllegalBody {
            continue;
        }
        let line = diag.range.start.line;
        let col = diag.range.start.character;
        let Some(range) = intellisense::removal_range(text, line, col, kind) else {
            continue;
        };
        let edit = workspace_edit(uri, range);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: removal_title(locale, code).to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag.clone()]),
            edit: Some(edit),
            ..Default::default()
        }));
    }
}

/// Diagnósticos dos params com o código dado (para associar ao quick fix).
fn diagnostics_with_code(params: &CodeActionParams, code: &str) -> Vec<Diagnostic> {
    params
        .context
        .diagnostics
        .iter()
        .filter(|d| d.code == Some(NumberOrString::String(code.to_string())))
        .cloned()
        .collect()
}

/// `WorkspaceEdit` que remove `range` (substitui por vazio) no arquivo `uri`.
fn workspace_edit(uri: &str, range: Range) -> WorkspaceEdit {
    let mut changes = std::collections::HashMap::new();
    if let Ok(parsed) = uri.parse::<Url>() {
        changes.insert(
            parsed,
            vec![TextEdit {
                range,
                new_text: String::new(),
            }],
        );
    }
    WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    }
}

fn removal_title(locale: Locale, code: &str) -> &'static str {
    let key = match code {
        "PP0009" => MsgKey::FixRemoveParam,
        "PP0005" => MsgKey::FixRemoveVariable,
        "PP0011" => MsgKey::FixRemoveDefine,
        "PP0012" => MsgKey::FixRemoveInclude,
        "PP0002" | "PP0003" => MsgKey::FixRemoveBody,
        _ => MsgKey::FixRemoveDeclaration,
    };
    msg(locale, key)
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
                ..Default::default()
            },
        )),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string(), "#".to_string(), "@".to_string()]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: Some(vec![",".to_string()]),
            ..Default::default()
        }),
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),
        references_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: intellisense::semantic_tokens_legend(),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..Default::default()
            },
        )),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: tower_lsp::lsp_types::WorkDoneProgressOptions::default(),
        })),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: None,
            file_operations: None,
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCALES: [Locale; 5] = [Locale::PtBr, Locale::Es, Locale::Ru, Locale::Ro, Locale::En];

    #[test]
    fn quick_fix_titles_follow_the_locale() {
        // Os títulos eram texto fixo em português: com o editor em inglês, o
        // diagnóstico vinha traduzido e a correção dele não.
        assert_eq!(
            removal_title(Locale::En, "PP0009"),
            "Remove unused parameter"
        );
        assert_eq!(
            removal_title(Locale::PtBr, "PP0009"),
            "Remover parâmetro não usado"
        );
        assert_ne!(
            removal_title(Locale::Ru, "PP0005"),
            removal_title(Locale::PtBr, "PP0005")
        );
    }

    #[test]
    fn quick_fix_titles_keep_their_placeholder_in_every_locale() {
        // Sem o `{}`, o título sairia sem o nome que a correção aplica.
        for key in [
            MsgKey::FixRenameTo,
            MsgKey::FixUsePragma,
            MsgKey::FixConvertToForward,
            MsgKey::FixReplaceWith,
        ] {
            for locale in LOCALES {
                assert_eq!(msg(locale, key).matches("{}").count(), 1, "{locale:?}");
            }
        }
    }
}
