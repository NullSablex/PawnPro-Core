//! Protocolo próprio entre o plugin (no servidor) e o adaptador DAP — JSON por
//! linha (NDJSON) sobre um socket local (Unix domain socket no Linux/macOS,
//! named pipe no Windows; ver [`transport`]). Simples de propósito: o adaptador
//! é quem fala DAP com o editor; aqui só trafegam comandos e eventos crus.
//!
//! Crate compartilhado para que plugin (`pawnpro-debug-plugin`) e adaptador
//! (`dap-adapter`) usem exatamente os mesmos tipos.
//!
//! Direções:
//! - **Adaptador → plugin**: [`Command`] (breakpoints, continue, step).
//! - **Plugin → adaptador**: [`Event`] (parou com variáveis, saiu).

use serde::{Deserialize, Serialize};

pub mod messages;
pub mod transport;

/// Modo de step pedido pelo adaptador.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    In,
    Over,
    Out,
}

/// Comando do adaptador para o plugin.
/// Um breakpoint resolvido: endereço de código, mais modificadores opcionais.
///
/// - `condition`: expressão `var OP valor`, avaliada no plugin; só dispara se
///   verdadeira.
/// - `hit_condition`: condição sobre a CONTAGEM de acertos (`5`, `>=3`, `%2`);
///   dispara só quando o nº de vezes que o endereço foi atingido a satisfaz.
/// - `log_message`: se presente, o breakpoint é um **logpoint** — em vez de
///   pausar, o plugin interpola a mensagem (trechos `{expr}` viram o valor da
///   variável) e a emite como saída, seguindo a execução.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breakpoint {
    pub addr: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Command {
    /// Substitui o conjunto de breakpoints (endereço + condição opcional).
    SetBreakpoints { breakpoints: Vec<Breakpoint> },
    /// Retoma a execução livre.
    Continue,
    /// Executa um passo no modo dado.
    Step { mode: Step },
    /// O adaptador terminou a configuração inicial (já enviou os breakpoints).
    /// Libera a VM que ficou segura na carga, para não perder breakpoints em
    /// código que roda uma única vez no início (ex.: `OnGameModeInit`).
    Configured,
    /// Edita uma variável em escopo na pausa atual: grava `value` na célula de
    /// `name`. `frame` é o índice do frame da pilha (0 = topo, onde a VM parou),
    /// para editar a variável no escopo correto. Só vale enquanto a VM está pausada.
    SetVariable {
        frame: usize,
        name: String,
        /// Índice do elemento, quando a variável é um array (`arr[index]`);
        /// `None` edita um escalar.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        value: i32,
    },
    /// Substitui o conjunto de data breakpoints (pausar quando uma variável muda).
    /// Cada alvo vem como frame + nome porque só o plugin sabe o `frm`/endereço
    /// para resolvê-lo; enviado enquanto a VM está pausada (o editor arma o data
    /// breakpoint a partir do painel Variáveis).
    SetDataBreakpoints { watches: Vec<DataWatch> },
    /// Liga/desliga a pausa em erros de runtime (filtro de exceção do editor).
    /// `false` deixa a VM abortar normalmente, sem pausar antes.
    SetExceptionFilter { runtime: bool },
    /// Lê memória de dados crua: `count` bytes a partir do endereço da variável
    /// `name` (elemento `index`, se array) no frame `frame`, mais `offset`. O
    /// plugin responde com um [`Event::MemoryData`] correlacionado por `id`.
    ReadMemory {
        id: u64,
        frame: usize,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        offset: i64,
        count: usize,
    },
}

/// Um data breakpoint pedido: a variável `name` em escopo no frame `frame`
/// (0 = topo). O plugin resolve o endereço de dados e passa a observar mudanças.
/// `index` observa um elemento de array (`name[index]`); `None` observa um escalar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataWatch {
    pub frame: usize,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

/// Evento do plugin para o adaptador.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event {
    /// A VM pausou. `frames` é a pilha de chamadas completa — do topo (frame 0,
    /// onde a VM parou) até o público de entrada. Cada frame traz sua linha-fonte
    /// e as variáveis em escopo naquele frame, enviadas junto para evitar
    /// idas-e-voltas de inspeção enquanto a VM está bloqueada.
    Paused {
        reason: String,
        frames: Vec<Frame>,
        /// Texto descritivo opcional (ex.: mensagem de um erro de runtime quando
        /// `reason == "exception"`). Vira o `description`/`text` do `stopped` DAP.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Saída de um **logpoint**: a mensagem já interpolada. A VM NÃO pausa; o
    /// adaptador encaminha como um evento `output` do DAP (canal do console).
    Output { text: String },
    /// Primeira mensagem do plugin ao aceitar a conexão: quem ele é.
    ///
    /// Sem isto, plugin e adaptador de versões diferentes conversavam até a
    /// primeira mensagem que um dos lados não entendia — e o sintoma era a
    /// depuração não fazer nada, sem erro em lugar nenhum.
    ///
    /// Um adaptador antigo ignora este evento (evento desconhecido não derruba
    /// a conexão), então anunciar não quebra quem já está instalado.
    Hello { version: String },
    /// O script terminou / a VM foi descarregada.
    Exited,
    /// Resposta a um [`Command::ReadMemory`]: os `bytes` lidos, correlacionados
    /// pelo `id` do pedido. Vazio se o endereço não pôde ser resolvido/lido.
    MemoryData { id: u64, bytes: Vec<u8> },
}

/// Um par variável→valor para a inspeção. Arrays trazem os elementos em
/// `children` (expansíveis na árvore do editor); escalares têm `children` vazio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Var {
    pub name: String,
    pub value: String,
    /// Elementos de um array (`[0]`, `[1]`, …); vazio para escalares.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Var>,
}

/// Um frame da pilha de chamadas na pausa. `name` é o nome da função (resolvido
/// do bloco de debug pelo endereço), `line` a linha-fonte do frame e `vars` as
/// variáveis em escopo nele.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub name: String,
    pub line: Option<i32>,
    pub vars: Vec<Var>,
}

/// Serializa uma mensagem como uma linha JSON (com `\n` ao final).
///
/// # Errors
/// Erro de serialização do serde (não deve ocorrer para estes tipos).
pub fn to_line<T: Serialize>(msg: &T) -> serde_json::Result<String> {
    let mut s = serde_json::to_string(msg)?;
    s.push('\n');
    Ok(s)
}

/// Desserializa uma linha JSON numa mensagem.
///
/// # Errors
/// Erro de desserialização (linha malformada ou variante desconhecida).
pub fn from_line<T: for<'de> Deserialize<'de>>(line: &str) -> serde_json::Result<T> {
    serde_json::from_str(line.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_roundtrip() {
        for cmd in [
            Command::SetBreakpoints {
                breakpoints: vec![
                    Breakpoint {
                        addr: 20,
                        condition: None,
                        hit_condition: None,
                        log_message: None,
                    },
                    Breakpoint {
                        addr: 48,
                        condition: Some("x == 5".into()),
                        hit_condition: Some(">=3".into()),
                        log_message: Some("x={x}".into()),
                    },
                ],
            },
            Command::Continue,
            Command::Step { mode: Step::Over },
            Command::SetVariable {
                frame: 1,
                name: "x".into(),
                index: None,
                value: 7,
            },
            Command::SetVariable {
                frame: 0,
                name: "arr".into(),
                index: Some(2),
                value: 9,
            },
            Command::SetDataBreakpoints {
                watches: vec![
                    DataWatch {
                        frame: 0,
                        name: "health".into(),
                        index: None,
                    },
                    DataWatch {
                        frame: 2,
                        name: "placar".into(),
                        index: Some(3),
                    },
                ],
            },
        ] {
            let line = to_line(&cmd).unwrap();
            assert!(line.ends_with('\n'));
            let back: Command = from_line(&line).unwrap();
            assert_eq!(cmd, back);
        }
    }

    #[test]
    fn event_roundtrip() {
        for ev in [
            Event::Paused {
                reason: "breakpoint".into(),
                frames: vec![Frame {
                    name: "main".into(),
                    line: Some(42),
                    vars: vec![Var {
                        name: "g".into(),
                        value: "1".into(),
                        children: vec![],
                    }],
                }],
                description: None,
            },
            Event::Paused {
                reason: "exception".into(),
                frames: vec![
                    Frame {
                        name: "foo".into(),
                        line: Some(7),
                        vars: vec![],
                    },
                    Frame {
                        name: "main".into(),
                        line: Some(20),
                        vars: vec![],
                    },
                ],
                description: Some("divisão por zero".into()),
            },
            Event::Output { text: "x=5".into() },
            Event::Exited,
        ] {
            let line = to_line(&ev).unwrap();
            let back: Event = from_line(&line).unwrap();
            assert_eq!(ev, back);
        }
    }

    #[test]
    fn command_tag_is_stable() {
        // O adaptador depende destes nomes — fixa o formato do protocolo.
        let line = to_line(&Command::Step { mode: Step::In }).unwrap();
        assert!(line.contains("\"cmd\":\"step\""));
        assert!(line.contains("\"mode\":\"in\""));
    }
}
