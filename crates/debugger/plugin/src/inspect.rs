//! Inspeção de variáveis em escopo — a lógica (quais símbolos, como formatar)
//! separada da leitura de memória real (`get_ref`), via o trait [`CellReader`].
//! Assim a coleta é testável com um leitor falso, sem servidor.

use samp::debug::AmxDbg;

use pawnpro_dbg_protocol::Var;

/// Lê uma célula (32 bits) da memória da AMX. A implementação real usa
/// `Amx::get_ref`; nos testes, um mapa em memória.
pub trait CellReader {
    /// Lê a célula no endereço de **data** dado. `None` se inválido.
    fn read_cell(&self, data_addr: i32) -> Option<i32>;
}

/// Coleta as variáveis visíveis no endereço de código `cip`, dado o frame `frm`.
/// Globais usam endereço absoluto; locais/args são relativos ao frame.
#[must_use]
pub fn collect(dbg: &AmxDbg, reader: &impl CellReader, cip: u32, frm: i32) -> Vec<Var> {
    let mut out = Vec::new();
    for sym in dbg.symbols_in_scope(cip) {
        // Endereço efetivo no data segment (global ou relativo ao frame), via SDK.
        let addr = sym.effective_address(frm);
        out.push(if sym.is_array() {
            build_array(sym, addr, reader, dbg)
        } else {
            let value = reader.read_cell(addr).map_or_else(
                || "?".to_string(),
                |c| format_scalar(c, dbg.tag_name(sym.tag)),
            );
            Var {
                name: sym.name.clone(),
                value,
                children: vec![],
            }
        });
    }
    out
}

/// Máximo de elementos de array expostos (evita despejar arrays enormes na
/// inspeção). Os primeiros `MAX_ELEMS`; o resto fica indicado por `…` no resumo.
const MAX_ELEMS: u32 = 256;

/// Formata um valor escalar conforme o tag do símbolo. Em Pawn todo valor é um
/// cell de 32 bits; o tag diz como interpretá-lo:
/// - `Float`: os bits são um `f32` IEEE-754 (senão `96.5` apareceria como o
///   inteiro `1119944704`).
/// - `bool`: `0`/`1` viram `false`/`true`.
/// - demais: inteiro com sinal.
fn format_scalar(cell: i32, tag: Option<&str>) -> String {
    match tag {
        Some("Float") => {
            let f = f32::from_bits(cell.cast_unsigned());
            // Notação enxuta: sem zeros à toa, mas mantendo a parte fracionária.
            format!("{f}")
        }
        Some("bool") => if cell == 0 { "false" } else { "true" }.to_string(),
        _ => cell.to_string(),
    }
}

/// Monta a [`Var`] de um array: lê os elementos (até [`MAX_ELEMS`]) como filhos
/// expansíveis e resume o valor. Se as células formam uma string terminada em
/// zero (texto), o resumo vira `"texto"`; senão, `[a, b, c, …]`.
fn build_array(
    sym: &samp::debug::DbgSymbol,
    base: i32,
    reader: &impl CellReader,
    dbg: &AmxDbg,
) -> Var {
    let tag = dbg.tag_name(sym.tag);
    let len = sym.dims.first().map_or(0, |d| d.size);
    let show = len.min(MAX_ELEMS);

    let mut cells = Vec::with_capacity(show as usize);
    let mut children = Vec::with_capacity(show as usize);
    for i in 0..show {
        let addr = base.wrapping_add(i32::try_from(i).unwrap_or(0) * 4);
        let cell = reader.read_cell(addr);
        cells.push(cell);
        children.push(Var {
            name: format!("[{i}]"),
            value: cell.map_or_else(|| "?".to_string(), |c| format_scalar(c, tag)),
            children: vec![],
        });
    }

    // Resumo: string (se parecer texto terminado em zero) ou prévia numérica.
    let value = as_string(&cells).map_or_else(
        || {
            const PREVIEW: usize = 8;
            let parts: Vec<String> = cells
                .iter()
                .take(PREVIEW)
                .map(|c| c.map_or_else(|| "?".to_string(), |v| v.to_string()))
                .collect();
            let ellipsis = if len as usize > parts.len() {
                ", …"
            } else {
                ""
            };
            format!("[{}{}]", parts.join(", "), ellipsis)
        },
        |s| format!("\"{s}\""),
    );

    Var {
        name: sym.name.clone(),
        value,
        children,
    }
}

/// Interpreta as células como uma string do Pawn: caracteres imprimíveis até um
/// terminador `0`. `None` se qualquer célula for ilegível/não-imprimível ou não
/// houver terminador — conservador, para não mostrar array de inteiros como texto.
/// Decodifica em Latin-1 (aproxima o Windows-1252 do SA-MP nos acentos).
fn as_string(cells: &[Option<i32>]) -> Option<String> {
    let mut s = String::new();
    for cell in cells {
        let c = (*cell)?;
        if c == 0 {
            return (!s.is_empty()).then_some(s); // terminador → fim da string
        }
        let b = u8::try_from(c).ok()?;
        let printable = (0x20..=0x7e).contains(&b) || (0xa0..=0xff).contains(&b);
        if !printable {
            return None;
        }
        s.push(char::from(b));
    }
    None // sem terminador na faixa lida → não trata como string
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn as_string_detects_terminated_text() {
        // "Oi" + terminador → string.
        let cells = vec![Some(79), Some(105), Some(0), Some(120)];
        assert_eq!(as_string(&cells), Some("Oi".to_string()));
        // Latin-1 (acento): 'á' = 0xE1.
        assert_eq!(as_string(&[Some(0xE1), Some(0)]), Some("á".to_string()));
    }

    #[test]
    fn as_string_conservative() {
        // Sem terminador na faixa → não é string.
        assert_eq!(as_string(&[Some(72), Some(105)]), None);
        // Caractere não-imprimível (7 = BEL) → não é string.
        assert_eq!(as_string(&[Some(72), Some(7), Some(0)]), None);
        // Célula ilegível → não é string.
        assert_eq!(as_string(&[Some(72), None, Some(0)]), None);
        // Só o terminador (vazio) → não é string.
        assert_eq!(as_string(&[Some(0)]), None);
    }

    #[test]
    fn format_scalar_by_tag() {
        // Float: os bits de 96.5 (1119944704) viram "96.5", não o inteiro cru.
        let bits_965 = 96.5f32.to_bits().cast_signed();
        assert_eq!(format_scalar(bits_965, Some("Float")), "96.5");
        assert_eq!(format_scalar(0, Some("Float")), "0"); // 0.0 → "0"
        let neg = (-3.25f32).to_bits().cast_signed();
        assert_eq!(format_scalar(neg, Some("Float")), "-3.25");
        // bool.
        assert_eq!(format_scalar(0, Some("bool")), "false");
        assert_eq!(format_scalar(1, Some("bool")), "true");
        assert_eq!(format_scalar(7, Some("bool")), "true"); // !=0 → true
        // Sem tag / tag desconhecido → inteiro com sinal.
        assert_eq!(format_scalar(255, None), "255");
        assert_eq!(format_scalar(-250, Some("Qualquer")), "-250");
    }

    /// Leitor falso: memória de dados como mapa endereço→célula.
    struct FakeMem(HashMap<i32, i32>);
    impl CellReader for FakeMem {
        fn read_cell(&self, addr: i32) -> Option<i32> {
            self.0.get(&addr).copied()
        }
    }

    /// Monta um `AmxDbg` com um global e um local (reusa o encoder do parser).
    fn dbg_with_symbols() -> AmxDbg {
        let mut t = Vec::new();
        // files: 1
        push_u32(&mut t, 0);
        push_cstr(&mut t, "a.pwn");
        // lines: 1
        push_u32(&mut t, 0);
        push_i32(&mut t, 1);
        // symbols: 2 — global "g" @200; local "x" rel -4, escopo [0,40)
        push_symbol(&mut t, 200, 0, 1, "g"); // global var (escopo 0..1)
        push_symbol_local(&mut t, (-4i32).cast_unsigned(), 8, 40, "x");
        // header
        let mut b = Vec::new();
        push_i32(&mut b, i32::try_from(22 + t.len()).unwrap());
        b.extend_from_slice(&samp::debug::AMX_DBG_MAGIC.to_le_bytes());
        b.push(1);
        b.push(1);
        push_i16(&mut b, 0); // flags
        push_i16(&mut b, 1); // files
        push_i16(&mut b, 1); // lines
        push_i16(&mut b, 2); // symbols
        push_i16(&mut b, 0); // tags
        push_i16(&mut b, 0); // automatons
        push_i16(&mut b, 0); // states
        b.extend_from_slice(&t);
        AmxDbg::parse(&b).unwrap()
    }

    fn push_i16(v: &mut Vec<u8>, x: i16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn push_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn push_i32(v: &mut Vec<u8>, x: i32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn push_cstr(v: &mut Vec<u8>, s: &str) {
        v.extend_from_slice(s.as_bytes());
        v.push(0);
    }

    fn push_symbol(v: &mut Vec<u8>, addr: u32, cs: u32, ce: u32, name: &str) {
        push_u32(v, addr);
        push_i16(v, 0); // tag
        push_u32(v, cs);
        push_u32(v, ce);
        v.push(1); // ident = variable
        v.push(0); // vclass = global
        push_i16(v, 0); // dim
        push_cstr(v, name);
    }
    fn push_symbol_local(v: &mut Vec<u8>, addr: u32, cs: u32, ce: u32, name: &str) {
        push_u32(v, addr);
        push_i16(v, 0);
        push_u32(v, cs);
        push_u32(v, ce);
        v.push(1); // variable
        v.push(1); // vclass = local
        push_i16(v, 0);
        push_cstr(v, name);
    }

    #[test]
    fn reads_global_and_local() {
        let dbg = dbg_with_symbols();
        let mut mem = HashMap::new();
        mem.insert(200, 99); // global g = 99
        mem.insert(100 - 4, 7); // local x: frm(100) + (-4) = 96 → 7
        let reader = FakeMem(mem);

        let vars = collect(&dbg, &reader, 10, 100);
        let g = vars.iter().find(|v| v.name == "g").unwrap();
        let x = vars.iter().find(|v| v.name == "x").unwrap();
        assert_eq!(g.value, "99");
        assert_eq!(x.value, "7");
    }

    #[test]
    fn local_out_of_scope_is_excluded() {
        let dbg = dbg_with_symbols();
        let reader = FakeMem(HashMap::new());
        // cip antes do escopo do local x [8,40): só o global aparece.
        let vars = collect(&dbg, &reader, 4, 100);
        assert!(vars.iter().any(|v| v.name == "g"));
        assert!(!vars.iter().any(|v| v.name == "x"));
    }
}
