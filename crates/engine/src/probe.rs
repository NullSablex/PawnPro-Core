//! Sonda contra um projeto Pawn de verdade.
//!
//! Roda só com `PAWNPRO_PROBE_PROJECT` apontando a pasta de um projeto: sem
//! ela, passa sem fazer nada — a engine não depende de projeto nenhum para
//! compilar. Com ela, passa cada `.pwn` e `.inc` pelo que a engine faz a cada
//! edição — análise, tokens semânticos e formatação — e cobra o que um exemplo
//! fixo não pega:
//!
//! - nenhuma etapa entra em pane, em arquivo nenhum;
//! - a formatação não muda os tokens de código nem o texto dos comentários —
//!   só o espaço entre eles;
//! - formatar de novo o que já foi formatado não muda nada.
//!
//! `PAWNPRO_PROBE_ONLY` restringe aos caminhos que contêm o texto dado, e
//! libera o detalhamento de onde vai o tempo da análise. `--nocapture` mostra
//! o relatório.
//!
//! ```bash
//! PAWNPRO_PROBE_PROJECT=/caminho/do/projeto cargo test --release -p pawnpro-engine probe -- --nocapture
//! ```

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tower_lsp::lsp_types::{HoverContents, Location, Position, Url};

use crate::analyzer::includes::collect_included_files;
use crate::analyzer::{
    deprecated, hints, includes, indentation, naming, pragmas, semantic, undefined, unused,
};
use crate::intellisense::{
    FormatStyle, format_document, get_code_lens, get_completions, get_definition, get_hover,
    get_references, get_semantic_tokens,
};
use crate::parser::parse_file;
use crate::parser::token_lexer::{TokenKind, tokenize};
use crate::workspace::WorkspaceState;

/// Onde um projeto costuma guardar os includes — o mesmo que o core entrega
/// quando o projeto não configura nada.
const INCLUDE_SUBDIRS: [&str; 3] = ["qawno/include", "pawno/include", "include"];

/// Quantas etapas lentas o relatório mostra.
const SLOWEST: usize = 10;

/// O projeto e o filtro de caminhos, se a sonda deve rodar.
fn target() -> Option<(PathBuf, Option<String>)> {
    let root = std::env::var("PAWNPRO_PROBE_PROJECT").ok()?;
    Some((
        PathBuf::from(root),
        std::env::var("PAWNPRO_PROBE_ONLY").ok(),
    ))
}

fn pawn_files(root: &Path, only: Option<&str>) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            !matches!(
                e.file_name().to_str(),
                Some(".git" | "node_modules" | ".pawnpro")
            )
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("pwn") || x.eq_ignore_ascii_case("inc"))
        })
        .filter(|p| only.is_none_or(|o| p.to_string_lossy().contains(o)))
        .collect();
    files.sort();
    files
}

/// O estado como o servidor o deixa depois de o core entregar a configuração.
fn project_state(root: &Path) -> WorkspaceState {
    let mut state = WorkspaceState::new();
    state.set_workspace_root(root.to_path_buf());
    state.include_paths = INCLUDE_SUBDIRS
        .iter()
        .map(|sub| root.join(sub))
        .filter(|p| p.is_dir())
        .collect();
    let sdk = root.join("qawno").join("include").join("open.mp.inc");
    state.set_sdk_file_opt(sdk.exists().then_some(sdk));
    state
}

/// Pawn costuma estar em windows-1252; para o que se mede aqui, um acento
/// trocado não muda nada.
fn read_text(file: &Path) -> Option<String> {
    std::fs::read(file)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// O texto depois de formatado.
///
/// `format_document` devolve nada quando já está formatado, ou uma edição
/// única com o documento inteiro.
fn formatted(text: &str, style: FormatStyle) -> String {
    format_document(text, style)
        .into_iter()
        .next()
        .map_or_else(|| text.to_string(), |edit| edit.new_text)
}

/// O texto sem o recuo de cada linha. Tanto a comparação quanto o relatório
/// tokenizam por aqui: se só um deles tirasse o recuo, o relatório apontaria um
/// lugar diferente do que disparou o alarme.
fn dedent(text: &str) -> String {
    text.lines()
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Os tokens de código, na ordem. O léxico descarta comentário e espaço: se a
/// formatação quebrar um `*/`, o código que ficou "dentro" do comentário some
/// daqui.
///
/// O recuo de cada linha sai antes: o léxico dá tokens diferentes para um `#`
/// recuado e para um `#` no começo da linha, e tirar o recuo de uma diretiva é
/// justamente o que o formatador faz. Sem isso, cada `#endinput` recuado viraria
/// um falso "código alterado".
fn code_tokens(text: &str) -> Vec<String> {
    tokenize(&dedent(text))
        .tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Eof))
        .map(|t| t.value)
        .collect()
}

/// O texto de cada comentário, com o espaço interno normalizado.
///
/// Normalizado porque reindentar um comentário de várias linhas muda o recuo
/// das linhas de dentro — isso é esperado; mudar o que está escrito, não.
fn comments(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let (mut in_str, mut in_char) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if in_str || in_char {
            if c == '\\' {
                i += 1;
            } else if (in_str && c == '"') || (in_char && c == '\'') {
                (in_str, in_char) = (false, false);
            }
        } else if c == '"' {
            in_str = true;
        } else if c == '\'' {
            in_char = true;
        } else if c == '/' && matches!(next, Some('/' | '*')) {
            let block = next == Some('*');
            let start = i;
            i += 2;
            while i < chars.len() {
                if block && chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    i += 2;
                    break;
                }
                if !block && chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            let body: String = chars[start..i.min(chars.len())].iter().collect();
            found.push(body.split_whitespace().collect::<Vec<_>>().join(" "));
            continue;
        }
        i += 1;
    }
    found
}

/// Onde duas versões do texto divergem em tokens de código: o primeiro token
/// diferente, com os vizinhos e a linha de cada lado.
fn first_token_difference(before: &str, after: &str) -> String {
    let a = tokenize(&dedent(before)).tokens;
    let b = tokenize(&dedent(after)).tokens;
    let at = a
        .iter()
        .zip(&b)
        .position(|(x, y)| x.value != y.value)
        .unwrap_or_else(|| a.len().min(b.len()));
    let around = |tokens: &[crate::parser::token_lexer::Token]| {
        let line = tokens.get(at).map_or(0, |t| t.line + 1);
        let window: Vec<&str> = tokens
            .iter()
            .skip(at.saturating_sub(2))
            .take(5)
            .map(|t| t.value.as_str())
            .collect();
        format!("linha {line}: {window:?}")
    };
    format!("token {at} — antes {}; depois {}", around(&a), around(&b))
}

/// O primeiro comentário que mudou, como estava e como ficou.
fn first_comment_difference(before: &str, after: &str) -> String {
    let (a, b) = (comments(before), comments(after));
    let short = |s: &str| s.chars().take(90).collect::<String>();
    a.iter().zip(&b).find(|(x, y)| x != y).map_or_else(
        || format!("comentários: {} → {}", a.len(), b.len()),
        |(x, y)| format!("antes {:?}; depois {:?}", short(x), short(y)),
    )
}

/// A primeira linha em que duas versões do texto divergem.
fn first_difference(before: &str, after: &str) -> String {
    before
        .lines()
        .zip(after.lines())
        .enumerate()
        .find(|(_, (b, a))| b != a)
        .map_or_else(
            || {
                format!(
                    "linhas: {} → {}",
                    before.lines().count(),
                    after.lines().count()
                )
            },
            |(i, (b, a))| format!("linha {}: {b:?} → {a:?}", i + 1),
        )
}

/// Roda uma etapa isolada: a pane de um arquivo não interrompe os outros.
fn timed<T>(work: impl FnOnce() -> T) -> (Option<T>, Duration) {
    let start = Instant::now();
    let out = catch_unwind(AssertUnwindSafe(work)).ok();
    (out, start.elapsed())
}

/// Cronometra uma etapa sem isolar pane.
fn stage<T>(work: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = work();
    (out, start.elapsed())
}

#[test]
fn a_real_project_survives_what_the_editor_does_on_every_edit() {
    let Some((root, only)) = target() else {
        return;
    };
    let state = project_state(&root);
    let style = FormatStyle::default();

    let files = pawn_files(&root, only.as_deref());
    let mut panics = Vec::new();
    let mut altered = Vec::new();
    let mut unstable = Vec::new();
    let mut slow: Vec<(Duration, &str, &Path)> = Vec::new();

    for file in &files {
        let Some(text) = read_text(file) else {
            continue;
        };
        let Ok(uri) = Url::from_file_path(file) else {
            continue;
        };
        let uri = uri.to_string();
        state.open_document(uri.clone(), text.clone(), 1);

        let (analysis, took) = timed(|| state.analyze(&uri));
        if analysis.is_none() {
            panics.push(format!("análise: {}", file.display()));
        }
        slow.push((took, "análise", file));

        let (tokens, took) = timed(|| get_semantic_tokens(&state, &uri));
        if tokens.is_none() {
            panics.push(format!("tokens: {}", file.display()));
        }
        slow.push((took, "tokens", file));

        let (once, took) = timed(|| formatted(&text, style));
        slow.push((took, "formatação", file));
        if let Some(once) = once {
            let damage = if code_tokens(&once) != code_tokens(&text) {
                Some("código")
            } else if comments(&once) != comments(&text) {
                Some("comentário")
            } else {
                None
            };
            if let Some(what) = damage {
                // No código, a primeira linha diferente costuma ser só recuo; o
                // que importa é onde a sequência de tokens diverge.
                let detail = if what == "código" {
                    first_token_difference(&text, &once)
                } else {
                    first_comment_difference(&text, &once)
                };
                altered.push(format!("{} [{what}]  ({detail})", file.display()));
            }
            match timed(|| formatted(&once, style)).0 {
                None => panics.push(format!("reformatação: {}", file.display())),
                Some(twice) if twice != once => unstable.push(format!(
                    "{}  ({})",
                    file.display(),
                    first_difference(&once, &twice)
                )),
                Some(_) => {}
            }
        } else {
            panics.push(format!("formatação: {}", file.display()));
        }

        state.close_document(&uri);
    }

    slow.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    eprintln!("\n== sonda: {} arquivos em {}", files.len(), root.display());
    eprintln!("-- etapas mais lentas:");
    for (took, stage, file) in slow.iter().take(SLOWEST) {
        eprintln!("{took:>12.1?}  {stage:<11} {}", file.display());
    }
    eprintln!(
        "-- pane: {}   código ou comentário alterado pela formatação: {}   formatação instável: {}",
        panics.len(),
        altered.len(),
        unstable.len()
    );

    assert!(panics.is_empty(), "entraram em pane: {panics:#?}");
    assert!(
        altered.is_empty(),
        "a formatação mudou código ou comentário: {altered:#?}"
    );
    assert!(
        unstable.is_empty(),
        "formatar de novo mudou o resultado: {unstable:#?}"
    );
}

/// As linhas em volta da primeira diferença entre duas formatações, nos
/// arquivos filtrados por `PAWNPRO_PROBE_ONLY`.
///
/// É o que permite recortar o menor trecho que oscila e transformá-lo num
/// teste de regressão.
#[test]
fn show_where_formatting_is_unstable() {
    let Some((root, Some(only))) = target() else {
        return;
    };
    let style = FormatStyle::default();
    for file in pawn_files(&root, Some(&only)) {
        let Some(text) = read_text(&file) else {
            continue;
        };
        let once = formatted(&text, style);
        let twice = formatted(&once, style);
        let Some(at) = once.lines().zip(twice.lines()).position(|(a, b)| a != b) else {
            continue;
        };
        let from = at.saturating_sub(8);
        eprintln!("\n== {} (difere na linha {})", file.display(), at + 1);
        for (label, version) in [("original", text.as_str()), ("1ª", &once), ("2ª", &twice)] {
            eprintln!("-- {label}:");
            for (i, line) in version.lines().enumerate().skip(from).take(12) {
                eprintln!("{:>5}: {line:?}", i + 1);
            }
        }
    }
}

/// Onde os tokens de código divergem, com as linhas de cada lado, nos arquivos
/// filtrados por `PAWNPRO_PROBE_ONLY`.
///
/// Serve ao "código alterado" que o relatório resumido não explica: mostra os
/// tokens em volta do ponto e o texto das linhas em que eles estão.
#[test]
fn show_where_code_tokens_diverge() {
    let Some((root, Some(only))) = target() else {
        return;
    };
    let style = FormatStyle::default();
    let dedent = |text: &str| {
        text.lines()
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n")
    };
    for file in pawn_files(&root, Some(&only)) {
        let Some(text) = read_text(&file) else {
            continue;
        };
        let (before, after) = (dedent(&text), dedent(&formatted(&text, style)));
        let (a, b) = (tokenize(&before).tokens, tokenize(&after).tokens);
        let Some(at) = a.iter().zip(&b).position(|(x, y)| x.value != y.value) else {
            continue;
        };
        eprintln!("\n== {} (token {at})", file.display());
        for (label, tokens, source) in [("antes", &a, &before), ("depois", &b, &after)] {
            let around: Vec<(u32, &str)> = tokens
                .iter()
                .skip(at.saturating_sub(3))
                .take(8)
                .map(|t| (t.line + 1, t.value.as_str()))
                .collect();
            eprintln!("-- {label}: {around:?}");
            let line = tokens.get(at).map_or(0, |t| t.line as usize);
            for (i, l) in source
                .lines()
                .enumerate()
                .skip(line.saturating_sub(3))
                .take(6)
            {
                eprintln!("{:>5}: {l:?}", i + 1);
            }
        }
    }
}

/// O que a análise de um arquivo custou, peça por peça.
struct StageTimes {
    parts: Vec<(&'static str, Duration)>,
    includes: usize,
    bytes: usize,
}

/// Refaz o que `WorkspaceState::analyze` faz, na mesma ordem, cronometrando
/// cada peça.
fn stage_times(state: &WorkspaceState, file: &Path, text: &str) -> StageTimes {
    let inc_paths = &state.include_paths;
    let locale = state.locale;

    let (parsed, t_parse) = stage(|| parse_file(text));
    let (resolved, t_collect) =
        stage(|| collect_included_files(file, inc_paths, &parsed.includes, 16, 1000));
    let inc_texts: Vec<&str> = resolved.files.values().map(|e| e.text.as_str()).collect();

    let parts = vec![
        ("parse do arquivo", t_parse),
        ("leitura dos includes", t_collect),
        (
            "includes",
            stage(|| includes::analyze_includes(&parsed.includes, file, inc_paths, locale)).1,
        ),
        (
            "semantic",
            stage(|| semantic::analyze_semantics(text, locale)).1,
        ),
        (
            "unused",
            // A coleta dos outros arquivos entra na conta: é o custo real a cada edição.
            stage(|| {
                let workspace = state.other_idents(file, &state.open_paths());
                unused::analyze_unused(
                    text,
                    file,
                    &parsed,
                    &resolved,
                    state.config.analysis.warn_unused_in_inc,
                    &workspace,
                    locale,
                )
            })
            .1,
        ),
        (
            "deprecated",
            stage(|| {
                deprecated::analyze_deprecated(text, file, &parsed, inc_paths, &resolved, locale)
            })
            .1,
        ),
        (
            "pragmas",
            stage(|| pragmas::analyze_pragmas(text, locale)).1,
        ),
        (
            "hints",
            stage(|| hints::analyze_hints(text, &parsed.symbols, locale)).1,
        ),
        (
            "undefined",
            stage(|| {
                undefined::analyze_undefined(
                    text,
                    file,
                    &parsed,
                    &resolved,
                    state.sdk_parsed.as_ref(),
                    locale,
                )
            })
            .1,
        ),
        (
            "indentation",
            stage(|| indentation::analyze_indentation(text, &inc_texts, None, locale)).1,
        ),
        (
            "naming",
            stage(|| {
                naming::analyze_naming(text, &parsed.symbols, &state.config.analysis.naming, locale)
            })
            .1,
        ),
    ];

    StageTimes {
        parts,
        includes: resolved.files.len(),
        bytes: resolved.files.values().map(|e| e.text.len()).sum(),
    }
}

/// Um cenário do uso real, montado no projeto da sonda só em memória: nada é
/// gravado nele.
struct RealScenario {
    root: PathBuf,
    state: WorkspaceState,
}

impl RealScenario {
    fn new() -> Option<Self> {
        let (root, _) = target()?;
        let state = project_state(&root);
        Some(Self { root, state })
    }

    /// Abre o arquivo como o editor o abriria, com o texto do disco: o
    /// caminho, a URI e o texto. `None` se ele não existe no projeto.
    fn open(&self, relative: &str) -> Option<(PathBuf, String, String)> {
        let path = self.root.join(relative);
        let text = read_text(&path)?;
        let uri = Url::from_file_path(&path).ok()?.to_string();
        self.state.open_document(uri.clone(), text.clone(), 1);
        Some((path, uri, text))
    }

    fn unit(&self, file: &Path) -> std::collections::HashSet<PathBuf> {
        self.state.unit_files(file, &self.state.open_paths())
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// Os programas da unidade de `file`, relativos à raiz: os `.pwn` que
    /// nenhum arquivo da unidade inclui — um `.pwn` incluído é trecho.
    fn programs(&self, file: &Path) -> Vec<String> {
        let open = self.state.open_paths();
        let unit = self.unit(file);
        let included: std::collections::HashSet<PathBuf> = unit
            .iter()
            .filter_map(|f| {
                let dir = f.parent()?.to_path_buf();
                let idents = self.state.idents_of(f, &open)?;
                Some(
                    idents
                        .includes()
                        .iter()
                        .filter_map(|d| {
                            includes::resolve_include(d, &dir, &self.state.include_paths)
                        })
                        .map(|p| p.canonicalize().unwrap_or(p))
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect();
        let mut names: Vec<String> = unit
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pwn")))
            .filter(|p| !included.contains(p))
            .map(|p| self.relative(&p))
            .collect();
        names.sort();
        names
    }
}

/// A posição LSP da primeira ocorrência de `needle`, `skip` bytes adentro.
fn position_of(text: &str, needle: &str, skip: usize) -> Option<Position> {
    text.lines().enumerate().find_map(|(i, l)| {
        l.find(needle).map(|c| Position {
            line: crate::util::to_u32(i),
            character: crate::text::utf16_col(l, c + skip),
        })
    })
}

/// O arquivo de uma localização, canônico.
fn file_of(loc: &Location) -> PathBuf {
    loc.uri
        .to_file_path()
        .map(|p| p.canonicalize().unwrap_or(p))
        .unwrap_or_default()
}

/// O uso real: o `gl_commands.inc` chama `NivelRequerido`, que o
/// `gl_stocks.inc` declara. Confere duas vezes — como está no disco, e com a
/// declaração renomeada para `NivelRequerido1` só em memória, o caso em que o
/// nome antigo sobra apenas em outros programas. Nas duas, o hover e a
/// definição no uso só podem vir da unidade de compilação dele.
#[test]
fn a_renamed_stock_is_not_found_in_another_program() {
    let Some(sc) = RealScenario::new() else {
        return;
    };
    let Some((path, uri, text)) = sc.open("include/gl_commands.inc") else {
        println!("== cenário 1: gl_commands.inc ausente, nada a conferir");
        return;
    };
    let Some(call) = position_of(&text, "NivelRequerido(", 2) else {
        println!("== cenário 1: nenhum uso de NivelRequerido, nada a conferir");
        return;
    };
    let Some((_, stocks_uri, on_disk)) = sc.open("include/gl_stocks.inc") else {
        println!("== cenário 1: gl_stocks.inc ausente, nada a conferir");
        return;
    };
    let renamed = on_disk.replace("stock NivelRequerido(", "stock NivelRequerido1(");

    println!(
        "== cenário 1: NivelRequerido no gl_commands.inc, linha {}",
        call.line + 1
    );
    println!("   programas que o compilam: {:?}", sc.programs(&path));
    for (version, label, stocks) in [
        (2, "como no disco", &on_disk),
        (3, "renomeada em memória", &renamed),
    ] {
        sc.state
            .change_document(&stocks_uri, stocks.clone(), version);
        let started = Instant::now();
        let hover = get_hover(&sc.state, &uri, call).map(|h| match h.contents {
            HoverContents::Markup(m) => m.value.lines().nth(1).unwrap_or("").to_string(),
            other => format!("{other:?}"),
        });
        let hover_time = started.elapsed();
        let started = Instant::now();
        let definition = get_definition(&sc.state, &uri, call);
        let definition_time = started.elapsed();
        println!(
            "   [{label}] hover ({hover_time:.1?}): {} — definição ({definition_time:.1?}): {}",
            hover.as_deref().unwrap_or("(nenhum)"),
            definition.as_ref().map_or_else(
                || "(nenhuma)".to_string(),
                |l| format!("{}:{}", sc.relative(&file_of(l)), l.range.start.line + 1)
            )
        );
        if let Some(loc) = &definition {
            assert!(
                sc.unit(&path).contains(&file_of(loc)),
                "[{label}] a definição saiu da unidade: {loc:?}"
            );
        }
    }

    // O pior caso: uma palavra comum sem declaração global. A busca passa pela
    // unidade inteira antes de desistir, e o autocomplete junta tudo dela.
    if let Some(common) = position_of(&text, "playerid", 2) {
        let started = Instant::now();
        let _ = get_hover(&sc.state, &uri, common);
        let hover_time = started.elapsed();
        let started = Instant::now();
        let items = get_completions(&sc.state, &uri, common).len();
        let completion_time = started.elapsed();
        println!(
            "   [pior caso, `playerid`] hover: {hover_time:.1?} — autocomplete: {items} itens em {completion_time:.1?}"
        );
    }
}

/// O uso real: `filterscripts/bplr.pwn` declara o próprio `NivelRequerido`.
/// As referências e o contador dele ficam dentro do programa dele — as
/// chamadas do `gl_commands.inc` não entram, a menos que ela o inclua.
#[test]
fn a_filterscript_counts_only_its_own_calls() {
    let Some(sc) = RealScenario::new() else {
        return;
    };
    let Some((fs, uri, text)) = sc.open("filterscripts/bplr.pwn") else {
        println!("== cenário 2: filterscripts/bplr.pwn ausente, nada a conferir");
        return;
    };
    let Some(decl) = position_of(&text, "stock NivelRequerido(", 7) else {
        println!("== cenário 2: a filterscript não declara NivelRequerido, nada a conferir");
        return;
    };
    let refs = get_references(&sc.state, &uri, decl);
    let unit = sc.unit(&fs);
    let outside: Vec<_> = refs
        .iter()
        .filter(|l| !unit.contains(&file_of(l)))
        .collect();
    let title = get_code_lens(&sc.state, &uri)
        .into_iter()
        .find(|l| l.range.start.line == decl.line)
        .and_then(|l| l.command.map(|c| c.title));
    let mut files: Vec<String> = refs.iter().map(|l| sc.relative(&file_of(l))).collect();
    files.dedup();

    println!("== cenário 2: NivelRequerido em filterscripts/bplr.pwn");
    println!("   programas da unidade: {:?}", sc.programs(&fs));
    println!(
        "   referências: {} (fora da unidade: {})",
        refs.len(),
        outside.len()
    );
    println!("   arquivos: {files:?}");
    println!("   contador: {}", title.as_deref().unwrap_or("(nenhum)"));
    assert!(
        outside.is_empty(),
        "referências de outro programa: {outside:?}"
    );
    let commands = sc.root.join("include").join("gl_commands.inc");
    let commands = commands.canonicalize().unwrap_or(commands);
    if !unit.contains(&commands) {
        assert!(
            !refs.iter().any(|l| file_of(l) == commands),
            "chamadas do gl_commands.inc entraram na filterscript"
        );
    }
}

/// Onde vai o tempo da análise, nos arquivos filtrados por `PAWNPRO_PROBE_ONLY`.
///
/// Cronometra cada peça e analisa também o arquivo inteiro duas vezes
/// seguidas: se a segunda não sai mais rápida, nada da primeira é aproveitado.
#[test]
fn where_the_analysis_time_goes() {
    let Some((root, Some(only))) = target() else {
        return;
    };
    let state = project_state(&root);

    for file in pawn_files(&root, Some(&only)) {
        let Some(text) = read_text(&file) else {
            continue;
        };
        let times = stage_times(&state, &file, &text);

        let Ok(uri) = Url::from_file_path(&file) else {
            continue;
        };
        let uri = uri.to_string();
        state.open_document(uri.clone(), text.clone(), 1);
        let first = stage(|| state.analyze(&uri)).1;
        let second = stage(|| state.analyze(&uri)).1;
        state.close_document(&uri);

        eprintln!(
            "\n== {}  ({} includes, {} KB lidos)",
            file.display(),
            times.includes,
            times.bytes / 1024
        );
        for (name, took) in times.parts {
            eprintln!("{took:>12.1?}  {name}");
        }
        eprintln!("-- análise completa: 1ª {first:.1?}, 2ª {second:.1?}");
    }
}
