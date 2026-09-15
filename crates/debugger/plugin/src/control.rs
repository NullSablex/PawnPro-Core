//! Lógica de controle de execução do debugger — pura e testável, sem FFI.
//!
//! O debug hook do AMX é chamado a cada linha. Esta camada decide, a partir do
//! estado (`cip`, `frm`) e dos breakpoints/modo de step, se a execução deve
//! **pausar**. Mantida separada da casca FFI ([`crate::hook`]) para poder ser
//! testada sem servidor, usando o `amxdbg` para mapear `cip` → linha.

// A API de step (StepMode::{In,Over,Out}, request_step, resume) é exercida pelos
// testes e será consumida pela ponte com o adaptador (próximo passo); por ora
// não tem chamador no build de produção.
#![allow(dead_code)]

/// Modo de execução pedido pelo adaptador (traduzido dos comandos DAP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepMode {
    /// Rodando livremente; só pausa em breakpoint.
    #[default]
    Run,
    /// Para na próxima linha, qualquer frame (step in).
    In,
    /// Para na próxima linha do mesmo frame ou de um frame pai (step over).
    Over,
    /// Para ao retornar ao frame pai (step out).
    Out,
}

/// Por que a execução pausou (vira o `reason` do evento `stopped` do DAP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint,
    Step,
    Entry,
}

/// Um breakpoint resolvido, com seus modificadores e o contador de acertos.
/// O endereço de código já vem resolvido pelo adaptador (via `amxdbg`), e é o que
/// se compara com o `cip` da VM.
#[derive(Debug, Clone, Default)]
pub struct Bp {
    pub addr: u32,
    /// Condição lógica (`var OP valor`); `None` = incondicional.
    pub condition: Option<String>,
    /// Condição sobre a contagem de acertos (`5`, `>=3`, `%2`); `None` = sempre.
    pub hit_condition: Option<String>,
    /// Mensagem de logpoint; `Some` = não pausa, só registra (ver [`BreakAction`]).
    pub log_message: Option<String>,
    /// Quantas vezes este endereço já foi atingido (e passou na `condition`).
    /// Contado pelo hook; alimenta a avaliação de `hit_condition`.
    pub hits: u32,
}

/// Um data breakpoint resolvido: o endereço absoluto de dados a observar, o último
/// valor visto e o nome (para a mensagem). `frame_frm` guarda o `frm` do frame
/// dono quando a variável é **local** — o watch expira quando esse frame retorna
/// (o slot da pilha é reusado); variáveis **globais** têm `frame_frm: None` e nunca
/// expiram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataWatch {
    pub addr: i32,
    pub frame_frm: Option<i32>,
    pub last: i32,
    pub name: String,
}

/// O que fazer ao atingir um endereço, decidido por [`Controller::on_hit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakAction {
    /// Pausar a execução (breakpoint normal/condicional satisfeito).
    Pause,
    /// Logpoint: emitir a mensagem (template `{expr}` ainda por interpolar) e
    /// seguir a execução. Não pausa.
    Log(String),
    /// Nada a fazer por breakpoint neste endereço (segue para a lógica de step).
    None,
}

/// Estado de controle compartilhado entre o hook e os comandos do adaptador.
#[derive(Debug, Default)]
pub struct Controller {
    /// Breakpoints resolvidos. São poucos; `Vec` basta.
    breakpoints: Vec<Bp>,
    /// Data breakpoints resolvidos (endereço + último valor). Ver [`DataWatch`].
    data_watches: Vec<DataWatch>,
    mode: StepMode,
    /// `frm` no instante em que um step foi pedido — referência para over/out.
    step_frame: i32,
    /// Já paramos ao menos uma vez? (para emitir `Entry` no primeiro hit).
    started: bool,
}

impl Controller {
    #[must_use]
    pub fn new() -> Self {
        Self::new_const()
    }

    /// Construtor `const` — necessário para inicializar o estado global do hook
    /// num `static`. `HashSet::new` é const desde Rust 1.64.
    #[must_use]
    pub const fn new_const() -> Self {
        Self {
            breakpoints: Vec::new(),
            data_watches: Vec::new(),
            mode: StepMode::Run,
            step_frame: 0,
            started: false,
        }
    }

    /// Substitui o conjunto de breakpoints. Zera os contadores de acerto (um novo
    /// conjunto recomeça a contagem de hit-count do zero).
    pub fn set_breakpoints(&mut self, bps: impl IntoIterator<Item = Bp>) {
        self.breakpoints = bps.into_iter().collect();
    }

    /// Decide a ação para um endereço atingido. `eval` avalia a `condition`
    /// lógica contra as variáveis em escopo (o hook a fornece, pois precisa da VM);
    /// só é chamado se houver `condition`. A ordem segue o DAP: primeiro a
    /// condição lógica filtra, depois o acerto conta para o hit-count, e por fim
    /// `log_message` decide entre pausar e registrar.
    pub fn on_hit(&mut self, cip: u32, eval: impl FnOnce(&str) -> bool) -> BreakAction {
        let Some(bp) = self.breakpoints.iter_mut().find(|b| b.addr == cip) else {
            return BreakAction::None;
        };
        // Condição lógica não satisfeita → nem conta como acerto.
        if let Some(expr) = &bp.condition
            && !eval(expr)
        {
            return BreakAction::None;
        }
        // Acerto válido: incrementa e testa a condição de contagem.
        bp.hits += 1;
        if let Some(hc) = &bp.hit_condition
            && !hit_passes(hc, bp.hits)
        {
            return BreakAction::None;
        }
        match &bp.log_message {
            Some(msg) => BreakAction::Log(msg.clone()),
            None => BreakAction::Pause,
        }
    }

    /// Substitui o conjunto de data breakpoints (já resolvidos a endereço + valor
    /// inicial pelo hook, que tem a VM).
    pub fn set_data_watches(&mut self, watches: Vec<DataWatch>) {
        self.data_watches = watches;
    }

    /// Verifica os data breakpoints neste passo. `read_cell` lê a célula atual de
    /// um endereço de dados; `is_frm_live` diz se o frame dono (por `frm`) ainda
    /// está vivo na pilha — watches de locais cujo frame retornou são descartados
    /// (o slot foi reusado, observá-lo daria falso-positivo). Devolve o nome da
    /// primeira variável que mudou (e portanto deve pausar), já atualizando o
    /// último valor; `None` se nada mudou.
    #[must_use]
    pub fn check_data_watches(
        &mut self,
        read_cell: impl Fn(i32) -> Option<i32>,
        is_frm_live: impl Fn(i32) -> bool,
    ) -> Option<String> {
        // Expira watches de locais cujo frame retornou (globais têm `frame_frm`
        // `None` e permanecem).
        self.data_watches
            .retain(|w| w.frame_frm.is_none_or(&is_frm_live));
        for w in &mut self.data_watches {
            let Some(cur) = read_cell(w.addr) else {
                continue;
            };
            if cur != w.last {
                w.last = cur;
                return Some(w.name.clone());
            }
        }
        None
    }

    /// Há algum data breakpoint armado? (o hook evita o trabalho de checagem se não.)
    #[must_use]
    pub fn has_data_watches(&self) -> bool {
        !self.data_watches.is_empty()
    }

    /// Define o modo de step, capturando o frame atual como referência.
    pub fn request_step(&mut self, mode: StepMode, current_frame: i32) {
        self.mode = mode;
        self.step_frame = current_frame;
    }

    /// Volta ao modo livre (comando `continue`).
    pub fn resume(&mut self) {
        self.mode = StepMode::Run;
    }

    /// Marca que paramos num breakpoint (zera o step e registra que já iniciou).
    /// Chamado pelo hook quando um breakpoint — já com a condição satisfeita —
    /// dispara. Mantido separado de `should_stop` porque a avaliação da condição
    /// precisa das variáveis da VM, que só o hook acessa.
    pub fn hit_breakpoint(&mut self) {
        self.mode = StepMode::Run;
        self.started = true;
    }

    /// Decide se a VM deve pausar por **step** neste passo. `frm` é a base do
    /// frame. `None` = continuar. O breakpoint é tratado à parte (ver
    /// `breakpoint_at`/`hit_breakpoint`).
    #[must_use]
    pub fn should_stop(&mut self, _cip: u32, frm: i32) -> Option<StopReason> {
        let stop = match self.mode {
            StepMode::Run => false,
            // Step in: para em qualquer próxima linha.
            StepMode::In => true,
            // Step over: para se voltamos ao mesmo frame ou a um frame pai
            // (frm >= o frame de referência; a pilha cresce para baixo).
            StepMode::Over => frm >= self.step_frame,
            // Step out: para só ao subir para um frame pai.
            StepMode::Out => frm > self.step_frame,
        };
        if stop {
            self.mode = StepMode::Run;
            let reason = if self.started {
                StopReason::Step
            } else {
                StopReason::Entry
            };
            self.started = true;
            Some(reason)
        } else {
            None
        }
    }
}

/// Decide se um acerto satisfaz a condição de contagem (`hitCondition` do DAP).
/// Formatos aceitos (com ou sem espaços): `N` (exatamente N — o DAP trata como
/// "a partir de N", então usamos `>=`), `==N`, `>=N`, `<=N`, `>N`, `<N`, `%N`
/// (a cada N acertos). `hits` é a contagem ATUAL (já incrementada neste acerto).
///
/// Conservador: formato não reconhecido → `true` (não engole o breakpoint).
#[must_use]
pub fn hit_passes(cond: &str, hits: u32) -> bool {
    let c = cond.trim();
    // `%N`: a cada N acertos (N>0).
    if let Some(n) = c
        .strip_prefix('%')
        .and_then(|n| n.trim().parse::<u32>().ok())
    {
        return n != 0 && hits.is_multiple_of(n);
    }
    // Operadores explícitos (2 chars antes de 1 char).
    for (op, f) in [
        ("==", (|h, n| h == n) as fn(u32, u32) -> bool),
        (">=", |h, n| h >= n),
        ("<=", |h, n| h <= n),
        (">", |h, n| h > n),
        ("<", |h, n| h < n),
    ] {
        if let Some(rest) = c.strip_prefix(op) {
            return rest.trim().parse::<u32>().map_or(true, |n| f(hits, n));
        }
    }
    // Só um número: o DAP define "pare a partir do N-ésimo acerto".
    c.parse::<u32>().map_or(true, |n| hits >= n)
}

/// Interpola a mensagem de um logpoint: cada `{expr}` é substituído pelo valor da
/// variável `expr` (via `lookup`, mesmo das condições). Chaves não fechadas ou
/// variáveis fora de escopo ficam como o texto literal `{expr}`. `{{`/`}}` são
/// chaves escapadas (viram `{`/`}`), seguindo a convenção do VS Code.
#[must_use]
pub fn interpolate_log(template: &str, lookup: &impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' if chars.get(i + 1) == Some(&'{') => {
                out.push('{');
                i += 2;
            }
            '}' if chars.get(i + 1) == Some(&'}') => {
                out.push('}');
                i += 2;
            }
            '{' => {
                // Lê até o `}` de fecho.
                if let Some(close) = chars[i + 1..].iter().position(|&c| c == '}') {
                    let expr: String = chars[i + 1..i + 1 + close].iter().collect();
                    let name = expr.trim();
                    if let Some(v) = lookup(name) {
                        out.push_str(&v);
                    } else {
                        out.push('{');
                        out.push_str(&expr);
                        out.push('}');
                    }
                    i += 1 + close + 1;
                } else {
                    // Sem fecho: literal.
                    out.push('{');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Operando de uma condição: inteiro, float ou booleano. Permite comparar os três
/// tipos que o Pawn expõe na inspeção (`int`, `Float:`, `bool:`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Operand {
    Int(i32),
    Float(f32),
    Bool(bool),
}

/// Avalia uma condição simples de breakpoint `lado OP lado`, com `OP` em
/// `==`, `!=`, `<`, `>`, `<=`, `>=`. Cada lado é um literal (`100`, `0x64`,
/// `96.5`, `true`/`false`) ou o nome de uma variável; `lookup` devolve o **valor
/// já formatado** da variável (ex.: `"96.5"`, `"true"`, `"12"`).
///
/// Conservador por design: se a expressão não for reconhecida ou um operando não
/// resolver, retorna `true` — assim o breakpoint **para** (melhor parar a mais do
/// que engolir o breakpoint por uma condição malformada).
#[must_use]
pub fn eval_condition(expr: &str, lookup: &impl Fn(&str) -> Option<String>) -> bool {
    // Operadores de 2 chars antes dos de 1 char (para `<=`/`>=`/`==`/`!=`).
    const OPS: [&str; 6] = ["==", "!=", "<=", ">=", "<", ">"];
    for op in OPS {
        if let Some((lhs, rhs)) = expr.split_once(op) {
            let (Some(l), Some(r)) = (
                resolve_operand(lhs.trim(), lookup),
                resolve_operand(rhs.trim(), lookup),
            ) else {
                return true; // operando não resolvido → para
            };
            return compare(l, r, op);
        }
    }
    true // sem operador reconhecido → para
}

/// Compara dois operandos. Int e Float são comparáveis entre si (Int promovido a
/// Float). Bool só compara igualdade com Bool. Tipos incompatíveis → `true`.
fn compare(l: Operand, r: Operand, op: &str) -> bool {
    use Operand::Bool;
    // Bool: só `==`/`!=` contra outro bool.
    if let (Bool(a), Bool(b)) = (l, r) {
        return match op {
            "==" => a == b,
            "!=" => a != b,
            _ => true,
        };
    }
    // Numérico: trata tudo como f32 (Int vira Float sem perda para a faixa usual).
    let (Some(a), Some(b)) = (as_f32(l), as_f32(r)) else {
        return true;
    };
    match op {
        "==" => (a - b).abs() < f32::EPSILON,
        "!=" => (a - b).abs() >= f32::EPSILON,
        "<=" => a <= b,
        ">=" => a >= b,
        "<" => a < b,
        ">" => a > b,
        _ => true,
    }
}

/// Valor numérico de um operando (`Bool` não é numérico).
#[allow(clippy::cast_precision_loss)] // comparação de breakpoint; precisão de f32 basta
fn as_f32(op: Operand) -> Option<f32> {
    match op {
        Operand::Int(i) => Some(i as f32),
        Operand::Float(f) => Some(f),
        Operand::Bool(_) => None,
    }
}

/// Resolve um operando: literal (`true`/`false`, `0x` hex, decimal, float) ou
/// nome de variável (cujo valor formatado é reinterpretado pelo mesmo parser).
fn resolve_operand(s: &str, lookup: &impl Fn(&str) -> Option<String>) -> Option<Operand> {
    if let Some(op) = parse_literal(s) {
        return Some(op);
    }
    // Variável: pega o valor formatado e reinterpreta (ex.: "96.5" → Float).
    let value = lookup(s)?;
    parse_literal(&value)
}

/// Interpreta um texto como literal de operando, ou `None` se não casar.
fn parse_literal(s: &str) -> Option<Operand> {
    match s {
        "true" => return Some(Operand::Bool(true)),
        "false" => return Some(Operand::Bool(false)),
        _ => {}
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i32::from_str_radix(hex, 16).ok().map(Operand::Int);
    }
    if let Ok(i) = s.parse::<i32>() {
        return Some(Operand::Int(i));
    }
    if let Ok(f) = s.parse::<f32>() {
        return Some(Operand::Float(f));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Breakpoint só com endereço (sem modificadores).
    fn plain(addr: u32) -> Bp {
        Bp {
            addr,
            ..Bp::default()
        }
    }

    /// `eval` que sempre passa (condição verdadeira) — para casos sem condição.
    fn always(_: &str) -> bool {
        true
    }

    #[test]
    fn on_hit_unconditional_pauses() {
        let mut c = Controller::new();
        c.set_breakpoints([plain(20), {
            let mut b = plain(48);
            b.condition = Some("x == 5".to_string());
            b
        }]);
        // Endereço 20: incondicional → pausa.
        assert_eq!(c.on_hit(20, always), BreakAction::Pause);
        // Endereço sem breakpoint → None.
        assert_eq!(c.on_hit(24, always), BreakAction::None);
        // Endereço 48: condição verdadeira → pausa; falsa → None.
        assert_eq!(c.on_hit(48, |_| true), BreakAction::Pause);
        assert_eq!(c.on_hit(48, |_| false), BreakAction::None);
    }

    #[test]
    fn on_hit_counts_only_when_condition_holds() {
        let mut c = Controller::new();
        c.set_breakpoints([{
            let mut b = plain(10);
            b.condition = Some("x == 5".to_string());
            b.hit_condition = Some(">=2".to_string());
            b
        }]);
        // Condição falsa não conta o acerto: hits fica 0.
        assert_eq!(c.on_hit(10, |_| false), BreakAction::None);
        // 1º acerto válido (hits=1) ainda não atinge >=2.
        assert_eq!(c.on_hit(10, |_| true), BreakAction::None);
        // 2º acerto válido (hits=2) → pausa.
        assert_eq!(c.on_hit(10, |_| true), BreakAction::Pause);
    }

    #[test]
    fn on_hit_logpoint_returns_log_not_pause() {
        let mut c = Controller::new();
        c.set_breakpoints([{
            let mut b = plain(12);
            b.log_message = Some("x={x}".to_string());
            b
        }]);
        assert_eq!(c.on_hit(12, always), BreakAction::Log("x={x}".to_string()));
    }

    #[test]
    fn hit_passes_formats() {
        // Número puro = "a partir de N".
        assert!(!hit_passes("3", 2));
        assert!(hit_passes("3", 3));
        assert!(hit_passes("3", 4));
        // Operadores.
        assert!(hit_passes("==2", 2));
        assert!(!hit_passes("==2", 3));
        assert!(hit_passes(">=2", 2));
        assert!(hit_passes(">2", 3));
        assert!(!hit_passes(">2", 2));
        assert!(hit_passes("<=2", 2));
        assert!(hit_passes("<3", 2));
        // Módulo: a cada N.
        assert!(hit_passes("%2", 2));
        assert!(!hit_passes("%2", 3));
        assert!(hit_passes("%2", 4));
        assert!(!hit_passes("%0", 5)); // N=0 nunca dispara
        // Espaços e malformado (conservador → true).
        assert!(hit_passes(" >= 2 ", 2));
        assert!(hit_passes("abc", 1));
    }

    #[test]
    fn interpolate_log_substitutes_vars() {
        let lk = |n: &str| vars_lookup(n);
        assert_eq!(interpolate_log("x={x}", &lk), "x=5");
        assert_eq!(interpolate_log("{x} e {y}", &lk), "5 e 10");
        // Variável fora de escopo: fica literal.
        assert_eq!(interpolate_log("v={z}", &lk), "v={z}");
        // Chaves escapadas.
        assert_eq!(interpolate_log("{{x}}", &lk), "{x}");
        // Sem fecho: literal.
        assert_eq!(interpolate_log("a {x", &lk), "a {x");
        // Sem chaves: inalterado.
        assert_eq!(interpolate_log("sem var", &lk), "sem var");
    }

    /// Helper: lookup que devolve o valor JÁ FORMATADO (string), como o plugin faz.
    fn vars_lookup(name: &str) -> Option<String> {
        let v = match name {
            "x" => "5",
            "y" => "10",
            "hp" => "-50",
            "id" => "0",
            "big" => "2000000000",
            "flag" => "1",
            "taxa" => "96.5",    // Float
            "lotado" => "false", // bool
            "ativo" => "true",   // bool
            _ => return None,
        };
        Some(v.to_string())
    }

    #[test]
    fn eval_condition_operators() {
        let vars = vars_lookup;
        assert!(eval_condition("x == 5", &vars));
        assert!(!eval_condition("x == 6", &vars));
        assert!(eval_condition("x != 6", &vars));
        assert!(eval_condition("x < 10", &vars));
        assert!(eval_condition("y >= 10", &vars));
        assert!(eval_condition("y > x", &vars)); // var vs var
        assert!(eval_condition("x == 0x5", &vars)); // hex
        // Operando que não resolve → conservador: para (true).
        assert!(eval_condition("z == 1", &vars));
        // Sem operador reconhecido → para (true).
        assert!(eval_condition("alguma_coisa", &vars));
    }

    #[test]
    fn eval_condition_float_and_bool() {
        let vars = vars_lookup;
        // --- Float ---
        assert!(eval_condition("taxa == 96.5", &vars));
        assert!(!eval_condition("taxa == 96.4", &vars));
        assert!(eval_condition("taxa > 96", &vars)); // float vs int
        assert!(eval_condition("taxa < 100", &vars));
        assert!(eval_condition("taxa >= 96.5", &vars));
        assert!(!eval_condition("taxa < 50", &vars));
        // --- Bool ---
        assert!(eval_condition("lotado == false", &vars));
        assert!(!eval_condition("lotado == true", &vars));
        assert!(eval_condition("ativo == true", &vars));
        assert!(eval_condition("ativo != false", &vars));
        // Bool com operador de ordem não faz sentido → conservador (true).
        assert!(eval_condition("ativo < lotado", &vars));
    }

    #[test]
    fn eval_condition_edge_cases() {
        let vars = vars_lookup;

        // --- Valores negativos ---
        assert!(eval_condition("hp == -50", &vars));
        assert!(eval_condition("hp < 0", &vars));
        assert!(!eval_condition("hp > 0", &vars));
        assert!(eval_condition("hp <= -50", &vars));

        // --- Zero ---
        assert!(eval_condition("id == 0", &vars));
        assert!(!eval_condition("id != 0", &vars));

        // --- Espaçamentos variados (sem espaço, espaço extra) ---
        assert!(eval_condition("flag==1", &vars)); // colado
        assert!(eval_condition("flag  ==  1", &vars)); // espaço duplo
        assert!(eval_condition("  flag == 1  ", &vars)); // espaços nas bordas

        // --- Limites grandes ---
        assert!(eval_condition("big > 1000000", &vars));
        assert!(eval_condition("big == 2000000000", &vars));

        // --- Hex ---
        assert!(eval_condition("flag == 0x1", &vars));
        assert!(eval_condition("flag == 0X1", &vars)); // maiúsculo

        // --- Precedência de operador: '<=' não deve casar como '<' ---
        // hp(-50) <= -50 é verdadeiro; se casasse como '<', daria -50 < (= -50)
        // o rhs viraria "= -50" e falharia o parse → conservador true.
        assert!(eval_condition("hp <= -50", &vars));
        // hp >= -50 verdadeiro.
        assert!(eval_condition("hp >= -50", &vars));
        // hp >= -49 falso (-50 não é >= -49).
        assert!(!eval_condition("hp >= -49", &vars));
    }

    #[test]
    fn eval_condition_malformed_is_conservative() {
        let vars = vars_lookup;
        // Lado direito vazio → operando não resolve → para (true).
        assert!(eval_condition("x ==", &vars));
        // Lado esquerdo vazio → para (true).
        assert!(eval_condition("== 5", &vars));
        // Expressão composta não suportada (and/or) → sem operador de comparação
        // reconhecido no topo → para (true).
        assert!(eval_condition("x == 5 && id == 0", &vars));
        // String literal não vira inteiro → para (true).
        assert!(eval_condition("x == abc", &vars));
    }

    #[test]
    fn step_in_stops_next_line() {
        let mut c = Controller::new();
        c.started = true; // já passou da entrada
        c.request_step(StepMode::In, 100);
        assert_eq!(c.should_stop(99, 100), Some(StopReason::Step));
    }

    #[test]
    fn step_over_skips_deeper_frame() {
        let mut c = Controller::new();
        c.started = true;
        c.request_step(StepMode::Over, 100);
        // Entrou numa sub-função (frame mais profundo: frm menor) → não para.
        assert_eq!(c.should_stop(50, 60), None);
        // Voltou ao mesmo frame → para.
        c.request_step(StepMode::Over, 100);
        assert_eq!(c.should_stop(52, 100), Some(StopReason::Step));
    }

    #[test]
    fn step_out_waits_parent_frame() {
        let mut c = Controller::new();
        c.started = true;
        c.request_step(StepMode::Out, 60);
        assert_eq!(c.should_stop(50, 60), None); // mesmo frame: continua
        c.request_step(StepMode::Out, 60);
        assert_eq!(c.should_stop(52, 100), Some(StopReason::Step)); // frame pai
    }

    #[test]
    fn first_stop_is_entry() {
        let mut c = Controller::new();
        c.request_step(StepMode::In, 100);
        assert_eq!(c.should_stop(10, 100), Some(StopReason::Entry));
    }

    #[test]
    fn run_mode_never_stops_without_breakpoint() {
        let mut c = Controller::new();
        assert_eq!(c.should_stop(123, 100), None);
    }

    /// Helper: watch global (nunca expira) num endereço com valor inicial.
    fn global_watch(addr: i32, name: &str, last: i32) -> DataWatch {
        DataWatch {
            addr,
            frame_frm: None,
            last,
            name: name.to_string(),
        }
    }

    #[test]
    fn data_watch_detects_change_once() {
        let mut c = Controller::new();
        c.set_data_watches(vec![global_watch(200, "g", 5)]);
        // Valor mudou de 5 → 9: dispara e atualiza o último.
        assert_eq!(
            c.check_data_watches(|a| (a == 200).then_some(9), |_| true),
            Some("g".to_string())
        );
        // Mesmo valor (9) agora: não dispara de novo.
        assert_eq!(
            c.check_data_watches(|a| (a == 200).then_some(9), |_| true),
            None
        );
    }

    #[test]
    fn data_watch_no_change_is_silent() {
        let mut c = Controller::new();
        c.set_data_watches(vec![global_watch(200, "g", 5)]);
        assert_eq!(
            c.check_data_watches(|a| (a == 200).then_some(5), |_| true),
            None
        );
    }

    #[test]
    fn data_watch_unreadable_address_does_not_fire() {
        let mut c = Controller::new();
        c.set_data_watches(vec![global_watch(200, "g", 5)]);
        // Endereço inacessível (None) → conservador, não inventa mudança.
        assert_eq!(c.check_data_watches(|_| None, |_| true), None);
    }

    #[test]
    fn data_watch_local_expires_when_frame_returns() {
        let mut c = Controller::new();
        // Local no frame frm=500; valor "mudaria" para 9.
        c.set_data_watches(vec![DataWatch {
            addr: 496,
            frame_frm: Some(500),
            last: 5,
            name: "x".to_string(),
        }]);
        // Frame 500 já retornou (não está vivo): o watch é descartado e não dispara,
        // mesmo com o slot agora contendo outro valor.
        assert_eq!(
            c.check_data_watches(|a| (a == 496).then_some(9), |_| false),
            None
        );
        assert!(!c.has_data_watches()); // podado
    }

    #[test]
    fn data_watch_local_lives_while_frame_alive() {
        let mut c = Controller::new();
        c.set_data_watches(vec![DataWatch {
            addr: 496,
            frame_frm: Some(500),
            last: 5,
            name: "x".to_string(),
        }]);
        // Frame 500 ainda vivo → observa e dispara na mudança.
        assert_eq!(
            c.check_data_watches(|a| (a == 496).then_some(9), |frm| frm == 500),
            Some("x".to_string())
        );
        assert!(c.has_data_watches());
    }
}
