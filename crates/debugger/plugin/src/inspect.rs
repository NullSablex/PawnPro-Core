//! Inspeção de variáveis em escopo — a lógica (quais símbolos, como formatar)
//! separada da leitura de memória real (`get_ref`), via o trait [`CellReader`].
//! Assim a coleta é testável com um leitor falso, sem servidor.

use samp::debug::{AmxDbg, DbgSymDim, DbgSymbol, Ident};

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
    dbg.symbols_in_scope(cip)
        .into_iter()
        .map(|sym| {
            let tag = dbg.tag_name(sym.tag);
            match symbol_base(sym, frm, reader) {
                Some(base) if sym.is_array() => build_array(sym, base, reader, tag),
                base => Var {
                    name: sym.name.clone(),
                    value: base
                        .and_then(|addr| reader.read_cell(addr))
                        .map_or_else(|| "?".to_string(), |c| format_scalar(c, tag)),
                    children: vec![],
                },
            }
        })
        .collect()
}

/// Tamanho de uma célula, em bytes.
const CELL: i32 = 4;

/// Onde começam os dados do símbolo: a célula de um escalar, ou o primeiro
/// elemento de um array.
///
/// Parâmetros por referência (`&x`, `arr[]`) guardam na própria célula o
/// endereço do dado, como o `pawndbg` lê (`get_symbolvalue`).
fn symbol_base(sym: &DbgSymbol, frm: i32, reader: &impl CellReader) -> Option<i32> {
    let addr = sym.effective_address(frm);
    match sym.ident {
        Ident::Reference | Ident::RefArray => reader.read_cell(addr),
        _ => Some(addr),
    }
}

/// O que um caminho de índices alcança.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Located {
    pub addr: i32,
    /// `true` quando o caminho chega a uma célula (escalar ou elemento da
    /// última dimensão); `false` num array ou sub-array inteiro.
    pub is_cell: bool,
}

/// Endereço do elemento `path` do símbolo — vazio para o próprio símbolo.
///
/// Arrays de várias dimensões começam por vetores de indireção: a célula de
/// cada índice guarda o deslocamento, relativo a ela mesma, até o sub-array
/// (`adjust_indirectiontables` em `sc1.c`). Cada nível soma o índice e segue
/// esse deslocamento, como o `pawndbg`. O limite só é conferido quando a
/// dimensão tem tamanho conhecido: `arr[]` recebido por parâmetro não tem.
#[must_use]
pub fn locate(
    sym: &DbgSymbol,
    frm: i32,
    path: &[usize],
    reader: &impl CellReader,
) -> Option<Located> {
    let base = symbol_base(sym, frm, reader)?;
    if !sym.is_array() {
        return path.is_empty().then_some(Located {
            addr: base,
            is_cell: true,
        });
    }
    if path.len() > sym.dims.len() {
        return None;
    }
    let mut addr = base;
    for (level, &index) in path.iter().enumerate() {
        let size = sym.dims[level].size;
        if size > 0 && u32::try_from(index).ok()? >= size {
            return None;
        }
        addr = addr.checked_add(i32::try_from(index).ok()?.checked_mul(CELL)?)?;
        if level + 1 < sym.dims.len() {
            addr = sub_array(addr, reader)?;
        }
    }
    Some(Located {
        addr,
        is_cell: path.len() == sym.dims.len(),
    })
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

/// Onde começa o sub-array de uma célula de indireção: a célula guarda o
/// deslocamento relativo a ela mesma (`adjust_indirectiontables` em `sc1.c`).
fn sub_array(cell_addr: i32, reader: &impl CellReader) -> Option<i32> {
    cell_addr.checked_add(reader.read_cell(cell_addr)?)
}

/// Monta a [`Var`] de um array, uma dimensão por nível: os elementos (até
/// [`MAX_ELEMS`]) viram filhos expansíveis, e as linhas de um array de várias
/// dimensões têm os próprios filhos. Células que formam uma string terminada em
/// zero resumem como `"texto"`; senão, `[a, b, c, …]`.
fn build_array(sym: &DbgSymbol, base: i32, reader: &impl CellReader, tag: Option<&str>) -> Var {
    let (value, children) = build_level(&sym.dims, 0, base, reader, tag);
    Var {
        name: sym.name.clone(),
        value,
        children,
    }
}

/// Resumo e filhos da dimensão `level`, a partir do endereço `base`.
fn build_level(
    dims: &[DbgSymDim],
    level: usize,
    base: i32,
    reader: &impl CellReader,
    tag: Option<&str>,
) -> (String, Vec<Var>) {
    let size = dims.get(level).map_or(0, |d| d.size);
    let last = level + 1 >= dims.len();
    // Tamanho desconhecido (`arr[]` por parâmetro): um elemento, como o
    // `pawndbg` — ler além disso seria ler o que não é do array.
    let known = size > 0;
    let show = if known { size.min(MAX_ELEMS) } else { 1 };
    let more = !known || size > show;

    let mut children = Vec::with_capacity(show as usize);
    let mut cells = Vec::new();
    for i in 0..show {
        let slot = i32::try_from(i)
            .ok()
            .and_then(|i| i.checked_mul(CELL))
            .and_then(|off| base.checked_add(off));
        let name = format!("[{i}]");
        if last {
            let cell = slot.and_then(|addr| reader.read_cell(addr));
            cells.push(cell);
            children.push(Var {
                name,
                value: cell.map_or_else(|| "?".to_string(), |c| format_scalar(c, tag)),
                children: vec![],
            });
        } else {
            let sub = slot.and_then(|addr| sub_array(addr, reader));
            let (value, grandchildren) = sub.map_or_else(
                || ("?".to_string(), vec![]),
                |sub| build_level(dims, level + 1, sub, reader, tag),
            );
            children.push(Var {
                name,
                value,
                children: grandchildren,
            });
        }
    }

    let ellipsis = if more { ", …" } else { "" };
    let value = if last {
        // Uma string de tamanho desconhecido precisa ser lida até o terminador.
        let text = if known {
            as_string(&cells)
        } else {
            as_string(&read_until_zero(base, reader))
        };
        text.map_or_else(
            || {
                const PREVIEW: usize = 8;
                let parts: Vec<String> = cells
                    .iter()
                    .take(PREVIEW)
                    .map(|c| c.map_or_else(|| "?".to_string(), |v| v.to_string()))
                    .collect();
                let more = more || cells.len() > parts.len();
                format!("[{}{}]", parts.join(", "), if more { ", …" } else { "" })
            },
            |s| format!("\"{s}\""),
        )
    } else {
        format!(
            "[{}{ellipsis}]",
            children
                .iter()
                .map(|c| c.value.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    (value, children)
}

/// Células a partir de `base` até o primeiro zero, com teto em [`MAX_ELEMS`].
fn read_until_zero(base: i32, reader: &impl CellReader) -> Vec<Option<i32>> {
    let mut cells = Vec::new();
    for i in 0..MAX_ELEMS {
        let cell = i32::try_from(i)
            .ok()
            .and_then(|i| i.checked_mul(CELL))
            .and_then(|off| base.checked_add(off))
            .and_then(|addr| reader.read_cell(addr));
        cells.push(cell);
        if cell.is_none_or(|c| c == 0) {
            break;
        }
    }
    cells
}

/// Interpreta as células como uma string do Pawn: caracteres imprimíveis até um
/// terminador `0`. `None` se qualquer célula for ilegível/não-imprimível ou não
/// houver terminador — conservador, para não mostrar array de inteiros como texto.
///
/// Um caractere só não basta: `[100, 0, 0]` é muito mais provável num array de
/// números do que a string `"d"`, e o resumo mentiria sobre os dados.
/// Decodifica em Latin-1 (aproxima o Windows-1252 do SA-MP nos acentos).
fn as_string(cells: &[Option<i32>]) -> Option<String> {
    let mut s = String::new();
    for cell in cells {
        let c = (*cell)?;
        if c == 0 {
            return (s.chars().count() >= 2).then_some(s); // terminador → fim da string
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
        assert_eq!(
            as_string(&[Some(0xE1), Some(0xE9), Some(0)]),
            Some("áé".to_string())
        );
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
        // Um número pequeno seguido de zeros é número, não a string "d".
        assert_eq!(as_string(&[Some(100), Some(0), Some(0)]), None);
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

    /// Um símbolo com `ident`, `vclass` e dimensões escolhidos, visível em
    /// `[0, 100)`.
    fn symbol(addr: i32, ident: u8, vclass: u8, dims: &[u32], name: &str) -> AmxDbg {
        let mut t = Vec::new();
        push_u32(&mut t, 0);
        push_cstr(&mut t, "a.pwn");
        push_u32(&mut t, 0);
        push_i32(&mut t, 1);
        push_u32(&mut t, addr.cast_unsigned());
        push_i16(&mut t, 0);
        push_u32(&mut t, 0);
        push_u32(&mut t, 100);
        t.push(ident);
        t.push(vclass);
        push_i16(&mut t, i16::try_from(dims.len()).unwrap());
        // As dimensões vêm depois do nome (`AMX_DBG_SYMDIM`, `amxdbg.h`).
        push_cstr(&mut t, name);
        for size in dims {
            push_i16(&mut t, 0);
            push_u32(&mut t, *size);
        }
        let mut b = Vec::new();
        push_i32(&mut b, i32::try_from(22 + t.len()).unwrap());
        b.extend_from_slice(&samp::debug::AMX_DBG_MAGIC.to_le_bytes());
        b.push(1);
        b.push(1);
        push_i16(&mut b, 0);
        push_i16(&mut b, 1);
        push_i16(&mut b, 1);
        push_i16(&mut b, 1);
        push_i16(&mut b, 0);
        push_i16(&mut b, 0);
        push_i16(&mut b, 0);
        b.extend_from_slice(&t);
        AmxDbg::parse(&b).unwrap()
    }

    const VARIABLE: u8 = 1;
    const REFERENCE: u8 = 2;
    const ARRAY: u8 = 3;
    const REFARRAY: u8 = 4;
    const GLOBAL: u8 = 0;
    const LOCAL: u8 = 1;

    /// `new g_Conta[4][3]` em 1000, como o compilador o dispõe: quatro
    /// células de indireção com o deslocamento, relativo a cada uma, até a
    /// linha; depois as linhas, de três células cada. Valores = linha * 10 +
    /// coluna.
    fn two_dimensional() -> (AmxDbg, FakeMem) {
        let dbg = symbol(1000, ARRAY, GLOBAL, &[4, 3], "g_Conta");
        let mut mem = HashMap::new();
        for row in 0..4 {
            let cell = 1000 + row * 4;
            let data = 1016 + row * 12;
            mem.insert(cell, data - cell);
            for col in 0..3 {
                mem.insert(data + col * 4, row * 10 + col);
            }
        }
        (dbg, FakeMem(mem))
    }

    #[test]
    fn indirection_offsets_match_the_compiler_layout() {
        // Os valores que o servidor real mostrou para `g_Conta[4][E_CONTA]`.
        let (_, mem) = two_dimensional();
        let offsets: Vec<i32> = (0..4).map(|row| mem.0[&(1000 + row * 4)]).collect();
        assert_eq!(offsets, [16, 24, 32, 40]);
    }

    #[test]
    fn locates_cells_of_a_two_dimensional_array() {
        let (dbg, mem) = two_dimensional();
        let sym = &dbg.symbols[0];
        let cell = locate(sym, 0, &[2, 1], &mem).unwrap();
        assert!(cell.is_cell);
        assert_eq!(mem.read_cell(cell.addr), Some(21));
        // A linha tem endereço, mas não é uma célula.
        let row = locate(sym, 0, &[2], &mem).unwrap();
        assert!(!row.is_cell);
        assert_eq!(row.addr, 1016 + 2 * 12);
        // Fora do limite, em qualquer dimensão.
        assert_eq!(locate(sym, 0, &[4, 0], &mem), None);
        assert_eq!(locate(sym, 0, &[0, 3], &mem), None);
        assert_eq!(locate(sym, 0, &[0, 0, 0], &mem), None);
    }

    /// O painel mostrava as células de indireção como se fossem os dados.
    #[test]
    fn two_dimensional_array_shows_rows_with_their_cells() {
        let (dbg, mem) = two_dimensional();
        let vars = collect(&dbg, &mem, 10, 0);
        let conta = &vars[0];
        assert_eq!(conta.children.len(), 4);
        assert_eq!(conta.children[2].children[1].value, "21");
        assert_eq!(conta.children[3].value, "[30, 31, 32]");
        assert!(!conta.value.contains("16"), "{}", conta.value);
    }

    /// `arr[]` recebido por parâmetro: a célula no frame guarda o endereço do
    /// array, e o tamanho não é conhecido.
    #[test]
    fn array_parameter_is_read_through_its_reference() {
        let dbg = symbol(12, REFARRAY, LOCAL, &[0], "arr");
        let sym = &dbg.symbols[0];
        // frm = 500: o parâmetro em 512 aponta para o array em 2000.
        let mem = FakeMem(HashMap::from([
            (512, 2000),
            (2000, 7),
            (2004, 8),
            (2008, 9),
        ]));
        let cell = locate(sym, 500, &[2], &mem).unwrap();
        assert_eq!(
            mem.read_cell(cell.addr),
            Some(9),
            "sem limite conhecido, vale o índice"
        );

        let vars = collect(&dbg, &mem, 10, 500);
        assert_eq!(vars[0].children[0].value, "7");
    }

    #[test]
    fn unknown_size_string_parameter_reads_up_to_the_terminator() {
        let dbg = symbol(12, REFARRAY, LOCAL, &[0], "cmdtext");
        let mut mem = HashMap::from([(512, 3000)]);
        for (i, b) in b"/teste\0".iter().enumerate() {
            mem.insert(3000 + i32::try_from(i).unwrap() * 4, i32::from(*b));
        }
        let vars = collect(&dbg, &FakeMem(mem), 10, 500);
        assert_eq!(vars[0].value, "\"/teste\"");
    }

    #[test]
    fn reference_parameter_is_dereferenced() {
        let dbg = symbol(16, REFERENCE, LOCAL, &[], "x");
        let mem = FakeMem(HashMap::from([(516, 4000), (4000, 42)]));
        assert_eq!(collect(&dbg, &mem, 10, 500)[0].value, "42");
        assert_eq!(
            locate(&dbg.symbols[0], 500, &[], &mem).map(|l| l.addr),
            Some(4000)
        );
        // Escalar não aceita índice.
        assert_eq!(locate(&dbg.symbols[0], 500, &[0], &mem), None);
    }

    #[test]
    fn plain_scalar_and_array_are_unchanged() {
        let dbg = symbol(800, VARIABLE, GLOBAL, &[], "g");
        let mem = FakeMem(HashMap::from([(800, 5)]));
        assert_eq!(
            locate(&dbg.symbols[0], 0, &[], &mem).map(|l| l.addr),
            Some(800)
        );

        let dbg = symbol(900, ARRAY, GLOBAL, &[5], "a");
        let mem = FakeMem(HashMap::new());
        let cell = locate(&dbg.symbols[0], 0, &[3], &mem).unwrap();
        assert_eq!((cell.addr, cell.is_cell), (912, true));
        assert_eq!(locate(&dbg.symbols[0], 0, &[5], &mem), None);
    }
}
