//! Sessão DAP: estado e roteamento de requests. Independente de I/O — recebe um
//! [`Request`] e devolve as mensagens a enviar, o que a torna testável sem stream.
//!
//! Traduz DAP usando o `samp_sdk::debug` para mapear linha ↔ endereço. A pilha
//! da última pausa chega pelo [`FrameCache`], que a conexão com o plugin
//! alimenta; o resto do estado é desta sessão e de mais ninguém.

use pawnpro_dbg_protocol::{Breakpoint, Command, DataWatch, Step};
use samp_sdk::debug::AmxDbg;
use serde_json::{Value, json};

use pawnpro_dbg_protocol::messages::{self, Locale, MsgKey};

use crate::dap::{Event, Request, Response};
use crate::frames::FrameCache;
use crate::sources::{LineView, Sources};

/// Mensagem de saída do `session`. Mantém o `session` puro: ele decide, o
/// laço de [`crate::serve`] executa o I/O (responde ao editor ou fala com o
/// plugin).
pub enum Outgoing {
    Response(Response),
    Event(Event),
    /// Pôr um servidor no lugar do que houver, com um canal novo para o
    /// plugin dele. Serve tanto ao `launch` (não há o que derrubar) quanto ao
    /// `restart` (derruba o atual e sobe outro, mantendo a sessão de depuração
    /// viva). O servidor morre junto com a sessão, sem o editor rastrear nada.
    SpawnServer(SpawnSpec),
    /// Derrubar o servidor e a conexão com o plugin dele, mantendo a sessão.
    StopServer,
    /// Encaminhar um comando ao plugin (breakpoints/continue/step).
    ToPlugin(Command),
    /// Ler memória crua do plugin (bloqueia esperando a resposta) e responder o
    /// `readMemory` do editor. Resolvido no laço (o `session` é puro). `seq` é a
    /// resposta já numerada; `address` é o texto que volta no campo `address`.
    ReadMemory {
        seq: i64,
        address: String,
        frame: usize,
        name: String,
        path: Vec<usize>,
        offset: i64,
        count: usize,
    },
    /// Gravar uma célula no plugin, esperar a confirmação e só então responder
    /// o `setVariable`/`setExpression` — e refletir o valor no painel. Resolvido
    /// no laço, como a leitura de memória.
    WriteVariable(Write),
}

/// Uma escrita pedida pelo editor, com o que a resposta precisa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    /// A resposta já numerada.
    pub seq: i64,
    pub frame: usize,
    pub name: String,
    pub path: Vec<usize>,
    pub value: i32,
    /// O texto que volta ao painel no sucesso.
    pub shown: String,
    /// A posição da variável no frame, para atualizar o cache.
    pub var: usize,
}

impl Write {
    /// Como o editor escreve a variável: `nome[i][j]`.
    #[must_use]
    pub fn label(&self) -> String {
        self.path
            .iter()
            .fold(self.name.clone(), |label, i| format!("{label}[{i}]"))
    }
}

/// Comando do servidor a executar, mais o que o plugin precisa saber dele.
///
/// O canal com o plugin não está aqui: quem o reserva é o laço, a cada subida,
/// para que um servidor velho nunca alcance a sessão do novo.
#[derive(Clone)]
pub struct SpawnSpec {
    pub exe: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub amx_path: String,
    /// Locale do editor (ex.: `pt-BR`), para o plugin localizar as mensagens de
    /// erro. Vazio = inglês.
    pub locale: String,
}

/// Um breakpoint como pedido pelo editor (DAP), antes de resolver a linha em
/// endereço. Campos opcionais espelham os modificadores do DAP.
#[derive(Clone)]
struct ReqBp {
    line: i32,
    condition: Option<String>,
    hit_condition: Option<String>,
    log_message: Option<String>,
}

#[derive(Default)]
pub struct Session {
    seq: i64,
    /// Bloco de debug do `.amx` em depuração (carregado no `launch`).
    dbg: Option<AmxDbg>,
    /// Breakpoints de linha resolvidos: (linha-fonte, endereço de código).
    breakpoints: Vec<(i32, u32)>,
    /// Breakpoints de linha resolvidos (forma completa, com modificadores).
    line_bps: Vec<Breakpoint>,
    /// Breakpoints de função resolvidos (parar ao entrar na função por nome).
    fn_bps: Vec<Breakpoint>,
    /// Caminho do arquivo-fonte (o `source.path` que o editor enviou em
    /// `setBreakpoints`). Usado no `stackTrace` para o frame apontar à fonte —
    /// senão o editor mostra "Origem Desconhecida".
    source_path: Option<String>,
    /// Guarda contra laço: o restart pede a recompilação por evento e a
    /// extensão reenvia o `restart`. Sem isto, um `.pwn` cuja data continuasse
    /// à frente do `.amx` (compilação falhou, relógio adiantado) pediria
    /// rebuild para sempre.
    rebuild_requested: bool,
    /// Idioma das mensagens do adaptador, do `locale` do `initialize`.
    locale: Locale,
    terminated: bool,
    /// O editor já recebeu `terminated`: mandar de novo repetiria o fim.
    terminated_sent: bool,
    /// Breakpoints como o editor os pediu — linha e modificadores, antes de
    /// virarem endereço.
    ///
    /// O endereço depende do `.amx`: recompilar o gamemode muda o mapa
    /// linha↔endereço, e os endereços resolvidos antes passam a apontar para
    /// instruções erradas. Guardar o pedido original é o que permite resolver
    /// tudo de novo depois de um restart.
    requested_bps: Vec<(String, Vec<ReqBp>)>,
    /// Como o servidor foi iniciado, guardado do `launch`.
    ///
    /// É o que permite reiniciá-lo **sem encerrar a sessão**: o servidor novo
    /// recebe um canal novo, e o estado que vale (breakpoints, bloco de debug)
    /// continua aqui.
    spawn_spec: Option<SpawnSpec>,
    /// Pilha da última pausa, alimentada pela conexão com o plugin.
    frames: FrameCache,
    /// Arrays e sub-arrays expansíveis da pausa atual; o `variablesReference`
    /// de cada um é [`VAR_REF_BASE`] + a posição aqui.
    var_refs: Vec<VarPath>,
    /// O texto dos fontes como foi compilado, para breakpoints e pilha
    /// continuarem certos depois de uma edição.
    sources: Sources,
}

/// `true` se o restart precisa de um `.amx` novo, compilado com `-d3`.
///
/// Dois casos: o `.pwn` ao lado mudou depois do `.amx`, ou o `.amx` não tem
/// bloco de debug — o comando Compilar comum gera o binário de produção, sem
/// `-d3`, e a data sozinha não diz isso. Sem `.pwn` não há o que compilar, e
/// pedir travaria o restart.
fn needs_rebuild(amx: &str) -> bool {
    let Some(source) = amx
        .strip_suffix(".amx")
        .or_else(|| amx.strip_suffix(".AMX"))
        .map(|base| format!("{base}.pwn"))
    else {
        return false;
    };
    let Ok(source_modified) = std::fs::metadata(&source).and_then(|m| m.modified()) else {
        return false;
    };
    let Ok(bytes) = std::fs::read(amx) else {
        return false;
    };
    let newer = std::fs::metadata(amx)
        .and_then(|m| m.modified())
        .is_ok_and(|amx_modified| source_modified > amx_modified);
    newer || read_debug(&bytes).is_none()
}

/// O bloco de debug de um `.amx`, ou de um arquivo de debug avulso.
fn read_debug(bytes: &[u8]) -> Option<AmxDbg> {
    AmxDbg::from_amx(bytes)
        .or_else(|_| AmxDbg::parse(bytes))
        .ok()
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    const fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    /// Resposta `ok` simples a um request (o caso da maioria dos handlers).
    fn reply(&mut self, req: &Request, body: Value) -> Vec<Outgoing> {
        let seq = self.next_seq();
        vec![Outgoing::Response(Response::ok(seq, req, body))]
    }

    /// Resposta `ok` precedida de um comando ao plugin (continue/step/...). O
    /// comando vai antes da resposta para o plugin já receber a ação.
    fn reply_with(&mut self, req: &Request, cmd: Command, body: Value) -> Vec<Outgoing> {
        let seq = self.next_seq();
        vec![
            Outgoing::ToPlugin(cmd),
            Outgoing::Response(Response::ok(seq, req, body)),
        ]
    }

    #[must_use]
    pub const fn is_terminated(&self) -> bool {
        self.terminated
    }

    /// Idioma pedido pelo editor no `initialize`.
    #[must_use]
    pub const fn locale(&self) -> Locale {
        self.locale
    }

    /// A pilha compartilhada com a conexão do plugin.
    #[must_use]
    pub fn frames(&self) -> FrameCache {
        self.frames.clone()
    }

    /// Processa um request e devolve as mensagens a enviar (resposta + eventos).
    pub fn handle(&mut self, req: &Request) -> Vec<Outgoing> {
        match req.command.as_str() {
            "initialize" => self.on_initialize(req),
            "launch" => self.on_launch(req),
            "setBreakpoints" => self.on_set_breakpoints(req),
            "setFunctionBreakpoints" => self.on_set_function_breakpoints(req),
            "threads" => self.on_threads(req),
            "continue" => self.on_continue(req),
            "next" => self.on_step(req, Step::Over),
            "stepIn" => self.on_step(req, Step::In),
            "stepOut" => self.on_step(req, Step::Out),
            "stackTrace" => self.on_stack_trace(req),
            "scopes" => self.on_scopes(req),
            "variables" => self.on_variables(req),
            "setVariable" => self.on_set_variable(req),
            "setExpression" => self.on_set_expression(req),
            "dataBreakpointInfo" => self.on_data_breakpoint_info(req),
            "setDataBreakpoints" => self.on_set_data_breakpoints(req),
            "setExceptionBreakpoints" => self.on_set_exception_breakpoints(req),
            "completions" => self.on_completions(req),
            "readMemory" => self.on_read_memory(req),
            "evaluate" => self.on_evaluate(req),
            "terminate" => self.on_terminate(req),
            "disconnect" => self.on_disconnect(req),
            "restart" => self.on_restart(req),
            "configurationDone" => self.on_configuration_done(req),
            // Comandos ainda não implementados respondem ok vazio para não travar
            // o cliente.
            _ => self.ack(req),
        }
    }

    fn ack(&mut self, req: &Request) -> Vec<Outgoing> {
        self.reply(req, Value::Null)
    }

    fn on_initialize(&mut self, req: &Request) -> Vec<Outgoing> {
        // O cliente informa o idioma no `initialize`; guardamos para localizar as
        // mensagens do adaptador (o plugin recebe o seu próprio via `launch`).
        self.locale = req
            .arguments
            .get("locale")
            .and_then(Value::as_str)
            .map_or_else(Locale::default, Locale::from_tag);
        let runtime_label = messages::msg(self.locale, MsgKey::RuntimeErrorsLabel);
        // `supportsEvaluateForHovers` reaproveita o `evaluate` do painel
        // INSPEÇÃO ao passar o mouse no código.
        let caps = json!({
            "supportsConfigurationDoneRequest": true,
            "supportsTerminateRequest": true,
            // Sem isto o editor encerra e recria a sessão para reiniciar. Com
            // isto ele manda `restart`, e o servidor é trocado por baixo sem a
            // depuração cair.
            "supportsRestartRequest": true,
            "supportsEvaluateForHovers": true,
            "supportsConditionalBreakpoints": true,
            "supportsHitConditionalBreakpoints": true,
            "supportsLogPoints": true,
            "supportsSetVariable": true,
            "supportsSetExpression": true,
            "supportsDataBreakpoints": true,
            "supportsFunctionBreakpoints": true,
            "supportsCompletionsRequest": true,
            "supportsReadMemoryRequest": true,
            // Filtro de exceção: o editor liga/desliga a pausa em erros de runtime.
            "exceptionBreakpointFilters": [
                { "filter": "runtime", "label": runtime_label, "default": true }
            ],
        });
        let seq = self.next_seq();
        let resp = Response::ok(seq, req, caps);
        // DAP: após responder o initialize, emitir o evento `initialized`.
        let ev_seq = self.next_seq();
        let ev = Event::new(ev_seq, "initialized", Value::Null);
        vec![Outgoing::Response(resp), Outgoing::Event(ev)]
    }

    /// Carrega o bloco de debug do `.amx` e guarda o texto dos fontes dele.
    ///
    /// Os dois juntos: o retrato só vale para o binário que acabou de ser
    /// lido, e um bloco novo com o retrato velho mapearia linhas erradas.
    ///
    /// Sem bloco de debug, o mapa anterior sai junto: mantê-lo resolveria
    /// breakpoints com endereços de outro binário, em silêncio. Devolve o aviso
    /// para o console nesse caso.
    fn load_debug(&mut self, amx_path: &str) -> Option<Outgoing> {
        let loaded = std::fs::read(amx_path)
            .ok()
            .and_then(|bytes| read_debug(&bytes));
        if let Some(dbg) = loaded {
            self.sources = Sources::capture(std::path::Path::new(amx_path), &dbg);
            self.dbg = Some(dbg);
            return None;
        }
        self.dbg = None;
        self.sources = Sources::default();
        let text = messages::format(self.locale, MsgKey::AmxWithoutDebugInfo, &[amx_path]);
        let seq = self.next_seq();
        Some(Outgoing::Event(Event::new(
            seq,
            "output",
            json!({ "category": "important", "output": format!("{text}\n") }),
        )))
    }

    /// Como as linhas do arquivo `path` do editor se relacionam com as
    /// compiladas. Sem caminho, valem como estão.
    fn view(&self, path: &str) -> LineView {
        if path.is_empty() {
            LineView::Unchanged
        } else {
            self.sources.view(path)
        }
    }

    /// A referência do painel para um array ou sub-array, reaproveitando a que
    /// já existe.
    fn var_ref(&mut self, path: VarPath) -> i64 {
        let index = self
            .var_refs
            .iter()
            .position(|p| *p == path)
            .unwrap_or_else(|| {
                self.var_refs.push(path);
                self.var_refs.len() - 1
            });
        VAR_REF_BASE + i64::try_from(index).unwrap_or(0)
    }

    fn var_path(&self, reference: i64) -> Option<&VarPath> {
        let index = usize::try_from(reference.checked_sub(VAR_REF_BASE)?).ok()?;
        self.var_refs.get(index)
    }

    /// A variável de topo e o nó no caminho.
    fn var_node(
        &self,
        path: &VarPath,
    ) -> Option<(pawnpro_dbg_protocol::Var, pawnpro_dbg_protocol::Var)> {
        let root = self.frames.vars(path.frame).get(path.var)?.clone();
        let node = crate::expr::descend(&root, &path.path)?.clone();
        Some((root, node))
    }

    fn on_launch(&mut self, req: &Request) -> Vec<Outgoing> {
        // `arguments.program` = caminho do `.amx` compilado com `-d3`; dele
        // extraímos o bloco de debug (para mapear linha↔endereço).
        let amx_path = req
            .arguments
            .get("program")
            .or_else(|| req.arguments.get("debugInfo"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let warning = self.load_debug(&amx_path);
        // Origem-fonte padrão: o `.pwn` ao lado do `.amx`. Garante que o
        // `stackTrace` ancore o frame a um arquivo mesmo sem breakpoints (ex.: ao
        // pausar num erro de runtime). Um `setBreakpoints` posterior, se vier,
        // sobrescreve com o caminho exato que o editor conhece.
        if self.source_path.is_none() && amx_path.to_ascii_lowercase().ends_with(".amx") {
            self.source_path = Some(format!("{}.pwn", &amx_path[..amx_path.len() - 4]));
        }
        let seq = self.next_seq();

        // Sem o comando do servidor não há o que depurar: o plugin só alcança
        // o núcleo pelo canal que a subida do servidor reserva.
        let Some(spec) = spawn_spec_from(&req.arguments, amx_path) else {
            let detail = messages::msg(self.locale, MsgKey::LaunchWithoutServer);
            return vec![Outgoing::Response(Response::fail(seq, req, detail))];
        };
        self.spawn_spec = Some(spec.clone());
        warning
            .into_iter()
            .chain([
                Outgoing::SpawnServer(spec),
                Outgoing::Response(Response::ok(seq, req, Value::Null)),
            ])
            .collect()
    }

    /// Resolve todos os breakpoints de linha pedidos contra o bloco de debug
    /// atual, substituindo os endereços anteriores.
    ///
    /// Roda no `setBreakpoints` e de novo no `restart`: o endereço depende do
    /// `.amx`, e recompilar o gamemode muda o mapa linha↔endereço. Sem
    /// reresolver, os endereços antigos apontariam para instruções erradas —
    /// a VM pararia no lugar errado, ou em lugar nenhum.
    ///
    /// Devolve o `verified` de cada breakpoint, na ordem em que foram pedidos.
    fn resolve_line_breakpoints(&mut self) -> Vec<Value> {
        self.breakpoints.clear();
        let mut verified = Vec::new();
        let mut line_bps = Vec::new();

        for (path, requested) in self.requested_bps.clone() {
            let file = if path.is_empty() {
                None
            } else {
                Some(path.as_str())
            };
            // Linhas do editor são as do arquivo atual; o mapa do `.amx` é o da
            // compilação. A vista diz como passar de uma para a outra.
            let view = self.view(&path);
            // O mapa do `.amx` conhece o arquivo pelo nome da tabela dele.
            let table_name = file.map(|path| self.sources.table_name(path).unwrap_or(path));
            for ReqBp {
                line,
                condition,
                hit_condition,
                log_message,
            } in requested
            {
                let Some(compiled) = view.to_compiled(line) else {
                    // Linha nova ou alterada: não há código dela no binário, e
                    // resolvê-la por número pararia em outro código.
                    let message = messages::msg(self.locale, MsgKey::BreakpointNotCompiled);
                    verified.push(json!({ "verified": false, "line": line, "message": message }));
                    continue;
                };
                // A linha pedida pode não ser "quebrável": o endereço desliza
                // para a próxima linha executável. A linha onde a VM de fato
                // vai parar volta ao editor, na numeração atual.
                let resolved = self
                    .dbg
                    .as_ref()
                    .and_then(|d| d.line_to_address(compiled, table_name))
                    .and_then(|addr| {
                        let landed = self.dbg.as_ref()?.lookup_line(addr)?;
                        Some((addr, landed))
                    });
                let Some((addr, landed)) = resolved else {
                    verified.push(json!({ "verified": false, "line": line }));
                    continue;
                };
                // Deslizou para uma linha que não existe mais no arquivo atual:
                // a parada seria em código que o editor não mostra.
                let Some(current) = view.to_current(landed) else {
                    let message = messages::msg(self.locale, MsgKey::BreakpointNotCompiled);
                    verified.push(json!({ "verified": false, "line": line, "message": message }));
                    continue;
                };
                self.breakpoints.push((line, addr));
                line_bps.push(Breakpoint {
                    addr,
                    condition,
                    hit_condition,
                    log_message,
                });
                verified.push(json!({ "verified": true, "line": current }));
            }
        }

        self.line_bps = line_bps;
        verified
    }

    fn on_set_breakpoints(&mut self, req: &Request) -> Vec<Outgoing> {
        let file = req
            .arguments
            .get("source")
            .and_then(|s| s.get("path"))
            .and_then(Value::as_str);
        // Guarda o caminho-fonte que o editor conhece, para o `stackTrace` poder
        // devolver um frame ancorado neste arquivo.
        if let Some(p) = file {
            self.source_path = Some(p.to_string());
        }
        // Cada breakpoint pode trazer `line`, `condition` (expressão), `hitCondition`
        // (contagem) e `logMessage` (logpoint). Todos opcionais; strings vazias
        // viram `None`.
        let str_opt = |b: &Value, key: &str| {
            b.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let requested: Vec<ReqBp> = req
            .arguments
            .get("breakpoints")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| {
                        let line = i32::try_from(b.get("line").and_then(Value::as_i64)?).ok()?;
                        // logMessage NÃO é trimado para `None` por espaços internos,
                        // mas vazio total vira None (não é logpoint).
                        let log_message = b
                            .get("logMessage")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        Some(ReqBp {
                            line,
                            condition: str_opt(b, "condition"),
                            hit_condition: str_opt(b, "hitCondition"),
                            log_message,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Guarda o pedido como veio: é o que permite resolver tudo de novo se o
        // `.amx` mudar (recompilação + restart).
        let key = file.unwrap_or_default().to_string();
        self.requested_bps.retain(|(f, _)| f != &key);
        self.requested_bps.push((key, requested));

        let verified = self.resolve_line_breakpoints();
        let breakpoints = self.all_breakpoints();
        let body = json!({ "breakpoints": verified });
        self.reply_with(req, Command::SetBreakpoints { breakpoints }, body)
    }

    /// União dos breakpoints de linha e de função — o plugin mantém um conjunto só.
    fn all_breakpoints(&self) -> Vec<Breakpoint> {
        self.line_bps
            .iter()
            .chain(self.fn_bps.iter())
            .cloned()
            .collect()
    }

    /// `setFunctionBreakpoints`: substitui os breakpoints de FUNÇÃO. Cada `name` é
    /// resolvido no endereço de entrada da função (via `AmxDbg::function_address`)
    /// e entra na união enviada ao plugin. Responde verificado por breakpoint.
    fn on_set_function_breakpoints(&mut self, req: &Request) -> Vec<Outgoing> {
        let names: Vec<String> = req
            .arguments
            .get("breakpoints")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| b.get("name").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let mut fn_bps = Vec::new();
        let mut verified = Vec::new();
        for name in names {
            let addr = self.dbg.as_ref().and_then(|d| d.function_address(&name));
            if let Some(a) = addr {
                fn_bps.push(Breakpoint {
                    addr: a,
                    condition: None,
                    hit_condition: None,
                    log_message: None,
                });
            }
            // A linha compilada vira a atual; sem arquivo conhecido, fica como
            // está.
            let line = addr
                .and_then(|a| self.dbg.as_ref().and_then(|d| d.lookup_line(a)))
                .map(|compiled| {
                    // A função pode estar num include: a linha é a do arquivo dela.
                    let file = addr.and_then(|a| self.dbg.as_ref()?.lookup_file(a));
                    let path = file.map_or_else(
                        || self.source_path.clone().unwrap_or_default(),
                        |name| self.editor_path(name),
                    );
                    self.view(&path).to_current(compiled).unwrap_or(compiled)
                });
            verified.push(json!({ "verified": addr.is_some(), "line": line }));
        }

        self.fn_bps = fn_bps;
        let breakpoints = self.all_breakpoints();
        let body = json!({ "breakpoints": verified });
        self.reply_with(req, Command::SetBreakpoints { breakpoints }, body)
    }

    /// `configurationDone`: o editor terminou de configurar a sessão.
    ///
    /// Reenvia os breakpoints e sinaliza `Configured`, que libera a VM segura
    /// na carga — assim breakpoints em `OnGameModeInit` e afins são pegos. O
    /// editor manda `setBreakpoints` **antes** do `launch`, quando ainda não há
    /// canal com o plugin — e comando sem canal é descartado. Este é o ponto do protocolo
    /// em que tudo já foi configurado e o servidor já está subindo, então é aqui
    /// que o conjunto real de breakpoints tem de chegar ao plugin.
    fn on_configuration_done(&mut self, req: &Request) -> Vec<Outgoing> {
        let seq = self.next_seq();
        vec![
            Outgoing::ToPlugin(Command::SetBreakpoints {
                breakpoints: self.all_breakpoints(),
            }),
            Outgoing::ToPlugin(Command::Configured),
            Outgoing::Response(Response::ok(seq, req, Value::Null)),
        ]
    }

    /// `continue`: retoma a VM (manda `Continue` ao plugin).
    fn on_continue(&mut self, req: &Request) -> Vec<Outgoing> {
        self.reply_with(
            req,
            Command::Continue,
            json!({ "allThreadsContinued": true }),
        )
    }

    /// `next`/`stepIn`/`stepOut`: manda o step correspondente ao plugin.
    fn on_step(&mut self, req: &Request, mode: Step) -> Vec<Outgoing> {
        self.reply_with(req, Command::Step { mode }, Value::Null)
    }

    /// `stackTrace`: a pilha de chamadas completa da última pausa (frame 0 = topo).
    /// Cada frame carrega o nome da função, a linha-fonte e um `source` apontando ao
    /// arquivo — sem isso o editor mostra "Origem Desconhecida" e não destaca a
    /// linha. O `id` (1-based) identifica o frame nos `scopes`/`variables`/`evaluate`
    /// seguintes. Antes da primeira pausa (sem frames), devolve um frame-âncora só
    /// para o editor ter a fonte.
    /// O caminho, no editor, do arquivo `name` do bloco de debug. Se é o
    /// arquivo que o editor já conhece, vale o caminho dele, exatamente como
    /// veio; senão, o resolvido a partir da pasta da compilação.
    fn editor_path(&self, name: &str) -> String {
        match &self.source_path {
            Some(known) if crate::sources::file_name_matches(name, known) => known.clone(),
            _ => self.sources.disk_path(name),
        }
    }

    /// `stackTrace`: a pilha de chamadas completa da última pausa (frame 0 = topo).
    /// Cada frame aponta o próprio arquivo — um frame pode estar num include — e a
    /// linha na numeração atual dele. O `id` (1-based) identifica o frame nos
    /// `scopes`/`variables`/`evaluate` seguintes. Antes da primeira pausa (sem
    /// frames), devolve um frame-âncora só para o editor ter a fonte.
    fn on_stack_trace(&mut self, req: &Request) -> Vec<Outgoing> {
        // Referências de array valem para uma pausa: o editor pede a pilha de
        // novo a cada parada, antes de pedir variáveis.
        self.var_refs.clear();
        let frames = self.frames.all();
        let stack_frames: Vec<Value> = if frames.is_empty() {
            // Sem pausa ainda: frame-âncora para ancorar a fonte no editor.
            vec![with_source(
                json!({ "id": 1, "name": "main", "line": 0, "column": 0 }),
                self.source_path.as_deref(),
            )]
        } else {
            frames
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let path = f
                        .file
                        .as_deref()
                        .map(|name| self.editor_path(name))
                        .or_else(|| self.source_path.clone());
                    let compiled = f.line.unwrap_or(0);
                    // As linhas da pausa são as da compilação; o editor mostra o
                    // arquivo atual.
                    let current = f
                        .line
                        .map(|l| self.view(path.as_deref().unwrap_or_default()).to_current(l));
                    match current {
                        // A linha foi alterada ou apagada: apontar o arquivo
                        // destacaria outra linha no editor. O frame fica sem
                        // fonte e diz o que houve.
                        Some(None) => json!({
                            "id": i + 1,
                            "name": messages::format(
                                self.locale,
                                MsgKey::FrameLineChanged,
                                &[&f.name, &compiled.to_string()],
                            ),
                            "line": 0,
                            "column": 0,
                        }),
                        current => with_source(
                            json!({
                                "id": i + 1,
                                "name": f.name,
                                "line": current.flatten().unwrap_or(compiled),
                                "column": 0,
                            }),
                            path.as_deref(),
                        ),
                    }
                })
                .collect()
        };
        let total = stack_frames.len();
        let body = json!({ "stackFrames": stack_frames, "totalFrames": total });
        self.reply(req, body)
    }

    /// `scopes`: um escopo "Locais" por frame. O `frameId` (vindo do `stackTrace`)
    /// vira o `variablesReference` do escopo, para o `variables` seguinte saber de
    /// qual frame ler.
    fn on_scopes(&mut self, req: &Request) -> Vec<Outgoing> {
        let frame_id = req
            .arguments
            .get("frameId")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let body = json!({
            "scopes": [ { "name": "Locais", "variablesReference": frame_id, "expensive": false } ]
        });
        self.reply(req, body)
    }

    /// `variables`: variáveis do container referenciado. Um array ou sub-array
    /// (referência registrada nesta pausa) devolve seus elementos; senão é um
    /// escopo de frame (`ref - 1`) e devolve as variáveis de topo. Tudo que tem
    /// filhos ganha uma referência própria, para o editor expandir.
    fn on_variables(&mut self, req: &Request) -> Vec<Outgoing> {
        let reference = req
            .arguments
            .get("variablesReference")
            .and_then(Value::as_i64)
            .unwrap_or(0);

        let mut vars = Vec::new();
        if let Some(parent) = self.var_path(reference).cloned() {
            if let Some((root, node)) = self.var_node(&parent) {
                for child in &node.children {
                    let Some(i) = parse_elem_index(&child.name) else {
                        continue;
                    };
                    let path = VarPath {
                        path: [parent.path.as_slice(), &[i]].concat(),
                        ..parent.clone()
                    };
                    let child_ref = if child.children.is_empty() {
                        0
                    } else {
                        self.var_ref(path.clone())
                    };
                    vars.push(json!({
                        "name": child.name,
                        "value": child.value,
                        "variablesReference": child_ref,
                        "memoryReference": data_id(path.frame, &root.name, &path.path),
                    }));
                }
            }
        } else {
            let frame = frame_index(req.arguments.get("variablesReference"));
            for (var, v) in self.frames.vars(frame).iter().enumerate() {
                let child_ref = if v.children.is_empty() {
                    0
                } else {
                    self.var_ref(VarPath {
                        frame,
                        var,
                        path: Vec::new(),
                    })
                };
                vars.push(json!({
                    "name": v.name,
                    "value": v.value,
                    "variablesReference": child_ref,
                    "memoryReference": data_id(frame, &v.name, &[]),
                }));
            }
        }
        let body = json!({ "variables": vars });
        self.reply(req, body)
    }

    /// O caminho completo de um filho `[i]` do container `reference`, com a
    /// variável de topo. `None` se a referência não é de array, o nome não é
    /// um índice ou o filho não é uma célula (tem filhos próprios).
    fn element(&self, reference: i64, name: &str) -> Option<(VarPath, pawnpro_dbg_protocol::Var)> {
        let parent = self.var_path(reference)?;
        let path = VarPath {
            path: [parent.path.as_slice(), &[parse_elem_index(name)?]].concat(),
            ..parent.clone()
        };
        let (root, leaf) = self.var_node(&path)?;
        leaf.children.is_empty().then_some((path, root))
    }

    /// `setVariable`: edita uma variável no painel Variáveis durante a pausa. O
    /// novo valor (`value`) é um inteiro (decimal ou `0x` hex). Encaminha ao
    /// plugin, que escreve na célula da VM, e responde ao editor com o valor.
    fn on_set_variable(&mut self, req: &Request) -> Vec<Outgoing> {
        let name = req
            .arguments
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let raw = req
            .arguments
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let reference = req
            .arguments
            .get("variablesReference")
            .and_then(Value::as_i64)
            .unwrap_or(0);

        // Aceita inteiro (decimal/hex), float (`50.0`) e bool (`true`/`false`). O
        // valor enviado ao plugin é sempre uma célula i32 (float = bits IEEE-754,
        // bool = 0/1); `shown` é o texto amigável que volta para o painel.
        let seq = self.next_seq();
        let Some((value, shown)) = parse_set_value(&raw) else {
            let detail = messages::format(self.locale, MsgKey::InvalidValue, &[&raw]);
            return vec![Outgoing::Response(Response::fail(seq, req, detail))];
        };

        // Elemento de array: o `variablesReference` é o do container e o `name`
        // é `[i]`.
        if self.var_path(reference).is_some() {
            let Some((path, root)) = self.element(reference, &name) else {
                let detail = messages::format(self.locale, MsgKey::InvalidElement, &[&name]);
                return vec![Outgoing::Response(Response::fail(seq, req, detail))];
            };
            return vec![Outgoing::WriteVariable(Write {
                seq,
                frame: path.frame,
                name: root.name,
                path: path.path,
                value,
                shown,
                var: path.var,
            })];
        }

        // Escalar: o `variablesReference` é o escopo do frame (== frameId 1-based).
        let frame = frame_index(req.arguments.get("variablesReference"));
        // O array inteiro não é editável — o editor deve editar um elemento.
        let vars = self.frames.vars(frame);
        let Some(var) = vars.iter().position(|v| v.name == name) else {
            let detail = messages::format(self.locale, MsgKey::CannotEvaluate, &[&name]);
            return vec![Outgoing::Response(Response::fail(seq, req, detail))];
        };
        if !vars[var].children.is_empty() {
            let detail = messages::format(self.locale, MsgKey::ArrayEditElement, &[&name, &name]);
            return vec![Outgoing::Response(Response::fail(seq, req, detail))];
        }

        vec![Outgoing::WriteVariable(Write {
            seq,
            frame,
            name,
            path: Vec::new(),
            value,
            shown,
            var,
        })]
    }

    /// `setExpression`: edita um lvalue (`name`, `arr[i]` ou `arr[i][j]`) no
    /// watch/console. Os índices podem ser subexpressões. Encaminha ao plugin
    /// como `SetVariable`.
    fn on_set_expression(&mut self, req: &Request) -> Vec<Outgoing> {
        let expr = req
            .arguments
            .get("expression")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let raw = req
            .arguments
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let frame = req
            .arguments
            .get("frameId")
            .and_then(Value::as_i64)
            .and_then(|id| usize::try_from(id - 1).ok())
            .unwrap_or(0);

        let seq = self.next_seq();
        let Some((value, shown)) = parse_set_value(&raw) else {
            let detail = messages::format(self.locale, MsgKey::InvalidValue, &[&raw]);
            return vec![Outgoing::Response(Response::fail(seq, req, detail))];
        };
        let vars = self.frames.vars(frame);
        let Some((name, path)) = parse_lvalue(&expr, &vars) else {
            let detail = messages::format(self.locale, MsgKey::CannotEvaluate, &[&expr]);
            return vec![Outgoing::Response(Response::fail(seq, req, detail))];
        };

        // Nome que não está no frame não existe para o plugin: recusar aqui dá o
        // motivo certo, em vez de uma falha de escrita genérica.
        let Some(var) = vars.iter().position(|v| v.name == name) else {
            let detail = messages::format(self.locale, MsgKey::CannotEvaluate, &[&expr]);
            return vec![Outgoing::Response(Response::fail(seq, req, detail))];
        };
        vec![Outgoing::WriteVariable(Write {
            seq,
            frame,
            name,
            path,
            value,
            shown,
            var,
        })]
    }

    /// `dataBreakpointInfo`: o editor pergunta se dá para observar mudanças na
    /// variável `name` do container `variablesReference`. Respondemos um `dataId`
    /// opaco (`frame:name` ou `frame:name:i.j`) que o `setDataBreakpoints`
    /// seguinte reusa; `dataId: null` recusa. Só células são observáveis: um
    /// escalar ou um elemento da última dimensão — um array ou uma linha
    /// inteira não mudam de valor.
    fn on_data_breakpoint_info(&mut self, req: &Request) -> Vec<Outgoing> {
        let reference = req
            .arguments
            .get("variablesReference")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let name = req
            .arguments
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let resolved = if self.var_path(reference).is_some() {
            self.element(reference, &name).map(|(path, root)| {
                let description = path
                    .path
                    .iter()
                    .fold(root.name.clone(), |d, i| format!("{d}[{i}]"));
                (data_id(path.frame, &root.name, &path.path), description)
            })
        } else {
            let frame = frame_index(req.arguments.get("variablesReference"));
            self.frames
                .vars(frame)
                .iter()
                .any(|v| v.name == name && v.children.is_empty())
                .then(|| (data_id(frame, &name, &[]), name.clone()))
        };

        let body = if let Some((data_id, description)) = resolved {
            json!({
                "dataId": data_id,
                "description": description,
                "accessTypes": ["write"],
                "canPersist": false,
            })
        } else {
            json!({ "dataId": Value::Null, "description": name })
        };
        self.reply(req, body)
    }

    /// `setDataBreakpoints`: substitui o conjunto de data breakpoints. Decodifica
    /// cada `dataId` (`"frame:name"`) de volta em frame + nome e encaminha ao
    /// plugin, que resolve o endereço e passa a observar. Responde verificado.
    fn on_set_data_breakpoints(&mut self, req: &Request) -> Vec<Outgoing> {
        let watches: Vec<DataWatch> = req
            .arguments
            .get("breakpoints")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| parse_data_id(b.get("dataId")?.as_str()?))
                    .collect()
            })
            .unwrap_or_default();

        let verified: Vec<Value> = watches
            .iter()
            .map(|_| json!({ "verified": true }))
            .collect();
        let body = json!({ "breakpoints": verified });
        self.reply_with(req, Command::SetDataBreakpoints { watches }, body)
    }

    /// `setExceptionBreakpoints`: o editor envia os filtros ativos. Ligamos a
    /// pausa em erros de runtime se o filtro `runtime` estiver na lista; senão a
    /// desligamos (a VM aborta normalmente).
    fn on_set_exception_breakpoints(&mut self, req: &Request) -> Vec<Outgoing> {
        let runtime = req
            .arguments
            .get("filters")
            .and_then(Value::as_array)
            .is_some_and(|fs| fs.iter().any(|f| f.as_str() == Some("runtime")));
        self.reply_with(req, Command::SetExceptionFilter { runtime }, Value::Null)
    }

    /// `readMemory`: lê memória de dados crua a partir do `memoryReference` de uma
    /// variável (`frame:name` ou `frame:name:index`, montado em `variables`). A
    /// leitura em si (bloqueante, no plugin) é feita pelo laço via
    /// [`Outgoing::ReadMemory`].
    fn on_read_memory(&mut self, req: &Request) -> Vec<Outgoing> {
        let mem_ref = req
            .arguments
            .get("memoryReference")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let offset = req
            .arguments
            .get("offset")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let count = req
            .arguments
            .get("count")
            .and_then(Value::as_i64)
            .and_then(|c| usize::try_from(c).ok())
            .unwrap_or(0);

        let seq = self.next_seq();
        let Some(DataWatch { frame, name, path }) = parse_data_id(&mem_ref) else {
            return vec![Outgoing::Response(Response::fail(
                seq,
                req,
                format!("memoryReference inválido: '{mem_ref}'"),
            ))];
        };
        vec![Outgoing::ReadMemory {
            seq,
            address: mem_ref,
            frame,
            name,
            path,
            offset,
            count,
        }]
    }

    /// `completions`: autocomplete no watch/console. Sugere as variáveis em escopo
    /// no frame cujos nomes começam com o "pedaço" já digitado (o identificador
    /// antes do cursor). Sem prefixo, sugere todas.
    fn on_completions(&mut self, req: &Request) -> Vec<Outgoing> {
        let frame = req
            .arguments
            .get("frameId")
            .and_then(Value::as_i64)
            .and_then(|id| usize::try_from(id - 1).ok())
            .unwrap_or(0);
        let text = req
            .arguments
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("");
        let column = req
            .arguments
            .get("column")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let prefix = word_prefix(text, column);

        let targets: Vec<Value> = self
            .frames
            .vars(frame)
            .into_iter()
            .filter(|v| prefix.is_empty() || v.name.starts_with(&prefix))
            .map(|v| json!({ "label": v.name, "type": "variable" }))
            .collect();
        self.reply(req, json!({ "targets": targets }))
    }

    /// `evaluate`: painel INSPEÇÃO (watch) e hover. Avalia a expressão com o
    /// [`crate::expr`] contra as variáveis do frame: nome, literal, `arr[i]`, ou
    /// `A OP B` (aritmética/comparação). O que não avaliar vira falha explícita
    /// (o DAP exige falha para o editor mostrar "não disponível", não um valor falso).
    fn on_evaluate(&mut self, req: &Request) -> Vec<Outgoing> {
        let expr = req
            .arguments
            .get("expression")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        // O `frameId` (1-based, do `stackTrace`) escolhe o escopo; ausente (ex.:
        // console global) cai no frame do topo.
        let frame = req
            .arguments
            .get("frameId")
            .and_then(Value::as_i64)
            .and_then(|id| usize::try_from(id - 1).ok())
            .unwrap_or(0);

        let seq = self.next_seq();
        if let Some(result) = crate::expr::eval(expr, &self.frames.vars(frame)) {
            let body = json!({ "result": result, "variablesReference": 0 });
            vec![Outgoing::Response(Response::ok(seq, req, body))]
        } else {
            let detail = if expr.is_empty() {
                messages::format(self.locale, MsgKey::EmptyExpression, &[])
            } else {
                messages::format(self.locale, MsgKey::CannotEvaluate, &[expr])
            };
            vec![Outgoing::Response(Response::fail(seq, req, detail))]
        }
    }

    fn on_threads(&mut self, req: &Request) -> Vec<Outgoing> {
        // Uma única "thread" lógica — o servidor Pawn roda um VM.
        let body = json!({ "threads": [ { "id": 1, "name": "main" } ] });
        self.reply(req, body)
    }

    /// O evento `terminated`, uma vez por sessão.
    const fn terminated_event(&mut self) -> Option<Outgoing> {
        if self.terminated_sent {
            return None;
        }
        self.terminated_sent = true;
        let seq = self.next_seq();
        Some(Outgoing::Event(Event::new(seq, "terminated", Value::Null)))
    }

    /// `terminate`: encerra o programa depurado — o servidor —, mas não a
    /// sessão. O editor manda `disconnect` logo depois do `terminated`, e esse
    /// pedido precisa de resposta: fechar a conexão aqui o deixaria esperando
    /// o prazo esgotar, com "Parando" na tela.
    fn on_terminate(&mut self, req: &Request) -> Vec<Outgoing> {
        let seq = self.next_seq();
        let mut out = vec![
            Outgoing::StopServer,
            Outgoing::Response(Response::ok(seq, req, Value::Null)),
        ];
        out.extend(self.terminated_event());
        out
    }

    /// `disconnect`: encerra a sessão. O servidor cai junto com ela, então
    /// responder e sair basta.
    fn on_disconnect(&mut self, req: &Request) -> Vec<Outgoing> {
        self.terminated = true;
        let seq = self.next_seq();
        let mut out = vec![Outgoing::Response(Response::ok(seq, req, Value::Null))];
        out.extend(self.terminated_event());
        out
    }

    /// Reinicia o servidor **mantendo a sessão de depuração viva**.
    ///
    /// Antes isto encerrava a sessão (`terminated` com `restart: true`) e
    /// deixava o editor recriar tudo — o que derrubava a depuração a cada
    /// recarga do gamemode, justamente o ciclo mais comum de quem depura.
    ///
    /// O estado que vale continua na sessão — breakpoints de linha, de função
    /// e o bloco de debug — então nada precisa ser reenviado pelo editor.
    ///
    /// Sem `spawn_spec` o `launch` falhou, e não há servidor a reiniciar.
    fn on_restart(&mut self, req: &Request) -> Vec<Outgoing> {
        let Some(spec) = self.spawn_spec.clone() else {
            let seq = self.next_seq();
            let detail = messages::msg(self.locale, MsgKey::LaunchWithoutServer);
            return vec![Outgoing::Response(Response::fail(seq, req, detail))];
        };

        // Fonte mais novo que o binário: recompilar é atribuição da extensão
        // (o compilador e as flags são conhecimento dela), então pedimos por
        // evento e paramos aqui — ela compila e reenvia o `restart`. A
        // verificação fica NESTE ponto porque é por onde todo restart passa,
        // venha do botão nativo do editor ou do comando do PawnPro.
        if !self.rebuild_requested && needs_rebuild(&spec.amx_path) {
            self.rebuild_requested = true;
            let seq = self.next_seq();
            let ev_seq = self.next_seq();
            return vec![
                Outgoing::Response(Response::ok(seq, req, Value::Null)),
                Outgoing::Event(Event::new(
                    ev_seq,
                    "pawnproRebuild",
                    json!({ "program": spec.amx_path }),
                )),
            ];
        }
        // Chegou aqui: ou o binário está em dia, ou a extensão acabou de
        // recompilar. O ciclo se fecha para o próximo restart.
        self.rebuild_requested = false;

        // O `.amx` pode ter sido recompilado entre a sessão e o restart — é o
        // motivo mais comum para reiniciar. O mapa linha↔endereço muda junto,
        // então recarregar o bloco de debug e reresolver os breakpoints é o que
        // impede a VM de parar no lugar errado.
        let warning = self.load_debug(&spec.amx_path);
        let verified = self.resolve_line_breakpoints();

        let seq = self.next_seq();
        let ev_seq = self.next_seq();
        let bp_seq = self.next_seq();
        // `continued` avisa o editor de que não há mais frame parado: o processo
        // que os produzia acabou de morrer, e os painéis precisam limpar.
        // Os `breakpoint` avisam onde cada marcador ficou depois da recompilação
        // — a linha pode ter andado, ou o breakpoint deixado de ser válido.
        let mut out = vec![
            // O canal antigo morreu com o processo; o laço reserva um novo
            // junto com o servidor.
            Outgoing::SpawnServer(spec),
            // O plugin novo sobe sem breakpoint nenhum: o estado morreu junto
            // com o processo anterior. Reresolver os endereços e avisar o editor
            // não basta — sem isto a VM roda sem saber onde parar, e a execução
            // passa direto pelo breakpoint.
            Outgoing::ToPlugin(Command::SetBreakpoints {
                breakpoints: self.all_breakpoints(),
            }),
            // O plugin bloqueia a VM na carga esperando este sinal (com timeout
            // de 10 s). Sem ele o servidor novo ficaria parado até o timeout, e
            // breakpoints em `OnGameModeInit` e afins se perderiam.
            Outgoing::ToPlugin(Command::Configured),
            Outgoing::Response(Response::ok(seq, req, Value::Null)),
            Outgoing::Event(Event::new(
                ev_seq,
                "continued",
                json!({ "threadId": 1, "allThreadsContinued": true }),
            )),
        ];

        // Um `breakpoint` por marcador, com a linha onde ele de fato ficou.
        for (i, v) in verified.iter().enumerate() {
            let mut body = v.clone();
            if let Some(obj) = body.as_object_mut() {
                obj.insert("id".into(), json!(i + 1));
            }
            out.push(Outgoing::Event(Event::new(
                bp_seq + i64::try_from(i).unwrap_or(0),
                "breakpoint",
                json!({ "reason": "changed", "breakpoint": body }),
            )));
        }
        out.extend(warning);
        out
    }

    /// Breakpoints de linha resolvidos a endereço de código.
    #[cfg(test)]
    #[must_use]
    pub fn resolved_breakpoints(&self) -> &[(i32, u32)] {
        &self.breakpoints
    }

    /// Injeta um bloco de debug diretamente, sem passar pelo `.amx`.
    #[cfg(test)]
    pub fn set_debug(&mut self, dbg: AmxDbg) {
        self.dbg = Some(dbg);
    }

    /// Como o `load_debug`, com o bloco já montado: o `.amx` em disco só
    /// precisa existir para dar a data da compilação.
    #[cfg(test)]
    pub fn set_compiled(&mut self, amx_path: &std::path::Path, dbg: AmxDbg) {
        self.sources = Sources::capture(amx_path, &dbg);
        self.dbg = Some(dbg);
    }
}

/// Monta o [`SpawnSpec`] a partir dos argumentos do `launch`.
///
/// `None` sem `serverCommand.exe`: a extensão sempre o envia, e sem ele não há
/// servidor para depurar.
fn spawn_spec_from(arguments: &Value, amx_path: String) -> Option<SpawnSpec> {
    let cmd = arguments.get("serverCommand")?;
    let exe = cmd
        .get("exe")
        .and_then(Value::as_str)
        .filter(|exe| !exe.is_empty())?
        .to_string();
    let args = cmd
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let text = |value: Option<&Value>| value.and_then(Value::as_str).unwrap_or("").to_string();
    Some(SpawnSpec {
        exe,
        args,
        cwd: text(cmd.get("cwd")),
        amx_path,
        locale: text(arguments.get("locale")),
    })
}

/// Índice do frame (0-based) a partir de um `variablesReference`/`frameId`
/// (1-based, como o `stackTrace`/`scopes` definem). Ausente ou inválido → topo (0).
fn frame_index(reference: Option<&Value>) -> usize {
    reference
        .and_then(Value::as_i64)
        .and_then(|r| usize::try_from(r - 1).ok())
        .unwrap_or(0)
}

/// Base das referências de array — bem acima de qualquer id de frame (escopos
/// de frame são 1..N).
const VAR_REF_BASE: i64 = 1_000_000;

/// Um array ou sub-array expansível: a variável de topo `var` do `frame`, e os
/// índices até o nó (vazio = a própria variável).
#[derive(Debug, Clone, PartialEq, Eq)]
struct VarPath {
    frame: usize,
    var: usize,
    path: Vec<usize>,
}

/// O `dataId`/`memoryReference` de uma célula: `frame:name`, ou
/// `frame:name:i.j` para um elemento. Nomes Pawn não têm `:` nem `.`.
fn data_id(frame: usize, name: &str, path: &[usize]) -> String {
    if path.is_empty() {
        format!("{frame}:{name}")
    } else {
        let indices: Vec<String> = path.iter().map(ToString::to_string).collect();
        format!("{frame}:{name}:{}", indices.join("."))
    }
}

/// Índice de um elemento a partir do nome do filho `"[i]"` (como montado na
/// inspeção). `None` se não casar o formato.
fn parse_elem_index(name: &str) -> Option<usize> {
    name.strip_prefix('[')?.strip_suffix(']')?.parse().ok()
}

/// Interpreta um lvalue (`name`, `name[a]`, `name[a][b]`) para
/// `setExpression`. Os índices podem ser subexpressões, resolvidas por
/// [`crate::expr`] contra as variáveis do frame. `None` se não for um lvalue
/// simples.
fn parse_lvalue(expr: &str, vars: &[pawnpro_dbg_protocol::Var]) -> Option<(String, Vec<usize>)> {
    let expr = expr.trim();
    if let Some((name, indices)) = crate::expr::split_indexed(expr) {
        let path = crate::expr::eval_path(&indices, vars)?;
        return Some((name.to_string(), path));
    }
    (!expr.is_empty() && expr.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then(|| (expr.to_string(), Vec::new()))
}

/// Identificador sendo digitado antes do cursor (`column`, 1-based em `text`) —
/// a corrida final de `[A-Za-z0-9_]`. Usado para filtrar o autocomplete.
fn word_prefix(text: &str, column: i64) -> String {
    let n = usize::try_from(column).unwrap_or(0).saturating_sub(1);
    let typed: String = text.chars().take(n).collect();
    let mut tail: Vec<char> = typed
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    tail.reverse();
    tail.into_iter().collect()
}

/// Decodifica um `dataId` (`frame:name` ou `frame:name:i.j`, montado por
/// [`data_id`]) de volta em um [`DataWatch`].
fn parse_data_id(data_id: &str) -> Option<DataWatch> {
    let mut parts = data_id.splitn(3, ':');
    let frame = parts.next()?.parse().ok()?;
    let name = parts.next()?.to_string();
    let path = match parts.next() {
        Some(indices) => indices
            .split('.')
            .map(|i| i.parse().ok())
            .collect::<Option<Vec<usize>>>()?,
        None => Vec::new(),
    };
    Some(DataWatch { frame, name, path })
}

/// Anexa ao frame o arquivo `path`, se houver, para o editor ancorar a linha.
fn with_source(mut frame: Value, path: Option<&str>) -> Value {
    if let Some(path) = path {
        frame["source"] = json!({
            "name": std::path::Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path),
            "path": path,
        });
    }
    frame
}

/// Interpreta o texto digitado em `setVariable` e devolve `(célula, texto)`:
/// - a **célula** é o `i32` gravado na VM (float → bits IEEE-754; bool → 0/1);
/// - o **texto** é a forma amigável que volta ao painel (`50`, `1.5`, `true`).
///
/// Aceita: `true`/`false`, hex `0x..`, inteiro decimal, e float (`1.5`). `None`
/// se nada casar.
fn parse_set_value(raw: &str) -> Option<(i32, String)> {
    match raw {
        "true" => return Some((1, "true".to_string())),
        "false" => return Some((0, "false".to_string())),
        _ => {}
    }
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))
        && let Ok(i) = i32::from_str_radix(hex, 16)
    {
        return Some((i, i.to_string()));
    }
    if let Ok(i) = raw.parse::<i32>() {
        return Some((i, i.to_string()));
    }
    if let Ok(f) = raw.parse::<f32>() {
        // Grava os bits do float; mostra o float (não os bits). Mantém ao menos
        // uma casa decimal para o painel não exibir um float como se fosse int
        // (ex.: `50.0` viraria `50` com a formatação padrão).
        let shown = if f.fract() == 0.0 {
            format!("{f:.1}")
        } else {
            format!("{f}")
        };
        return Some((f.to_bits().cast_signed(), shown));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_set_value_types() {
        // Inteiro decimal/hex.
        assert_eq!(parse_set_value("100"), Some((100, "100".to_string())));
        assert_eq!(parse_set_value("-5"), Some((-5, "-5".to_string())));
        assert_eq!(parse_set_value("0x64"), Some((100, "100".to_string())));
        // Bool.
        assert_eq!(parse_set_value("true"), Some((1, "true".to_string())));
        assert_eq!(parse_set_value("false"), Some((0, "false".to_string())));
        // Float: célula = bits IEEE-754; texto preserva o `.0`.
        let (cell, shown) = parse_set_value("50.0").unwrap();
        assert_eq!(cell, 50.0f32.to_bits().cast_signed());
        assert_eq!(shown, "50.0");
        let (cell, shown) = parse_set_value("1.5").unwrap();
        assert_eq!(cell, 1.5f32.to_bits().cast_signed());
        assert_eq!(shown, "1.5");
        // Inválido.
        assert_eq!(parse_set_value("abc"), None);
        assert_eq!(parse_set_value(""), None);
    }

    /// Um `launch` com `serverCommand` guarda como o servidor foi iniciado, e é
    /// isso que permite reiniciá-lo depois sem derrubar a sessão.
    #[test]
    fn restart_replaces_server_without_ending_session() {
        let mut ses = Session::new();
        let out = ses.handle(&req(
            "launch",
            &json!({
                "program": "/tmp/gm.amx",
                "serverCommand": { "exe": "/tmp/omp-server", "args": [], "cwd": "/tmp" }
            }),
        ));
        assert!(
            out.iter().any(|o| matches!(o, Outgoing::SpawnServer(_))),
            "o launch deve subir o servidor"
        );

        let out = ses.handle(&req("restart", &json!({})));
        assert!(
            out.iter().any(|o| matches!(o, Outgoing::SpawnServer(_))),
            "o restart deve trocar o servidor"
        );
        assert!(
            !out.iter()
                .any(|o| matches!(o, Outgoing::Event(e) if e.event == "terminated")),
            "o restart NÃO deve encerrar a sessão"
        );
        assert!(!ses.terminated, "a sessão segue viva depois do restart");
    }

    /// Sequência real do editor: `setBreakpoints` chega ANTES do `launch`
    /// (o editor manda os breakpoints já na configuração da sessão). Se o
    /// pedido não sobreviver ao launch, nenhum breakpoint funciona.
    #[test]
    fn breakpoints_requested_before_launch_survive() {
        let mut ses = Session::new();
        ses.set_debug(sample_dbg());
        ses.handle(&req(
            "setBreakpoints",
            &json!({ "source": { "path": "a.pwn" }, "breakpoints": [ { "line": 4 } ] }),
        ));
        assert_eq!(
            ses.resolved_breakpoints(),
            &[(4, 20)],
            "resolveu antes do launch"
        );

        // O launch chega depois e recarrega o bloco de debug do `.amx`.
        ses.handle(&req("launch", &json!({ "program": "/tmp/missing.amx" })));
        assert_eq!(
            ses.resolved_breakpoints(),
            &[(4, 20)],
            "o launch não pode descartar os breakpoints já resolvidos"
        );
    }

    /// O editor manda `setBreakpoints` antes do `launch`, quando ainda não há
    /// canal com o plugin — e comando sem canal é descartado. O
    /// `configurationDone` precisa reenviar, senão nenhum breakpoint funciona.
    #[test]
    fn configuration_done_resends_breakpoints() {
        let mut ses = Session::new();
        ses.set_debug(sample_dbg());
        ses.handle(&req(
            "setBreakpoints",
            &json!({ "source": { "path": "a.pwn" }, "breakpoints": [ { "line": 4 } ] }),
        ));

        let out = ses.handle(&req("configurationDone", &json!({})));
        assert!(
            has_command(
                &out,
                |c| matches!(c, Command::SetBreakpoints { breakpoints }
                if breakpoints.len() == 1 && breakpoints[0].addr == 20)
            ),
            "o configurationDone deve reenviar o conjunto real ao plugin"
        );
        assert!(
            has_command(&out, |c| matches!(c, Command::Configured)),
            "e liberar a VM depois"
        );
    }

    /// Recompilar o gamemode muda o mapa linha↔endereço. O restart tem de
    /// reresolver os breakpoints contra o `.amx` novo — senão os endereços
    /// antigos apontariam para instruções erradas.
    #[test]
    fn restart_resolves_breakpoints_again() {
        // O `.amx` em disco precisa ter o bloco de debug: sem ele, o restart
        // descarta o mapa em vez de resolver com o antigo.
        let dir = std::env::temp_dir().join(format!("pawnpro-reresolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pwn = dir.join("a.pwn");
        std::fs::write(&pwn, "x\ny\nfoo();\nbar();\n").unwrap();
        set_mtime(&pwn, 1_000);
        let amx = dir.join("gm.amx");
        std::fs::write(&amx, dbg_raw("a.pwn", 0, |_| {})).unwrap();
        set_mtime(&amx, 2_000);
        let source = pwn.to_string_lossy().into_owned();
        let mut ses = Session::new();
        ses.handle(&req(
            "launch",
            &json!({
                "program": amx.to_str().unwrap(),
                "serverCommand": { "exe": "/tmp/omp-server", "args": [], "cwd": "/tmp" }
            }),
        ));
        ses.set_debug(sample_dbg());
        ses.handle(&req(
            "setBreakpoints",
            &json!({ "source": { "path": source }, "breakpoints": [ { "line": 4 } ] }),
        ));
        assert_eq!(
            ses.resolved_breakpoints(),
            &[(4, 20)],
            "o setBreakpoints resolve contra o mapa atual"
        );

        // O restart limpa e resolve de novo, a partir do pedido guardado.
        let out = ses.handle(&req("restart", &json!({})));
        assert_eq!(
            ses.resolved_breakpoints(),
            &[(4, 20)],
            "o breakpoint continua valendo depois do restart"
        );
        assert!(
            out.iter()
                .any(|o| matches!(o, Outgoing::Event(e) if e.event == "breakpoint")),
            "o editor precisa saber onde cada marcador ficou"
        );
        // Reresolver e avisar o editor não basta: o servidor novo traz um plugin
        // novo, sem breakpoint nenhum, e num canal novo. Sem subir com canal
        // novo e reenviar, a VM roda sem saber onde parar e a execução passa
        // direto.
        assert!(
            out.iter().any(|o| matches!(o, Outgoing::SpawnServer(_))),
            "o canal antigo morreu com o processo: o restart precisa de um novo"
        );
        assert!(
            has_command(
                &out,
                |c| matches!(c, Command::SetBreakpoints { breakpoints }
                if breakpoints.len() == 1 && breakpoints[0].addr == 20)
            ),
            "e reenviar os breakpoints ao plugin do servidor novo"
        );
        assert!(
            has_command(&out, |c| matches!(c, Command::Configured)),
            "e liberar a VM, que fica bloqueada na carga esperando o sinal"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Fonte mais novo que o binário: o restart pede a recompilação à extensão
    /// em vez de subir o servidor com o `.amx` velho — os breakpoints se
    /// resolveriam contra o mapa antigo, e a VM pararia no lugar errado.
    #[test]
    fn restart_requests_rebuild_when_source_changed() {
        // Um diretório por processo: dois `cargo test` ao mesmo tempo não podem
        // dividir os mesmos arquivos.
        let dir = std::env::temp_dir().join(format!("pawnpro-rebuild-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let amx = dir.join("gm.amx");
        let pwn = dir.join("gm.pwn");
        std::fs::write(&amx, b"binary").unwrap();
        // O fonte é escrito depois, então é o mais novo.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&pwn, b"source").unwrap();

        let mut ses = Session::new();
        ses.handle(&req(
            "launch",
            &json!({
                "program": amx.to_str().unwrap(),
                "serverCommand": { "exe": "/tmp/omp-server", "args": [], "cwd": "/tmp" }
            }),
        ));

        let out = ses.handle(&req("restart", &json!({})));
        assert!(
            out.iter()
                .any(|o| matches!(o, Outgoing::Event(e) if e.event == "pawnproRebuild")),
            "o fonte é mais novo: tem de pedir a recompilação"
        );
        assert!(
            !out.iter().any(|o| matches!(o, Outgoing::SpawnServer(_))),
            "e NÃO subir o servidor com o binário velho"
        );

        // A extensão compila e reenvia: agora o restart segue normalmente, sem
        // pedir de novo — senão seria laço.
        let out = ses.handle(&req("restart", &json!({})));
        assert!(
            out.iter().any(|o| matches!(o, Outgoing::SpawnServer(_))),
            "no reenvio o servidor sobe"
        );
        assert!(
            !out.iter()
                .any(|o| matches!(o, Outgoing::Event(e) if e.event == "pawnproRebuild")),
            "e não pede rebuild de novo"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// O comando Compilar comum gera o `.amx` de produção, sem `-d3`, e mais
    /// novo que o `.pwn`. Pela data, o restart subiria esse binário: sem
    /// breakpoints, sem variáveis, e sem dizer por quê.
    #[test]
    fn restart_rebuilds_a_binary_without_debug_info() {
        let dir = std::env::temp_dir().join(format!("pawnpro-nodebug-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (amx, pwn) = (dir.join("gm.amx"), dir.join("gm.pwn"));
        std::fs::write(&pwn, b"main() {}").unwrap();
        set_mtime(&pwn, 1_000);
        std::fs::write(&amx, b"sem bloco de debug").unwrap();
        set_mtime(&amx, 2_000);

        let mut ses = Session::new();
        ses.handle(&req(
            "launch",
            &json!({
                "program": amx.to_str().unwrap(),
                "serverCommand": { "exe": "/tmp/omp-server", "args": [], "cwd": "/tmp" }
            }),
        ));
        let out = ses.handle(&req("restart", &json!({})));
        std::fs::remove_dir_all(&dir).ok();
        assert!(has_event(&out, "pawnproRebuild"), "tem de pedir o -d3");
        assert!(!out.iter().any(|o| matches!(o, Outgoing::SpawnServer(_))));
    }

    /// Sem `.pwn` para recompilar, o binário sem debug sobe como está — mas o
    /// mapa da sessão anterior não pode continuar valendo para ele.
    #[test]
    fn binary_without_debug_info_drops_the_old_map_and_warns() {
        let dir = std::env::temp_dir().join(format!("pawnpro-dropmap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let amx = dir.join("gm.amx");
        std::fs::write(&amx, b"sem bloco de debug").unwrap();

        let mut ses = Session::new();
        ses.set_debug(sample_dbg());
        let out = ses.handle(&req(
            "launch",
            &json!({
                "program": amx.to_str().unwrap(),
                "serverCommand": { "exe": "/tmp/omp-server", "args": [], "cwd": "/tmp" }
            }),
        ));
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.iter().any(|o| matches!(o,
            Outgoing::Event(e) if e.event == "output"
                && e.body["output"].as_str().is_some_and(|t| t.contains("-d3")))));
        ses.handle(&req(
            "setBreakpoints",
            &json!({ "source": { "path": "a.pwn" }, "breakpoints": [ { "line": 4 } ] }),
        ));
        assert!(
            ses.resolved_breakpoints().is_empty(),
            "resolveu com o mapa velho"
        );
    }

    /// Sem `launch` bem-sucedido não há servidor: o restart falha em vez de
    /// subir um servidor que ninguém configurou.
    #[test]
    fn restart_without_server_fails_without_spawning() {
        let mut ses = Session::new();
        ses.handle(&req("launch", &json!({ "program": "/tmp/gm.amx" })));

        let out = ses.handle(&req("restart", &json!({})));
        assert!(
            !out.iter().any(|o| matches!(o, Outgoing::SpawnServer(_))),
            "sem spawn_spec não há o que reiniciar"
        );
        assert!(!first_response(&out).success, "e o editor fica sabendo");
    }

    /// Um projeto em disco com o `a.pwn` do `sample_dbg` (linha 3 = `foo();`
    /// no endereço 8, linha 4 = `bar();` no 20) e o `.amx` compilado dele.
    struct Compiled {
        dir: std::path::PathBuf,
    }

    impl Compiled {
        fn new(name: &str) -> (Self, Session) {
            let dir = std::env::temp_dir().join(format!("pawnpro-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let project = Self { dir };
            project.write_source("x\ny\nfoo();\nbar();\n", 1_000);
            let amx = project.dir.join("a.amx");
            std::fs::write(&amx, b"amx").unwrap();
            set_mtime(&amx, 2_000);
            let mut session = Session::new();
            session.set_compiled(&amx, sample_dbg());
            (project, session)
        }

        fn source(&self) -> String {
            self.dir.join("a.pwn").to_string_lossy().into_owned()
        }

        /// Grava o fonte com a data dada, em segundos desde a época.
        fn write_source(&self, text: &str, mtime: u64) {
            let path = self.dir.join("a.pwn");
            std::fs::write(&path, text).unwrap();
            set_mtime(&path, mtime);
        }
    }

    impl Drop for Compiled {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn set_mtime(path: &std::path::Path, seconds: u64) {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn set_line_breakpoints(ses: &mut Session, path: &str, lines: &[i64]) -> Vec<Outgoing> {
        let bps: Vec<Value> = lines.iter().map(|l| json!({ "line": l })).collect();
        ses.handle(&req(
            "setBreakpoints",
            &json!({ "source": { "path": path }, "breakpoints": bps }),
        ))
    }

    /// O caso que motivou o retrato: apagar uma linha acima desloca `bar();`
    /// para a linha 3, que no binário é `foo();`. Resolver pelo número pararia
    /// no código errado, sem aviso.
    #[test]
    fn breakpoint_after_an_edit_lands_on_the_same_code() {
        let (project, mut ses) = Compiled::new("bp-edit");
        project.write_source("y\nfoo();\nbar();\n", 3_000);

        let out = set_line_breakpoints(&mut ses, &project.source(), &[3]);
        assert_eq!(
            ses.resolved_breakpoints(),
            &[(3, 20)],
            "`bar();` tem de ir para o endereço de `bar();`"
        );
        let bp = &first_response(&out).body["breakpoints"][0];
        assert_eq!(bp["verified"], true);
        assert_eq!(bp["line"], 3, "a linha volta na numeração atual");
    }

    /// O compilador grava o arquivo relativo à pasta onde rodou
    /// (`../include/x.inc`), e o editor manda o caminho absoluto. Comparar o
    /// texto nunca casava: breakpoint em include incluído com `..` não
    /// funcionava.
    #[test]
    fn breakpoint_in_a_file_named_relative_to_the_build_folder() {
        let dir = std::env::temp_dir().join(format!("pawnpro-relative-{}", std::process::id()));
        let (build, src) = (dir.join("gamemodes"), dir.join("include"));
        std::fs::create_dir_all(&build).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        let source = src.join("a.inc");
        std::fs::write(&source, "x\ny\nfoo();\nbar();\n").unwrap();
        set_mtime(&source, 1_000);
        let amx = build.join("gm.amx");
        std::fs::write(&amx, b"amx").unwrap();
        set_mtime(&amx, 2_000);

        let mut ses = Session::new();
        ses.set_compiled(&amx, dbg_bytes_named("../include/a.inc", 0, |_| {}));
        set_line_breakpoints(&mut ses, &source.to_string_lossy(), &[4]);
        let resolved = ses.resolved_breakpoints().to_vec();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(resolved, [(4, 20)]);
    }

    /// Linha nova não tem código compilado: o editor precisa saber por quê, e
    /// o plugin não pode receber endereço nenhum para ela.
    #[test]
    fn breakpoint_on_a_new_line_is_not_verified() {
        let (project, mut ses) = Compiled::new("bp-new");
        project.write_source("x\ny\nfoo();\nnew_call();\nbar();\n", 3_000);

        let out = set_line_breakpoints(&mut ses, &project.source(), &[4]);
        assert!(ses.resolved_breakpoints().is_empty());
        let bp = &first_response(&out).body["breakpoints"][0];
        assert_eq!(bp["verified"], false);
        assert_eq!(bp["line"], 4);
        assert!(
            bp["message"]
                .as_str()
                .is_some_and(|m| m.contains("Restart"))
        );
    }

    /// O arquivo já estava editado quando o `.amx` foi carregado: o que foi
    /// compilado não é o que está em disco, e não há como mapear.
    #[test]
    fn source_newer_than_the_binary_is_not_trusted() {
        let (project, _) = Compiled::new("bp-newer");
        project.write_source("x\ny\nfoo();\nbar();\n", 3_000);
        let mut ses = Session::new();
        ses.set_compiled(&project.dir.join("a.amx"), sample_dbg());

        let out = set_line_breakpoints(&mut ses, &project.source(), &[4]);
        assert!(ses.resolved_breakpoints().is_empty());
        assert_eq!(
            first_response(&out).body["breakpoints"][0]["verified"],
            false
        );
    }

    #[test]
    fn unchanged_source_resolves_as_before() {
        let (project, mut ses) = Compiled::new("bp-same");
        set_line_breakpoints(&mut ses, &project.source(), &[4]);
        assert_eq!(ses.resolved_breakpoints(), &[(4, 20)]);
    }

    /// Cada frame aponta o próprio arquivo. Antes, todos apontavam o último
    /// arquivo em que o editor pôs breakpoint: com um breakpoint num include, o
    /// frame do `.pwn` principal destacava a linha certa no arquivo errado.
    #[test]
    fn each_frame_points_to_its_own_file() {
        let (project, mut ses) = Compiled::new("stack-files");
        let include_dir = project.dir.join("inc");
        std::fs::create_dir_all(&include_dir).unwrap();
        let include = include_dir.join("b.inc");
        std::fs::write(&include, "stock helper()\n{\n}\n").unwrap();
        set_mtime(&include, 1_000);
        let include_path = include.to_string_lossy().into_owned();

        // O editor abriu o `.pwn` e pôs um breakpoint no include.
        set_line_breakpoints(&mut ses, &project.source(), &[4]);
        set_line_breakpoints(&mut ses, &include_path, &[2]);
        ses.frames().replace(vec![
            pawnpro_dbg_protocol::Frame {
                name: "helper".into(),
                file: Some("inc/../inc/b.inc".into()),
                line: Some(2),
                vars: Vec::new(),
            },
            pawnpro_dbg_protocol::Frame {
                name: "bar".into(),
                file: Some("a.pwn".into()),
                line: Some(4),
                vars: Vec::new(),
            },
        ]);

        let out = ses.handle(&req("stackTrace", &Value::Null));
        let frames = &first_response(&out).body["stackFrames"];
        assert_eq!(frames[0]["source"]["path"], include_path.as_str());
        assert_eq!(frames[0]["line"], 2);
        assert_eq!(frames[1]["source"]["path"], project.source().as_str());
        assert_eq!(frames[1]["line"], 4);
    }

    /// A pausa chega com as linhas da compilação; o editor mostra o arquivo
    /// atual. Sem converter, o destaque cairia em outra linha.
    #[test]
    fn stack_trace_follows_the_edited_file() {
        let (project, mut ses) = Compiled::new("stack-edit");
        set_line_breakpoints(&mut ses, &project.source(), &[]);
        project.write_source("y\nfoo();\nbar();\n", 3_000);
        ses.frames().replace(vec![
            pawnpro_dbg_protocol::Frame {
                name: "bar".into(),
                file: None,
                line: Some(4),
                vars: Vec::new(),
            },
            pawnpro_dbg_protocol::Frame {
                name: "old".into(),
                file: None,
                line: Some(1),
                vars: Vec::new(),
            },
        ]);

        let out = ses.handle(&req("stackTrace", &Value::Null));
        let frames = &first_response(&out).body["stackFrames"];
        assert_eq!(frames[0]["line"], 3);
        assert!(frames[0]["source"].is_object());
        // A linha 1 da compilação (`x`) foi apagada: sem fonte, e o nome diz.
        assert_eq!(frames[1]["line"], 0);
        assert!(frames[1].get("source").is_none());
        assert!(frames[1]["name"].as_str().unwrap().contains("line 1"));
    }

    fn req(command: &str, args: &Value) -> Request {
        // Request não é Deserialize-friendly de construir à mão; via JSON.
        serde_json::from_value(json!({
            "seq": 1, "type": "request", "command": command, "arguments": args
        }))
        .unwrap()
    }

    /// Primeira Response na lista de saída.
    fn first_response(out: &[Outgoing]) -> &Response {
        out.iter()
            .find_map(|o| match o {
                Outgoing::Response(r) => Some(r),
                _ => None,
            })
            .expect("esperava uma Response")
    }
    /// `true` se há um Event com o nome dado.
    fn has_event(out: &[Outgoing], name: &str) -> bool {
        out.iter()
            .any(|o| matches!(o, Outgoing::Event(e) if e.event == name))
    }
    /// `true` se há um comando para o plugin que casa o predicado.
    fn has_command(out: &[Outgoing], pred: impl Fn(&Command) -> bool) -> bool {
        out.iter()
            .any(|o| matches!(o, Outgoing::ToPlugin(c) if pred(c)))
    }

    #[test]
    fn initialize_emits_capabilities_and_initialized() {
        let mut s = Session::new();
        let out = s.handle(&req("initialize", &Value::Null));
        let r = first_response(&out);
        assert!(r.success && r.command == "initialize");
        assert!(has_event(&out, "initialized"));
    }

    #[test]
    fn threads_returns_single_thread() {
        let mut s = Session::new();
        let out = s.handle(&req("threads", &Value::Null));
        assert_eq!(first_response(&out).body["threads"][0]["id"], 1);
    }

    #[test]
    fn set_breakpoints_resolves_and_forwards_addrs() {
        let mut s = Session::new();
        s.set_debug(sample_dbg());
        // Linhas são 1-based (a lib soma +1 ao zero-based do compilador): a
        // entrada gravada como `3` é a linha 4 para o editor, no endereço 20.
        let args = json!({
            "source": { "path": "a.pwn" },
            "breakpoints": [ { "line": 4 }, { "line": 999 } ]
        });
        let out = s.handle(&req("setBreakpoints", &args));
        let bps = first_response(&out).body["breakpoints"].as_array().unwrap();
        assert_eq!(bps[0]["verified"], true); // linha 4 existe
        assert_eq!(bps[1]["verified"], false); // linha 999 não
        assert_eq!(s.resolved_breakpoints(), &[(4, 20)]);
        // O endereço resolvido é encaminhado ao plugin (sem condição).
        assert!(has_command(
            &out,
            |c| matches!(c, Command::SetBreakpoints { breakpoints }
            if breakpoints.len() == 1 && breakpoints[0].addr == 20 && breakpoints[0].condition.is_none())
        ));
    }

    #[test]
    fn set_breakpoints_forwards_condition() {
        let mut s = Session::new();
        s.set_debug(sample_dbg());
        let args = json!({
            "source": { "path": "a.pwn" },
            "breakpoints": [ { "line": 4, "condition": "x == 5" } ]
        });
        let out = s.handle(&req("setBreakpoints", &args));
        assert!(has_command(
            &out,
            |c| matches!(c, Command::SetBreakpoints { breakpoints }
            if breakpoints.len() == 1
                && breakpoints[0].addr == 20
                && breakpoints[0].condition.as_deref() == Some("x == 5"))
        ));
    }

    #[test]
    fn set_breakpoints_forwards_hit_condition_and_logpoint() {
        let mut s = Session::new();
        s.set_debug(sample_dbg());
        let args = json!({
            "source": { "path": "a.pwn" },
            "breakpoints": [
                { "line": 4, "hitCondition": ">=3", "logMessage": "x={x}" }
            ]
        });
        let out = s.handle(&req("setBreakpoints", &args));
        assert!(has_command(
            &out,
            |c| matches!(c, Command::SetBreakpoints { breakpoints }
            if breakpoints.len() == 1
                && breakpoints[0].hit_condition.as_deref() == Some(">=3")
                && breakpoints[0].log_message.as_deref() == Some("x={x}"))
        ));
    }

    #[test]
    fn initialize_advertises_logpoints_and_hit_count() {
        let mut s = Session::new();
        let out = s.handle(&req("initialize", &Value::Null));
        let caps = &first_response(&out).body;
        assert_eq!(caps["supportsLogPoints"], true);
        assert_eq!(caps["supportsHitConditionalBreakpoints"], true);
    }

    #[test]
    fn continue_and_steps_forward_to_plugin() {
        let mut s = Session::new();
        assert!(has_command(
            &s.handle(&req("continue", &Value::Null)),
            |c| matches!(c, Command::Continue)
        ));
        assert!(has_command(
            &s.handle(&req("next", &Value::Null)),
            |c| matches!(c, Command::Step { mode: Step::Over })
        ));
        assert!(has_command(
            &s.handle(&req("stepIn", &Value::Null)),
            |c| matches!(c, Command::Step { mode: Step::In })
        ));
        assert!(has_command(
            &s.handle(&req("stepOut", &Value::Null)),
            |c| matches!(c, Command::Step { mode: Step::Out })
        ));
    }

    /// O plugin só alcança o núcleo pelo canal que a subida do servidor
    /// reserva: sem o comando do servidor, o `launch` não tem o que fazer e
    /// precisa dizer por quê, em vez de esperar um plugin que nunca vem.
    #[test]
    fn launch_without_server_command_fails() {
        let mut s = Session::new();
        let out = s.handle(&req("launch", &json!({ "program": "/tmp/gm.amx" })));
        assert!(!first_response(&out).success);
        assert!(!out.iter().any(|o| matches!(o, Outgoing::SpawnServer(_))));
    }

    #[test]
    fn launch_with_server_command_spawns_server() {
        let mut s = Session::new();
        let out = s.handle(&req(
            "launch",
            &json!({
                "program": "/tmp/gm.amx",
                "locale": "pt-BR",
                "serverCommand": { "exe": "/srv/omp-server", "args": ["-c"], "cwd": "/srv" }
            }),
        ));
        assert!(first_response(&out).success);
        let spec = out
            .iter()
            .find_map(|o| match o {
                Outgoing::SpawnServer(spec) => Some(spec),
                _ => None,
            })
            .expect("o launch deve subir o servidor");
        assert_eq!(spec.exe, "/srv/omp-server");
        assert_eq!(spec.args, ["-c"]);
        assert_eq!(spec.cwd, "/srv");
        assert_eq!(spec.amx_path, "/tmp/gm.amx");
        assert_eq!(spec.locale, "pt-BR");
    }

    #[test]
    fn launch_derives_source_from_program_for_stacktrace() {
        // Sem setBreakpoints, o stackTrace deve ancorar o frame ao `.pwn` derivado
        // do `program` (.amx) — senão o editor mostra "Origem Desconhecida" ao
        // pausar num erro de runtime.
        let mut s = Session::new();
        s.set_debug(sample_dbg());
        s.handle(&req(
            "launch",
            &json!({ "program": "/srv/gm/gamemode.amx" }),
        ));
        let out = s.handle(&req("stackTrace", &Value::Null));
        let frame = &first_response(&out).body["stackFrames"][0];
        assert_eq!(frame["source"]["path"], "/srv/gm/gamemode.pwn");
    }

    #[test]
    fn scopes_reference_follows_frame_id() {
        // O escopo "Locais" referencia o frame pedido (frameId), para o
        // `variables` seguinte ler daquele frame — e não de um id fixo.
        let mut s = Session::new();
        let out = s.handle(&req("scopes", &json!({ "frameId": 3 })));
        let scope = &first_response(&out).body["scopes"][0];
        assert_eq!(scope["variablesReference"], 3);
    }

    #[test]
    fn frame_index_maps_1based_reference_to_0based() {
        // variablesReference/frameId são 1-based (id do stackTrace); o índice do
        // frame é `ref - 1`. Ausente ou inválido cai no topo (0).
        assert_eq!(frame_index(Some(&json!(1))), 0);
        assert_eq!(frame_index(Some(&json!(3))), 2);
        assert_eq!(frame_index(None), 0);
        assert_eq!(frame_index(Some(&json!(0))), 0); // inválido → topo
    }

    /// Um array de enum como `g_Conta[4][E_CONTA]` tem duas dimensões: o painel
    /// precisa expandir a linha e marcar a célula, com o caminho inteiro.
    #[test]
    fn nested_array_expands_and_watches_the_cell() {
        use pawnpro_dbg_protocol::{Frame, Var};
        let cell = |v: &str, i: usize| Var {
            name: format!("[{i}]"),
            value: v.into(),
            children: vec![],
        };
        let row = |i: usize, values: [&str; 2]| Var {
            name: format!("[{i}]"),
            value: "[…]".into(),
            children: vec![cell(values[0], 0), cell(values[1], 1)],
        };
        let mut s = Session::new();
        s.frames().replace(vec![Frame {
            name: "Mexer".into(),
            file: None,
            line: Some(1),
            vars: vec![Var {
                name: "g_Conta".into(),
                value: "[…]".into(),
                children: vec![row(0, ["1", "2"]), row(1, ["30", "40"])],
            }],
        }]);
        s.handle(&req("stackTrace", &Value::Null));

        let top = s.handle(&req("variables", &json!({ "variablesReference": 1 })));
        let conta = first_response(&top).body["variables"][0]["variablesReference"]
            .as_i64()
            .unwrap();
        let row_list = s.handle(&req("variables", &json!({ "variablesReference": conta })));
        let second_row = &first_response(&row_list).body["variables"][1];
        assert_eq!(second_row["name"], "[1]");
        let second_row_ref = second_row["variablesReference"].as_i64().unwrap();
        assert_ne!(second_row_ref, 0, "a linha precisa ser expansível");

        let cells = s.handle(&req(
            "variables",
            &json!({ "variablesReference": second_row_ref }),
        ));
        let cell = &first_response(&cells).body["variables"][0];
        assert_eq!(cell["value"], "30");
        assert_eq!(cell["memoryReference"], "0:g_Conta:1.0");

        // A célula é observável, com o caminho completo.
        let info = s.handle(&req(
            "dataBreakpointInfo",
            &json!({ "variablesReference": second_row_ref, "name": "[0]" }),
        ));
        let body = &first_response(&info).body;
        assert_eq!(body["dataId"], "0:g_Conta:1.0");
        assert_eq!(body["description"], "g_Conta[1][0]");

        // A linha inteira não muda de valor: não é observável.
        let info = s.handle(&req(
            "dataBreakpointInfo",
            &json!({ "variablesReference": conta, "name": "[1]" }),
        ));
        assert!(first_response(&info).body["dataId"].is_null());

        // Editar a célula pede a escrita com o caminho inteiro. O painel só
        // muda quando o plugin confirmar — antes disso, nada de valor novo.
        let out = s.handle(&req(
            "setVariable",
            &json!({ "variablesReference": second_row_ref, "name": "[1]", "value": "7" }),
        ));
        let [Outgoing::WriteVariable(write)] = out.as_slice() else {
            panic!("esperava só o pedido de escrita");
        };
        assert_eq!(
            (write.name.as_str(), write.path.as_slice(), write.value),
            ("g_Conta", &[1, 1][..], 7)
        );
        assert_eq!(write.var, 0);
        assert_eq!(write.label(), "g_Conta[1][1]");
        assert_eq!(s.frames().vars(0)[0].children[1].children[1].value, "40");
    }

    #[test]
    fn read_memory_parses_reference() {
        let mut s = Session::new();
        let out = s.handle(&req(
            "readMemory",
            &json!({ "memoryReference": "0:health", "offset": 2, "count": 8 }),
        ));
        assert!(out.iter().any(|o| matches!(o,
            Outgoing::ReadMemory { frame: 0, name, path, offset: 2, count: 8, .. }
            if name == "health" && path.is_empty())));
        // Referência inválida → resposta de falha.
        let out = s.handle(&req(
            "readMemory",
            &json!({ "memoryReference": "lixo", "offset": 0, "count": 4 }),
        ));
        assert!(!first_response(&out).success);
    }

    #[test]
    fn parse_lvalue_scalar_and_element() {
        let no_vars: Vec<pawnpro_dbg_protocol::Var> = vec![];
        assert_eq!(parse_lvalue("x", &no_vars), Some(("x".into(), vec![])));
        assert_eq!(
            parse_lvalue("arr[2]", &no_vars),
            Some(("arr".into(), vec![2]))
        );
        assert_eq!(
            parse_lvalue("conta[1][2]", &no_vars),
            Some(("conta".into(), vec![1, 2]))
        );
        // Não é lvalue simples.
        assert_eq!(parse_lvalue("x + 1", &no_vars), None);
        assert_eq!(parse_lvalue("arr[", &no_vars), None);
        assert_eq!(parse_lvalue("", &no_vars), None);
    }

    /// Nome que não está no frame: o motivo é "não existe", não uma falha de
    /// escrita — e nada vai ao plugin.
    #[test]
    fn set_expression_on_unknown_name_fails_without_writing() {
        let mut s = Session::new();
        let out = s.handle(&req(
            "setExpression",
            &json!({ "expression": "nao_existe", "value": "3", "frameId": 1 }),
        ));
        assert!(!out.iter().any(|o| matches!(o, Outgoing::WriteVariable(_))));
        let response = first_response(&out);
        assert!(!response.success);
        assert!(
            response
                .message
                .as_deref()
                .is_some_and(|m| m.contains("could not evaluate"))
        );
    }

    #[test]
    fn set_expression_forwards_element_edit() {
        let mut s = Session::new();
        s.frames().replace(vec![pawnpro_dbg_protocol::Frame {
            name: "main".into(),
            file: None,
            line: Some(1),
            vars: vec![pawnpro_dbg_protocol::Var {
                name: "arr".into(),
                value: "[…]".into(),
                children: (0..3)
                    .map(|i| pawnpro_dbg_protocol::Var {
                        name: format!("[{i}]"),
                        value: "0".into(),
                        children: vec![],
                    })
                    .collect(),
            }],
        }]);
        let out = s.handle(&req(
            "setExpression",
            &json!({ "expression": "arr[2]", "value": "9", "frameId": 1 }),
        ));
        let [Outgoing::WriteVariable(write)] = out.as_slice() else {
            panic!("esperava só o pedido de escrita");
        };
        assert_eq!(
            (write.name.as_str(), write.path.as_slice(), write.value),
            ("arr", &[2][..], 9)
        );
        assert_eq!(write.shown, "9");
    }

    #[test]
    fn word_prefix_extracts_trailing_identifier() {
        assert_eq!(word_prefix("hea", 4), "hea"); // cursor no fim
        assert_eq!(word_prefix("x + he", 7), "he"); // após operador
        assert_eq!(word_prefix("arr[i", 6), "i"); // dentro de colchete
        assert_eq!(word_prefix("x + ", 5), ""); // depois de espaço → vazio
        assert_eq!(word_prefix("health", 4), "hea"); // cursor no meio
    }

    #[test]
    fn parse_elem_index_reads_bracketed() {
        assert_eq!(parse_elem_index("[0]"), Some(0));
        assert_eq!(parse_elem_index("[42]"), Some(42));
        assert_eq!(parse_elem_index("x"), None);
        assert_eq!(parse_elem_index("[a]"), None);
    }

    #[test]
    fn parse_data_id_splits_frame_name_index() {
        assert_eq!(
            parse_data_id("0:health"),
            Some(DataWatch {
                frame: 0,
                name: "health".into(),
                path: vec![],
            })
        );
        // Elemento de array: frame:name:i, ou frame:name:i.j em várias dimensões.
        assert_eq!(
            parse_data_id("2:arr:3"),
            Some(DataWatch {
                frame: 2,
                name: "arr".into(),
                path: vec![3],
            })
        );
        assert_eq!(
            parse_data_id("0:conta:1.2"),
            Some(DataWatch {
                frame: 0,
                name: "conta".into(),
                path: vec![1, 2],
            })
        );
        assert_eq!(parse_data_id("0:conta:1.x"), None);
        // Sem separador ou frame não-numérico → None.
        assert_eq!(parse_data_id("semdoispontos"), None);
        assert_eq!(parse_data_id("x:health"), None);
    }

    #[test]
    fn set_data_breakpoints_forwards_watches() {
        let mut s = Session::new();
        let args = json!({
            "breakpoints": [ { "dataId": "1:health" }, { "dataId": "0:g_placar" } ]
        });
        let out = s.handle(&req("setDataBreakpoints", &args));
        // Encaminha os dois watches decodificados ao plugin.
        assert!(has_command(
            &out,
            |c| matches!(c, Command::SetDataBreakpoints { watches }
            if watches.len() == 2
                && watches[0] == DataWatch { frame: 1, name: "health".into(), path: vec![] }
                && watches[1] == DataWatch { frame: 0, name: "g_placar".into(), path: vec![] })
        ));
        // E responde os dois como verificados.
        let bps = first_response(&out).body["breakpoints"].as_array().unwrap();
        assert_eq!(bps.len(), 2);
        assert_eq!(bps[0]["verified"], true);
    }

    #[test]
    fn set_exception_breakpoints_toggles_runtime() {
        let mut s = Session::new();
        // Filtro presente → liga.
        let out = s.handle(&req(
            "setExceptionBreakpoints",
            &json!({ "filters": ["runtime"] }),
        ));
        assert!(has_command(&out, |c| matches!(
            c,
            Command::SetExceptionFilter { runtime: true }
        )));
        // Lista vazia → desliga.
        let out = s.handle(&req("setExceptionBreakpoints", &json!({ "filters": [] })));
        assert!(has_command(&out, |c| matches!(
            c,
            Command::SetExceptionFilter { runtime: false }
        )));
    }

    #[test]
    fn function_breakpoints_resolve_and_union_with_line() {
        let mut s = Session::new();
        s.set_debug(sample_dbg_fn());
        // 1 breakpoint de linha (linha 4 → addr 20).
        s.handle(&req(
            "setBreakpoints",
            &json!({ "source": { "path": "a.pwn" }, "breakpoints": [ { "line": 4 } ] }),
        ));
        // Breakpoint de função "foo" (entrada em addr 8) + "naoexiste" (não resolve).
        let out = s.handle(&req(
            "setFunctionBreakpoints",
            &json!({ "breakpoints": [ { "name": "foo" }, { "name": "naoexiste" } ] }),
        ));
        // Verificação: foo ok, naoexiste não.
        let bps = first_response(&out).body["breakpoints"].as_array().unwrap();
        assert_eq!(bps[0]["verified"], true);
        assert_eq!(bps[1]["verified"], false);
        // A união enviada ao plugin tem o bp de linha (20) e o de função (8).
        assert!(has_command(
            &out,
            |c| matches!(c, Command::SetBreakpoints { breakpoints }
            if breakpoints.iter().any(|b| b.addr == 20) && breakpoints.iter().any(|b| b.addr == 8))
        ));
    }

    #[test]
    fn initialize_advertises_function_breakpoints() {
        let mut s = Session::new();
        let out = s.handle(&req("initialize", &Value::Null));
        assert_eq!(
            first_response(&out).body["supportsFunctionBreakpoints"],
            true
        );
    }

    #[test]
    fn initialize_advertises_exception_filter() {
        let mut s = Session::new();
        let out = s.handle(&req("initialize", &Value::Null));
        let filters = &first_response(&out).body["exceptionBreakpointFilters"];
        assert_eq!(filters[0]["filter"], "runtime");
    }

    #[test]
    fn initialize_advertises_data_breakpoints() {
        let mut s = Session::new();
        let out = s.handle(&req("initialize", &Value::Null));
        assert_eq!(first_response(&out).body["supportsDataBreakpoints"], true);
    }

    #[test]
    fn disconnect_terminates() {
        let mut s = Session::new();
        let out = s.handle(&req("disconnect", &Value::Null));
        assert!(s.is_terminated());
        assert!(has_event(&out, "terminated"));
    }

    /// Ao clicar em Parar, o editor manda `terminate` e, depois do
    /// `terminated`, `disconnect`. Se o `terminate` encerrasse a sessão, o
    /// `disconnect` ficaria sem resposta até o prazo do editor esgotar.
    #[test]
    fn terminate_stops_the_server_but_keeps_the_session_for_disconnect() {
        let mut s = Session::new();
        let out = s.handle(&req("terminate", &Value::Null));
        assert!(out.iter().any(|o| matches!(o, Outgoing::StopServer)));
        assert!(has_event(&out, "terminated"));
        assert!(!s.is_terminated(), "a sessão precisa esperar o disconnect");

        let out = s.handle(&req("disconnect", &Value::Null));
        assert!(first_response(&out).success);
        assert!(s.is_terminated());
        assert!(!has_event(&out, "terminated"), "o fim já foi avisado");
    }

    /// Bloco de debug mínimo (mesma forma do teste do amxdbg): a.pwn linha 3 → 20.
    fn sample_dbg() -> AmxDbg {
        dbg_bytes(0, |_| {})
    }

    /// Como `sample_dbg`, mas com uma função `foo` no range `[8, 40)` — para testar
    /// `setFunctionBreakpoints` (o endereço de entrada cai na 1ª linha, addr 8).
    fn sample_dbg_fn() -> AmxDbg {
        dbg_bytes(1, |t| {
            ext_u32(t, 0); // address
            ext_i16(t, 0); // tag
            ext_u32(t, 8); // codestart
            ext_u32(t, 40); // codeend
            t.push(9); // ident = Function
            t.push(0); // vclass = global
            ext_i16(t, 0); // dim
            ext_cstr(t, "foo"); // name
        })
    }

    /// Monta um `AmxDbg` com 1 arquivo, 2 linhas ((8,2),(20,3)) e `nsyms` símbolos
    /// (escritos por `push_syms`).
    fn dbg_bytes(nsyms: i16, push_syms: impl Fn(&mut Vec<u8>)) -> AmxDbg {
        dbg_bytes_named("a.pwn", nsyms, push_syms)
    }

    /// Como `dbg_bytes`, com o nome do arquivo na tabela escolhido.
    fn dbg_bytes_named(file: &str, nsyms: i16, push_syms: impl Fn(&mut Vec<u8>)) -> AmxDbg {
        AmxDbg::parse(&dbg_raw(file, nsyms, push_syms)).unwrap()
    }

    /// Os bytes de um bloco de debug avulso — o que um `.amx.dbg` guarda, e o
    /// que o `load_debug` também aceita.
    fn dbg_raw(file: &str, nsyms: i16, push_syms: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
        let mut t = Vec::new();
        ext_u32(&mut t, 0);
        ext_cstr(&mut t, file);
        ext_u32(&mut t, 8);
        ext_i32(&mut t, 2);
        ext_u32(&mut t, 20);
        ext_i32(&mut t, 3);
        push_syms(&mut t);
        let mut b = Vec::new();
        ext_i32(&mut b, i32::try_from(22 + t.len()).unwrap());
        b.extend_from_slice(&samp_sdk::debug::AMX_DBG_MAGIC.to_le_bytes());
        b.push(1);
        b.push(1);
        ext_i16(&mut b, 0); // flags
        ext_i16(&mut b, 1); // files
        ext_i16(&mut b, 2); // lines
        ext_i16(&mut b, nsyms); // symbols
        ext_i16(&mut b, 0); // tags
        ext_i16(&mut b, 0); // automatons
        ext_i16(&mut b, 0); // states
        b.extend_from_slice(&t);
        b
    }

    fn ext_i16(v: &mut Vec<u8>, x: i16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn ext_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn ext_i32(v: &mut Vec<u8>, x: i32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn ext_cstr(v: &mut Vec<u8>, s: &str) {
        v.extend_from_slice(s.as_bytes());
        v.push(0);
    }
}
