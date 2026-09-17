//! Avaliador de expressões simples para o `evaluate` (watch/hover). Suporta um
//! operando ou `A OP B` (um operador), com operandos **literais** (`10`, `0x0a`,
//! `1.5`, `true`), **variáveis** em escopo, ou **elementos de array** `arr[i]`
//! (o índice pode ser literal, variável ou uma subexpressão). Operadores:
//! `+ - * / %` (aritmética; inteiros seguem a semântica do Pawn) e
//! `== != < > <= >=` (comparação, resultado `true`/`false`).
//!
//! Sem cadeias com precedência (um operador de topo) — previsível e conservador:
//! o que não casar devolve `None` e o editor mostra "não disponível".

use pawnpro_dbg_protocol::Var;

#[derive(Clone, Copy)]
enum Val {
    Int(i32),
    Float(f32),
    Bool(bool),
}

/// Avalia `expr` contra as `vars` do frame; devolve o texto do resultado ou
/// `None` se não for avaliável.
#[must_use]
pub fn eval(expr: &str, vars: &[Var]) -> Option<String> {
    Some(format_val(eval_expr(expr.trim(), vars)?))
}

/// Operadores, 2-char antes de 1-char (para `<=`/`>=`/`==`/`!=`).
const OPS: [&str; 11] = ["==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%"];

fn eval_expr(expr: &str, vars: &[Var]) -> Option<Val> {
    if let Some((op, l, r)) = split_binary(expr) {
        return apply(
            op,
            eval_operand(l.trim(), vars)?,
            eval_operand(r.trim(), vars)?,
        );
    }
    eval_operand(expr.trim(), vars)
}

/// Primeiro operador binário em profundidade 0 (fora de `[]`) com lado esquerdo
/// não-vazio — assim `-5` fica como literal e `arr[i]` não é fatiado por dentro.
fn split_binary(expr: &str) -> Option<(&'static str, &str, &str)> {
    let bytes = expr.as_bytes();
    let mut depth: i32 = 0;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => depth -= 1,
            _ if depth == 0 => {
                for op in OPS {
                    if expr[i..].starts_with(op) && !expr[..i].trim().is_empty() {
                        return Some((op, &expr[..i], &expr[i + op.len()..]));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn eval_operand(s: &str, vars: &[Var]) -> Option<Val> {
    let s = s.trim();
    if let Some(v) = parse_value(s) {
        return Some(v);
    }
    // arr[i] ou arr[i][j]: cada índice pode ser uma subexpressão.
    if let Some((name, indices)) = split_indexed(s) {
        let path = eval_path(&indices, vars)?;
        let leaf = descend(vars.iter().find(|v| v.name == name)?, &path)?;
        return leaf.children.is_empty().then(|| parse_value(&leaf.value))?;
    }
    // variável simples (o valor em cache já vem formatado)
    parse_value(&vars.iter().find(|v| v.name == s)?.value)
}

/// Separa `nome[a][b]` em `nome` e as expressões de cada índice. `None` se não
/// for um nome seguido só de índices entre colchetes balanceados.
#[must_use]
pub fn split_indexed(s: &str) -> Option<(&str, Vec<&str>)> {
    let s = s.trim();
    let open = s.find('[')?;
    let name = s[..open].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let mut indices = Vec::new();
    let mut rest = &s[open..];
    while !rest.is_empty() {
        rest = rest.strip_prefix('[')?;
        let mut depth = 1;
        let close = rest.char_indices().find_map(|(i, c)| {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(i)
        })?;
        indices.push(&rest[..close]);
        rest = rest[close + 1..].trim_start();
    }
    Some((name, indices))
}

/// Avalia cada índice e devolve o caminho. `None` se algum não for um inteiro
/// não negativo.
#[must_use]
pub fn eval_path(indices: &[&str], vars: &[Var]) -> Option<Vec<usize>> {
    indices
        .iter()
        .map(|expr| usize::try_from(as_i32(eval_expr(expr, vars)?)?).ok())
        .collect()
}

/// O elemento no caminho dado, descendo pelos filhos (`[i]`, `[j]`, …).
#[must_use]
pub fn descend<'a>(var: &'a Var, path: &[usize]) -> Option<&'a Var> {
    path.iter().try_fold(var, |node, i| node.children.get(*i))
}

/// Interpreta um literal/valor formatado: `true`/`false`, hex, inteiro, float.
fn parse_value(s: &str) -> Option<Val> {
    let s = s.trim();
    match s {
        "true" => return Some(Val::Bool(true)),
        "false" => return Some(Val::Bool(false)),
        _ => {}
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i32::from_str_radix(hex, 16).ok().map(Val::Int);
    }
    if let Ok(i) = s.parse::<i32>() {
        return Some(Val::Int(i));
    }
    s.parse::<f32>().ok().map(Val::Float)
}

fn apply(op: &str, a: Val, b: Val) -> Option<Val> {
    match op {
        "==" | "!=" | "<" | ">" | "<=" | ">=" => cmp(op, a, b),
        "+" | "-" | "*" | "/" | "%" => arith(op, a, b),
        _ => None,
    }
}

fn cmp(op: &str, lhs: Val, rhs: Val) -> Option<Val> {
    if let (Val::Bool(a), Val::Bool(b)) = (lhs, rhs) {
        return match op {
            "==" => Some(Val::Bool(a == b)),
            "!=" => Some(Val::Bool(a != b)),
            _ => None, // ordem em bool não faz sentido
        };
    }
    let (a, b) = (as_f64(lhs)?, as_f64(rhs)?);
    let result = match op {
        "==" => (a - b).abs() < f64::EPSILON,
        "!=" => (a - b).abs() >= f64::EPSILON,
        "<" => a < b,
        ">" => a > b,
        "<=" => a <= b,
        ">=" => a >= b,
        _ => return None,
    };
    Some(Val::Bool(result))
}

fn arith(op: &str, lhs: Val, rhs: Val) -> Option<Val> {
    // Ambos inteiros → aritmética inteira (semântica do Pawn: `/` e `%` truncam).
    if let (Val::Int(a), Val::Int(b)) = (lhs, rhs) {
        let result = match op {
            "+" => a.wrapping_add(b),
            "-" => a.wrapping_sub(b),
            "*" => a.wrapping_mul(b),
            "/" => a.checked_div(b)?,
            "%" => a.checked_rem(b)?,
            _ => return None,
        };
        return Some(Val::Int(result));
    }
    let (a, b) = (as_f64(lhs)?, as_f64(rhs)?);
    let result = match op {
        "+" => a + b,
        "-" => a - b,
        "*" => a * b,
        "/" if b != 0.0 => a / b,
        "%" if b != 0.0 => a % b,
        _ => return None,
    };
    #[expect(clippy::cast_possible_truncation)] // volta a f32 (célula Pawn)
    Some(Val::Float(result as f32))
}

fn as_f64(v: Val) -> Option<f64> {
    match v {
        Val::Int(i) => Some(f64::from(i)),
        Val::Float(f) => Some(f64::from(f)),
        Val::Bool(_) => None,
    }
}

const fn as_i32(v: Val) -> Option<i32> {
    match v {
        Val::Int(i) => Some(i),
        _ => None,
    }
}

fn format_val(v: Val) -> String {
    match v {
        Val::Int(i) => i.to_string(),
        Val::Float(f) => format!("{f}"),
        Val::Bool(b) => b.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(name: &str, value: &str) -> Var {
        Var {
            name: name.into(),
            value: value.into(),
            children: vec![],
        }
    }
    fn array(name: &str, elems: &[&str]) -> Var {
        Var {
            name: name.into(),
            value: "[...]".into(),
            children: elems.iter().map(|e| scalar("", e)).collect(),
        }
    }

    fn vars() -> Vec<Var> {
        vec![
            scalar("x", "5"),
            scalar("y", "10"),
            scalar("taxa", "96.5"),
            scalar("ativo", "true"),
            array("arr", &["7", "8", "9"]),
            scalar("i", "1"),
        ]
    }

    #[test]
    fn multidimensional_element() {
        let v = vec![
            Var {
                name: "conta".into(),
                value: "[…]".into(),
                children: vec![array("[0]", &["1", "2"]), array("[1]", &["30", "40"])],
            },
            scalar("i", "1"),
        ];
        assert_eq!(eval("conta[1][0]", &v), Some("30".into()));
        assert_eq!(eval("conta[i][i]", &v), Some("40".into()));
        assert_eq!(eval("conta[1][0] + 1", &v), Some("31".into()));
        // Uma linha inteira não é um valor.
        assert_eq!(eval("conta[1]", &v), None);
        assert_eq!(eval("conta[2][0]", &v), None);
    }

    #[test]
    fn split_indexed_reads_each_bracket() {
        assert_eq!(split_indexed("a[1]"), Some(("a", vec!["1"])));
        assert_eq!(
            split_indexed("a[i + 1][b[2]]"),
            Some(("a", vec!["i + 1", "b[2]"]))
        );
        assert_eq!(split_indexed("a[1] + 2"), None);
        assert_eq!(split_indexed("a[1"), None);
        assert_eq!(split_indexed("[1]"), None);
    }

    #[test]
    fn bare_variable_and_literal() {
        let v = vars();
        assert_eq!(eval("x", &v), Some("5".into()));
        assert_eq!(eval("42", &v), Some("42".into()));
        assert_eq!(eval("0x0a", &v), Some("10".into()));
        assert_eq!(eval("true", &v), Some("true".into()));
    }

    #[test]
    fn arithmetic_integer_semantics() {
        let v = vars();
        assert_eq!(eval("x + 1", &v), Some("6".into()));
        assert_eq!(eval("y - x", &v), Some("5".into()));
        assert_eq!(eval("x * 3", &v), Some("15".into()));
        assert_eq!(eval("7 / 2", &v), Some("3".into())); // trunca (Pawn)
        assert_eq!(eval("7 % 2", &v), Some("1".into()));
        assert_eq!(eval("x / 0", &v), None); // div por zero → indisponível
    }

    #[test]
    fn arithmetic_float() {
        let v = vars();
        assert_eq!(eval("taxa + 0.5", &v), Some("97".into()));
        assert_eq!(eval("taxa - 96", &v), Some("0.5".into()));
    }

    #[test]
    fn comparisons() {
        let v = vars();
        assert_eq!(eval("x < y", &v), Some("true".into()));
        assert_eq!(eval("x == 5", &v), Some("true".into()));
        assert_eq!(eval("y <= 9", &v), Some("false".into()));
        assert_eq!(eval("ativo == true", &v), Some("true".into()));
        assert_eq!(eval("ativo < true", &v), None); // ordem em bool → None
    }

    #[test]
    fn array_index() {
        let v = vars();
        assert_eq!(eval("arr[0]", &v), Some("7".into()));
        assert_eq!(eval("arr[i]", &v), Some("8".into())); // i = 1
        assert_eq!(eval("arr[i + 1]", &v), Some("9".into())); // índice é subexpr
        assert_eq!(eval("arr[9]", &v), None); // fora do limite
        assert_eq!(eval("arr[0] + arr[2]", &v), Some("16".into()));
    }

    #[test]
    fn unresolved_is_none() {
        let v = vars();
        assert_eq!(eval("zzz", &v), None); // fora de escopo
        assert_eq!(eval("arr", &v), None); // array cru não é escalar
        assert_eq!(eval("", &v), None);
        assert_eq!(eval("x + + y", &v), None); // malformado
    }
}
