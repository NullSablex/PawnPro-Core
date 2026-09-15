//! Tratamento do debug break — a camada de decisão que o SDK alimenta.
//!
//! O encanamento da VM (instalar o hook, ler `cip`/`frm`, ler/escrever células
//! com checagem de limites) fica no SDK `samp`; este módulo só decide se pausa
//! numa linha e, na pausa, coleta variáveis ([`inspect`]), avisa o adaptador
//! ([`bridge`]) e **bloqueia** até continuar/step ([`gate`]).
//!
//! [`on_break`] é chamado por `SampPlugin::on_debug_break` (ver `lib.rs`), que o
//! SDK liga via `samp::plugin::enable_debug_hook`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use samp::debug::AmxDbg;
use samp::prelude::Amx;

use crate::bridge::BRIDGE;
use crate::control::{
    Bp, BreakAction, Controller, DataWatch, StepMode, StopReason, eval_condition, interpolate_log,
};
use crate::gate::Resume;
use crate::inspect::{self, CellReader};
use crate::runtime_error::{self, Locale};
use pawnpro_dbg_protocol::{Breakpoint, Event, Frame};
use samp::debug::OpcodeMap;
use samp::debug::VClass;
use samp::debug::stack;

/// Tamanho (bytes) de uma instrução AMX. No hook, o `cip` aponta para a célula
/// SEGUINTE ao `OP_BREAK`; voltamos isto para chegar ao endereço da linha.
const BREAK_OP_SIZE: u32 = 4;

/// Controle de execução (breakpoints/step), compartilhado com a thread TCP.
static STATE: Mutex<Controller> = Mutex::new(Controller::new_const());
/// Bloco de debug do `.amx` depurado (carregado no `on_load` do plugin).
static DBG: Mutex<Option<AmxDbg>> = Mutex::new(None);

/// Contexto da pausa ATUAL, válido só enquanto a VM está bloqueada em
/// [`on_pause`]. A thread do socket usa isto para atender comandos que precisam
/// da VM num frame específico (editar variável, ler memória). O ponteiro do
/// `amx` vai como `usize` para ser `Send` — a thread da VM está parada, então
/// ele continua válido durante a pausa.
static PAUSE_CTX: Mutex<Option<PauseCtx>> = Mutex::new(None);

/// Ponteiro do `amx` pausado (como `usize`) e o `(cip, frm)` de cada frame,
/// índice 0 = topo (onde a VM parou).
type PauseCtx = (usize, Vec<(u32, i32)>);

/// Mapa de opcodes da VM carregada, para detectar erro de runtime antes do
/// abort. `None` até `load_opcode_map` rodar; numa imagem não-relocada o mapa é
/// efetivamente identidade. Montado uma vez por VM, na carga.
static OPCODE_MAP: Mutex<Option<OpcodeMap>> = Mutex::new(None);

/// Idioma das mensagens de erro (resolvido do locale do editor, via env var).
/// Padrão inglês até `set_locale` rodar no carregamento do plugin.
static LOCALE: Mutex<Locale> = Mutex::new(Locale::En);

/// Pausa em erro de runtime ligada? (filtro de exceção do editor). Ligado por
/// padrão; o adaptador desliga via `Command::SetExceptionFilter`.
static RUNTIME_ERRORS: AtomicBool = AtomicBool::new(true);

/// Liga/desliga a pausa em erros de runtime (div-zero, bounds, STACKERR, …).
pub fn set_runtime_errors(on: bool) {
    RUNTIME_ERRORS.store(on, Ordering::Relaxed);
}

/// Define o idioma das mensagens de erro. Chamado no `on_load` a partir de
/// `PAWNPRO_DBG_LOCALE` (que o adaptador propaga do editor).
pub fn set_locale(locale: Locale) {
    if let Ok(mut guard) = LOCALE.lock() {
        *guard = locale;
    }
}

/// Lê células pelo `Amx::read_cell` do SDK (com checagem de limites, espelhando
/// `amx_GetAddr`). Mantém [`inspect::collect`] desacoplado do SDK e testável com
/// um leitor falso.
impl CellReader for Amx {
    fn read_cell(&self, data_addr: i32) -> Option<i32> {
        Amx::read_cell(self, data_addr)
    }
}

/// Trata um debug break: lê `cip`/`frm` da VM e decide o motivo da pausa. Nenhum
/// panic atravessa de volta para o SDK — os locks são tomados com `if let Ok`.
///
/// Chamado por `SampPlugin::on_debug_break`.
pub fn on_break(amx: &Amx) {
    let (Some(raw_cip), Some(frm)) = (amx.cip(), amx.frame()) else {
        return;
    };
    // No hook, o `cip` já aponta para a instrução DEPOIS do `OP_BREAK` (o ip
    // avançou uma célula de 4 bytes). A tabela de linhas/breakpoints usa o
    // endereço do próprio break: voltamos 4 para bater.
    let cip = raw_cip.wrapping_sub(BREAK_OP_SIZE);

    // Erro de runtime tem prioridade sobre breakpoint/step: se a PRÓXIMA
    // instrução (`raw_cip`) for abortar a VM, pausa agora com reason
    // "exception" — o ABORT da VM retornaria sem nos chamar de novo. A linha
    // mostrada continua sendo a do break atual (`cip`).
    if RUNTIME_ERRORS.load(Ordering::Relaxed)
        && let Some(err) = detect_runtime_error(amx, raw_cip)
    {
        if let Ok(mut ctrl) = STATE.lock() {
            ctrl.hit_breakpoint(); // limpa step pendente e marca como iniciado
        }
        let locale = LOCALE.lock().map(|g| *g).unwrap_or_default();
        on_pause(amx, cip, frm, "exception", Some(err.message(locale)));
        return;
    }

    // Data breakpoints: pausa se uma variável observada mudou de valor desde a
    // última linha. Verificado antes do breakpoint/step (é uma causa distinta de
    // parada); watches de locais expiram quando o frame dono retorna.
    if let Some(name) = check_data_watch(amx, cip, frm) {
        if let Ok(mut ctrl) = STATE.lock() {
            ctrl.hit_breakpoint();
        }
        on_pause(amx, cip, frm, "data breakpoint", Some(&name));
        return;
    }

    let reason = {
        let Ok(mut ctrl) = STATE.lock() else { return };
        // Decisão do breakpoint (condição + hit-count + logpoint) num só lugar;
        // a condição é avaliada preguiçosamente contra as variáveis em escopo.
        match ctrl.on_hit(cip, |expr| eval_breakpoint_condition(amx, cip, frm, expr)) {
            BreakAction::Pause => {
                ctrl.hit_breakpoint();
                Some(StopReason::Breakpoint)
            }
            // Logpoint: emite a mensagem interpolada e segue — mas um step
            // pendente ainda pode parar nesta linha.
            BreakAction::Log(template) => {
                emit_logpoint(amx, cip, frm, &template);
                ctrl.should_stop(cip, frm)
            }
            BreakAction::None => ctrl.should_stop(cip, frm),
        }
    };
    if let Some(reason) = reason {
        on_pause(amx, cip, frm, reason_str(reason), None);
    }
}

/// Interpola a mensagem de um logpoint com as variáveis em escopo e a envia ao
/// adaptador como um evento `Output`, sem pausar.
fn emit_logpoint(amx: &Amx, cip: u32, frm: i32, template: &str) {
    let Ok(guard) = DBG.lock() else { return };
    let Some(dbg) = guard.as_ref() else { return };
    let vars = inspect::collect(dbg, amx, cip, frm);
    let lookup = |name: &str| -> Option<String> {
        vars.iter()
            .find(|v| v.name == name)
            .map(|v| v.value.clone())
    };
    let text = interpolate_log(template, &lookup);
    BRIDGE.send(&Event::Output { text });
}

fn reason_str(r: crate::control::StopReason) -> &'static str {
    use crate::control::StopReason::{Breakpoint, Entry, Step};
    match r {
        Breakpoint => "breakpoint",
        Step => "step",
        Entry => "entry",
    }
}

/// Pausa: coleta as variáveis em escopo, avisa o adaptador e bloqueia até
/// continuar/step. Roda na thread da VM (o servidor congela — esperado em dev).
fn on_pause(amx: &Amx, cip: u32, frm: i32, reason: &str, description: Option<&str>) {
    let (frames, ctx) = match DBG.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(dbg) => build_frames(dbg, amx, cip, frm),
            None => (Vec::new(), Vec::new()),
        },
        Err(_) => (Vec::new(), Vec::new()),
    };

    // Publica o contexto da pausa (cip/frm de cada frame) para a thread do socket
    // editar variáveis no frame selecionado enquanto a VM está bloqueada abaixo.
    if let (Ok(mut guard), Some(ptr)) = (PAUSE_CTX.lock(), amx.amx()) {
        *guard = Some((ptr.as_ptr() as usize, ctx));
    }

    BRIDGE.send(&Event::Paused {
        reason: reason.to_string(),
        frames,
        description: description.map(str::to_string),
    });

    // Bloqueia até o adaptador mandar continuar/step.
    let action = BRIDGE.wait_resume();

    // Saindo da pausa: invalida o contexto (a VM retoma e os ponteiros não
    // valem mais).
    if let Ok(mut ctx) = PAUSE_CTX.lock() {
        *ctx = None;
    }
    if let Ok(mut ctrl) = STATE.lock() {
        match action {
            Resume::Continue => ctrl.resume(),
            Resume::Step(mode) => ctrl.request_step(mode, frm),
        }
        // `Run` é o estado pós-continue; o step já foi armado acima.
        let _ = StepMode::Run;
    }
}

/// Monta a call stack da pausa: caminha a cadeia de frames do AMX e, para cada
/// um, resolve nome da função e linha no bloco de debug e coleta as variáveis em
/// escopo ali. Devolve os frames do protocolo e os contextos `(cip, frm)` na
/// mesma ordem, para [`set_variable`] e [`read_memory`] mirarem o frame escolhido.
fn build_frames(dbg: &AmxDbg, amx: &Amx, cip: u32, frm: i32) -> (Vec<Frame>, Vec<(u32, i32)>) {
    let stp = amx.stp().unwrap_or(0);
    let ctx = stack::walk(cip, frm, stp, |addr| amx.read_cell(addr));
    let frames = ctx
        .iter()
        .map(|&(fcip, ffrm)| Frame {
            name: dbg.lookup_function(fcip).unwrap_or("???").to_string(),
            line: dbg.lookup_line(fcip),
            vars: inspect::collect(dbg, amx, fcip, ffrm),
        })
        .collect();
    (frames, ctx)
}

/// Avalia a condição de um breakpoint contra as variáveis em escopo no `cip`/`frm`
/// atual. `true` = a condição vale (deve pausar). Conservador: o que não puder ser
/// avaliado também dá `true`, para não engolir o breakpoint.
fn eval_breakpoint_condition(amx: &Amx, cip: u32, frm: i32, expr: &str) -> bool {
    let Ok(guard) = DBG.lock() else { return true };
    let Some(dbg) = guard.as_ref() else {
        return true;
    };
    let vars = inspect::collect(dbg, amx, cip, frm);
    // Resolve o nome para o valor JÁ FORMATADO (ex.: "96.5", "true", "12"), que
    // `eval_condition` reinterpreta por tipo. Array (valor "[...]") não casa como
    // literal → cai no caminho conservador.
    let lookup = |name: &str| -> Option<String> {
        vars.iter()
            .find(|v| v.name == name)
            .map(|v| v.value.clone())
    };
    eval_condition(expr, &lookup)
}

/// Carrega o bloco de debug usado pela inspeção (chamar no `on_load` do plugin).
pub fn load_debug(dbg: AmxDbg) {
    if let Ok(mut guard) = DBG.lock() {
        *guard = Some(dbg);
    }
}

/// Monta o mapa de opcodes desta VM (inverso de `amx_opcodelist` numa imagem
/// relocada), para a detecção de erro de runtime. Uma vez por VM, no `on_amx_load`.
pub fn load_opcode_map(amx: &Amx) {
    if let Ok(mut guard) = OPCODE_MAP.lock() {
        *guard = Some(amx.opcode_map());
    }
}

/// Varre a linha-fonte a partir de `at` (offset de código: a primeira instrução
/// depois do `OP_BREAK`) e checa se alguma instrução vai abortar a VM. Simula
/// `pri`/`alt` a partir dos valores reais no break, já que a instrução que falha
/// fica no meio da linha. `None` = segura ou indecodificável.
fn detect_runtime_error(amx: &Amx, at: u32) -> Option<runtime_error::RuntimeError> {
    let guard = OPCODE_MAP.lock().ok()?;
    let map = guard.as_ref()?;
    let (pri, alt, frm) = (amx.pri()?, amx.alt()?, amx.frame()?);
    // Ponteiros de pilha/heap e limites, para detectar STACKERR/HEAPLOW/MEMACCESS.
    let (stk, hea, hlw, stp) = (amx.stack()?, amx.heap()?, amx.hlw()?, amx.stp()?);
    let read_code = |off: u32| amx.read_code(off);
    let read_data = |addr: i32| amx.read_cell(addr);
    let decode = |raw: i32| map.decode(raw);
    runtime_error::scan_line(
        at, pri, alt, frm, stk, hea, hlw, stp, &read_code, &read_data, &decode,
    )
}

/// Verifica os data breakpoints neste passo: devolve o nome da variável observada
/// que mudou de valor (e deve pausar), ou `None`. Barato quando não há watch
/// armado. Para expirar watches de locais, calcula os `frm` vivos caminhando a
/// pilha ([`stack::walk`]) — um frame cujo `frm` sumiu retornou.
fn check_data_watch(amx: &Amx, cip: u32, frm: i32) -> Option<String> {
    // Sem watches: não paga o custo de caminhar a pilha.
    if !STATE.lock().ok()?.has_data_watches() {
        return None;
    }
    let stp = amx.stp().unwrap_or(0);
    let live: Vec<i32> = stack::walk(cip, frm, stp, |a| amx.read_cell(a))
        .into_iter()
        .map(|(_, f)| f)
        .collect();
    STATE
        .lock()
        .ok()?
        .check_data_watches(|a| amx.read_cell(a), |f| live.contains(&f))
}

/// Atualiza os breakpoints (endereço + condição opcional) resolvidos pelo
/// adaptador.
pub fn set_breakpoints(bps: Vec<Breakpoint>) {
    if let Ok(mut ctrl) = STATE.lock() {
        ctrl.set_breakpoints(bps.into_iter().map(|b| Bp {
            addr: b.addr,
            condition: b.condition,
            hit_condition: b.hit_condition,
            log_message: b.log_message,
            hits: 0,
        }));
    }
}

/// Lê `count` bytes da memória de dados a partir da variável `name` (elemento
/// `index`, se array) no `frame`, mais `offset`, e responde com um
/// `Event::MemoryData` correlacionado por `id`. Vazio se não resolver/ler.
pub fn read_memory(
    id: u64,
    frame: usize,
    name: &str,
    index: Option<usize>,
    offset: i64,
    count: usize,
) {
    let bytes = read_memory_inner(frame, name, index, offset, count).unwrap_or_default();
    BRIDGE.send(&Event::MemoryData { id, bytes });
}

fn read_memory_inner(
    frame: usize,
    name: &str,
    index: Option<usize>,
    offset: i64,
    count: usize,
) -> Option<Vec<u8>> {
    let (amx_usize, frames) = PAUSE_CTX.lock().ok().and_then(|g| g.clone())?;
    let amx = Amx::data_only(amx_usize as *mut samp::raw::types::AMX);
    let (cip, frm) = *frames.get(frame)?;
    let guard = DBG.lock().ok()?;
    let dbg = guard.as_ref()?;
    let sym = dbg
        .symbols_in_scope(cip)
        .into_iter()
        .find(|s| s.name == name)?;
    let mut base = sym.effective_address(frm);
    if let Some(i) = index {
        if !sym.is_array() {
            return None;
        }
        base = base.wrapping_add(i32::try_from(i).ok()?.wrapping_mul(4));
    }
    let start = i32::try_from(i64::from(base) + offset).ok()?;

    // O SDK cuida do alinhamento de cells e para no primeiro endereço
    // inacessível — no fim do segmento devolve menos que `count`.
    amx.read_bytes(start, count)
}

/// Arma os data breakpoints pedidos pelo adaptador. Resolve cada `(frame, name)`
/// contra a pausa atual (o frame dá `cip`/`frm`; o símbolo em escopo dá o endereço
/// de dados e a classe global/local) e passa os watches resolvidos ao controlador.
/// Chamado pela thread do socket enquanto a VM está pausada.
pub fn set_data_breakpoints(reqs: Vec<pawnpro_dbg_protocol::DataWatch>) {
    let resolved = resolve_data_watches(reqs);
    if let Ok(mut ctrl) = STATE.lock() {
        ctrl.set_data_watches(resolved);
    }
}

/// Resolve os pedidos `(frame, name)` em [`DataWatch`]s com endereço absoluto,
/// classe (global → nunca expira; local → expira com o frame) e valor inicial.
/// Usa o contexto da pausa atual ([`PAUSE_CTX`]) e o bloco de debug. Um array só
/// é observável por um elemento (`index`); pedidos que não resolvem são ignorados.
fn resolve_data_watches(reqs: Vec<pawnpro_dbg_protocol::DataWatch>) -> Vec<DataWatch> {
    let Some((amx_usize, frames)) = PAUSE_CTX.lock().ok().and_then(|g| g.clone()) else {
        return Vec::new();
    };
    let amx = Amx::data_only(amx_usize as *mut samp::raw::types::AMX);
    let Ok(guard) = DBG.lock() else {
        return Vec::new();
    };
    let Some(dbg) = guard.as_ref() else {
        return Vec::new();
    };
    reqs.into_iter()
        .filter_map(|req| {
            let (cip, frm) = *frames.get(req.frame)?;
            let sym = dbg
                .symbols_in_scope(cip)
                .into_iter()
                .find(|s| s.name == req.name)?;
            let base = sym.effective_address(frm);
            // Elemento de array (`name[index]`) ou escalar. Arrays só são
            // observáveis por um elemento; escalares, sem índice.
            let (addr, name) = if let Some(i) = req.index {
                if !sym.is_array() {
                    return None;
                }
                let len = usize::try_from(sym.dims.first().map_or(0, |d| d.size)).unwrap_or(0);
                if i >= len {
                    return None;
                }
                let addr = base.wrapping_add(i32::try_from(i).ok()?.wrapping_mul(4));
                (addr, format!("{}[{i}]", req.name))
            } else {
                if sym.is_array() {
                    return None;
                }
                (base, req.name)
            };
            // Global: endereço absoluto, nunca expira. Local: relativo ao frame,
            // expira quando o frame `frm` retorna.
            let frame_frm = (sym.vclass != VClass::Global).then_some(frm);
            let last = amx.read_cell(addr).unwrap_or(0);
            Some(DataWatch {
                addr,
                frame_frm,
                last,
                name,
            })
        })
        .collect()
}

/// Edita uma variável em escopo no `frame` pedido (0 = topo) da pausa atual,
/// gravando `value` na célula via `Amx::write_cell` (com checagem de limites).
/// `index` mira um elemento de array; `None`, um escalar. Devolve `None` se não
/// houver pausa, o frame ou o índice estiverem fora de faixa, a variável não
/// estiver em escopo, o tipo não casar com o `index` ou o endereço for
/// inacessível. Chamado pela thread do socket com a VM pausada.
#[must_use]
pub fn set_variable(frame: usize, name: &str, index: Option<usize>, value: i32) -> Option<i32> {
    let (amx_usize, cip, frm) = {
        let guard = PAUSE_CTX.lock().ok()?;
        let (amx_usize, frames) = guard.as_ref()?;
        let (cip, frm) = *frames.get(frame)?;
        (*amx_usize, cip, frm)
    };
    let amx = Amx::data_only(amx_usize as *mut samp::raw::types::AMX);

    let guard = DBG.lock().ok()?;
    let dbg = guard.as_ref()?;
    let sym = dbg
        .symbols_in_scope(cip)
        .into_iter()
        .find(|s| s.name == name)?;

    // Endereço-alvo: elemento `index` de um array, ou a célula de um escalar.
    let addr = if let Some(i) = index {
        if !sym.is_array() {
            return None; // índice pedido em algo que não é array
        }
        let len = usize::try_from(sym.dims.first().map_or(0, |d| d.size)).unwrap_or(0);
        if i >= len {
            return None; // fora do limite do array
        }
        sym.effective_address(frm)
            .wrapping_add(i32::try_from(i).ok()?.wrapping_mul(4))
    } else {
        if sym.is_array() {
            return None; // array precisa de índice (o array inteiro não é editável)
        }
        sym.effective_address(frm)
    };

    amx.write_cell(addr, value).then_some(value)
}
