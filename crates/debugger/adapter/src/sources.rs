//! O texto dos fontes como foi compilado, para breakpoints valerem depois de
//! uma edição.
//!
//! O bloco de debug do `.amx` mapeia **linhas** a endereços. Editar o fonte
//! sem recompilar desloca as linhas, e a mesma linha passa a apontar para outro
//! código — a VM pararia no lugar errado, sem aviso.
//!
//! A sessão guarda o texto de cada fonte no momento em que o `.amx` foi
//! carregado (logo depois de a extensão compilar). Quando o editor pede um
//! breakpoint num arquivo que mudou desde então, a linha passa por um diff de
//! linhas entre esse texto e o atual: se a linha existe nos dois, vai para a
//! posição que ela tinha na compilação; se é nova ou foi alterada, não há
//! código compilado para ela, e o breakpoint fica sem verificação.
//!
//! O diff é de texto, não de estrutura: não depende de como o compilador marca
//! as linhas nem de macros que declaram funções.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use samp_sdk::debug::AmxDbg;
use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};

/// Teto do diff. Estourado, o `similar` devolve um diff menos enxuto, mas as
/// linhas que ele dá como iguais continuam iguais — o mapeamento segue correto,
/// só marca mais linhas como alteradas.
const DIFF_DEADLINE: Duration = Duration::from_millis(500);

/// Um fonte conforme estava quando o `.amx` foi carregado.
struct Snapshot {
    /// O nome como aparece no bloco de debug.
    name: String,
    /// Onde o arquivo está em disco, sem `.` nem `..`.
    disk: PathBuf,
    /// `None` quando o arquivo já era mais novo que o `.amx` (o que foi
    /// compilado não é o que estava em disco) ou não pôde ser lido.
    lines: Option<Vec<Vec<u8>>>,
}

/// Os fontes da compilação atual.
#[derive(Default)]
pub struct Sources {
    /// Quando o `.amx` foi gravado. `None` sem `.amx` em disco.
    compiled_at: Option<SystemTime>,
    /// Pasta onde o compilador rodou, de onde partem os nomes relativos.
    base: Option<PathBuf>,
    snapshots: Vec<Snapshot>,
}

/// Como as linhas pedidas pelo editor se relacionam com as compiladas.
pub enum LineView {
    /// O arquivo é o que foi compilado: as linhas valem como estão.
    Unchanged,
    /// O arquivo mudou: as linhas passam pelo mapa.
    Changed(LineMap),
    /// O arquivo mudou e não há como saber o que foi compilado.
    Unknown,
}

impl LineView {
    /// Linha compilada de uma linha atual. `None` quando ela não existe no
    /// código compilado.
    #[must_use]
    pub fn to_compiled(&self, line: i32) -> Option<i32> {
        match self {
            Self::Unchanged => Some(line),
            Self::Changed(map) => map.to_compiled(line),
            Self::Unknown => None,
        }
    }

    /// Linha atual de uma linha compilada. `None` quando ela foi alterada ou
    /// apagada desde a compilação.
    #[must_use]
    pub fn to_current(&self, line: i32) -> Option<i32> {
        match self {
            Self::Unchanged => Some(line),
            Self::Changed(map) => map.to_current(line),
            Self::Unknown => None,
        }
    }
}

/// Correspondência entre as linhas compiladas e as atuais (base 1).
pub struct LineMap {
    /// Índice = linha atual − 1.
    compiled_of_current: Vec<Option<i32>>,
    /// Índice = linha compilada − 1.
    current_of_compiled: Vec<Option<i32>>,
}

impl LineMap {
    fn between(compiled: &[Vec<u8>], current: &[Vec<u8>]) -> Self {
        let mut compiled_of_current = vec![None; current.len()];
        let mut current_of_compiled = vec![None; compiled.len()];
        let deadline = Instant::now() + DIFF_DEADLINE;
        for op in capture_diff_slices_deadline(Algorithm::Myers, compiled, current, Some(deadline))
        {
            if let DiffOp::Equal {
                old_index,
                new_index,
                len,
            } = op
            {
                for k in 0..len {
                    compiled_of_current[new_index + k] = line_number(old_index + k);
                    current_of_compiled[old_index + k] = line_number(new_index + k);
                }
            }
        }
        Self {
            compiled_of_current,
            current_of_compiled,
        }
    }

    fn to_compiled(&self, line: i32) -> Option<i32> {
        lookup(&self.compiled_of_current, line)
    }

    fn to_current(&self, line: i32) -> Option<i32> {
        lookup(&self.current_of_compiled, line)
    }
}

fn line_number(index: usize) -> Option<i32> {
    i32::try_from(index + 1).ok()
}

fn lookup(table: &[Option<i32>], line: i32) -> Option<i32> {
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    table.get(index).copied().flatten()
}

impl Sources {
    /// Guarda o texto dos fontes do bloco de debug.
    ///
    /// Nomes relativos são da pasta onde o compilador rodou, que é a do `.amx`:
    /// a extensão compila na pasta do `.pwn`, e o `.amx` sai ao lado dele.
    #[must_use]
    pub fn capture(amx_path: &Path, dbg: &AmxDbg) -> Self {
        let compiled_at = modified(amx_path);
        let base = amx_path.parent().unwrap_or_else(|| Path::new(""));
        let mut snapshots: Vec<Snapshot> = Vec::new();
        for file in &dbg.files {
            // Um arquivo incluído mais de uma vez aparece repetido na tabela.
            if snapshots.iter().any(|s| s.name == file.name) {
                continue;
            }
            let disk = normalize(&resolve(base, &file.name));
            let lines = match (modified(&disk), compiled_at) {
                (Some(changed), Some(compiled)) if changed <= compiled => read_lines(&disk),
                _ => None,
            };
            snapshots.push(Snapshot {
                name: file.name.clone(),
                disk,
                lines,
            });
        }
        Self {
            compiled_at,
            base: Some(base.to_path_buf()),
            snapshots,
        }
    }

    /// O caminho em disco de um arquivo do bloco de debug, sem `..`: é o que o
    /// editor usa para abrir e comparar. Sem pasta conhecida, o nome como veio.
    #[must_use]
    pub fn disk_path(&self, name: &str) -> String {
        let Some(base) = &self.base else {
            return name.to_string();
        };
        normalize(&resolve(base, name))
            .to_string_lossy()
            .into_owned()
    }

    /// O arquivo do bloco de debug que o editor conhece por `path`.
    ///
    /// Compara caminhos em disco, não texto: o compilador grava os includes
    /// relativos à pasta onde rodou (`../include/x.inc`), e isso nunca é
    /// sufixo do caminho absoluto que o editor usa. Só quando o caminho não
    /// resolve cai na regra de sufixo do `samp_sdk`.
    fn find(&self, path: &str) -> Option<&Snapshot> {
        let wanted = normalize(Path::new(path));
        self.snapshots
            .iter()
            .find(|s| s.disk == wanted)
            .or_else(|| {
                self.snapshots
                    .iter()
                    .find(|s| file_name_matches(&s.name, path))
            })
    }

    /// O nome, na tabela do bloco de debug, do arquivo que o editor conhece
    /// por `path` — é o que o mapa linha↔endereço entende.
    #[must_use]
    pub fn table_name(&self, path: &str) -> Option<&str> {
        self.find(path).map(|s| s.name.as_str())
    }

    /// Como tratar as linhas do arquivo que o editor conhece por `path`.
    #[must_use]
    pub fn view(&self, path: &str) -> LineView {
        // Sem `.amx` não há o que comparar: vale o comportamento de sempre.
        let Some(compiled_at) = self.compiled_at else {
            return LineView::Unchanged;
        };
        if let Some(compiled) = self.find(path).and_then(|s| s.lines.as_ref()) {
            return match read_lines(Path::new(path)) {
                Some(current) if &current == compiled => LineView::Unchanged,
                Some(current) => LineView::Changed(LineMap::between(compiled, &current)),
                None => LineView::Unknown,
            };
        }
        // Sem o texto da compilação, só a data diz se o arquivo pode ter mudado.
        match modified(Path::new(path)) {
            Some(changed) if changed <= compiled_at => LineView::Unchanged,
            _ => LineView::Unknown,
        }
    }
}

fn resolve(base: &Path, name: &str) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Tira `.` e `..` pelo texto do caminho, sem tocar no disco: um link
/// simbólico no meio não pode trocar o arquivo que o editor mostra.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Linhas em bytes: o fonte Pawn costuma estar em Windows-1252, e comparar
/// bytes evita decidir codificação. O `\r` final sai para uma troca de fim de
/// linha não parecer edição.
fn read_lines(path: &Path) -> Option<Vec<Vec<u8>>> {
    let bytes = fs::read(path).ok()?;
    Some(
        bytes
            .split(|&b| b == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec())
            .collect(),
    )
}

/// A mesma regra do `samp_sdk` para casar o nome da tabela com o caminho do
/// editor: iguais, ou um é sufixo do outro numa fronteira de diretório.
pub fn file_name_matches(table: &str, path: &str) -> bool {
    let norm = |s: &str| s.replace('\\', "/");
    let (table, path) = (norm(table), norm(path));
    let suffix_at_boundary = |a: &str, b: &str| {
        b.strip_suffix(a)
            .is_some_and(|head| head.is_empty() || head.ends_with('/'))
    };
    table == path || suffix_at_boundary(&table, &path) || suffix_at_boundary(&path, &table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<Vec<u8>> {
        text.split('\n').map(|l| l.as_bytes().to_vec()).collect()
    }

    #[test]
    fn unchanged_lines_move_with_the_edits_around_them() {
        let compiled = lines("a\nb\nc\nd");
        // Duas linhas novas no topo, `c` alterada.
        let current = lines("x\ny\na\nb\nC\nd");
        let map = LineMap::between(&compiled, &current);
        assert_eq!(map.to_compiled(3), Some(1), "a");
        assert_eq!(map.to_compiled(4), Some(2), "b");
        assert_eq!(map.to_compiled(6), Some(4), "d");
        assert_eq!(map.to_current(4), Some(6));
    }

    /// Linha nova ou alterada não tem código compilado: casá-la com outra
    /// seria parar em código que não é o dela.
    #[test]
    fn new_and_changed_lines_have_no_compiled_code() {
        let compiled = lines("a\nb\nc");
        let current = lines("a\nnova\nB\nc");
        let map = LineMap::between(&compiled, &current);
        assert_eq!(map.to_compiled(2), None);
        assert_eq!(map.to_compiled(3), None);
        assert_eq!(map.to_current(2), None, "a b compilada foi alterada");
    }

    #[test]
    fn debug_block_names_become_editor_paths() {
        let mut sources = Sources::default();
        assert_eq!(sources.disk_path("x.inc"), "x.inc", "sem pasta, como veio");
        sources.base = Some(PathBuf::from("/srv/bplr/gamemodes"));
        assert_eq!(
            sources.disk_path("../include/gl_commands.inc"),
            "/srv/bplr/include/gl_commands.inc"
        );
        assert_eq!(
            sources.disk_path("molde.pwn"),
            "/srv/bplr/gamemodes/molde.pwn"
        );
        assert_eq!(sources.disk_path("/abs/y.inc"), "/abs/y.inc");
    }

    #[test]
    fn out_of_range_lines_are_none() {
        let map = LineMap::between(&lines("a"), &lines("a"));
        assert_eq!(map.to_compiled(0), None);
        assert_eq!(map.to_compiled(2), None);
        assert_eq!(map.to_compiled(-1), None);
    }

    #[test]
    fn names_from_the_debug_block_match_editor_paths() {
        assert!(file_name_matches(
            "/srv/gm/debug-demo.pwn",
            "/srv/gm/debug-demo.pwn"
        ));
        assert!(file_name_matches(
            "molde.pwn",
            "/srv/bplr/gamemodes/molde.pwn"
        ));
        assert!(!file_name_matches("demo.pwn", "/srv/gm/debug-demo.pwn"));
    }

    /// Gerador determinístico, para a simulação não depender de sorte.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(self.0 >> 33).unwrap() % n
        }
    }

    /// Edições aleatórias num fonte cheio de linhas repetidas (`}`, `return 1;`,
    /// linhas vazias), com a resposta certa conhecida pela construção.
    ///
    /// Duas linhas idênticas e vizinhas — uma nova, uma antiga — não se
    /// distinguem pelo texto, e o diff pode dar qualquer uma como a antiga: a
    /// parada é no mesmo comando, ao lado. O que não pode acontecer é uma linha
    /// ir para fora do seu trecho, entre os vizinhos que o diff confirmou: isso
    /// seria parar em outro código.
    #[test]
    fn random_edits_never_map_a_line_to_other_code() {
        let pieces = ["{", "}", "    return 1;", "", "    }", "\treturn 0;"];
        let mut rng = Lcg(7);
        let compiled: Vec<Vec<u8>> = (0..3000)
            .map(|i| {
                if rng.below(3) == 0 {
                    format!("    printf(\"{i}\");").into_bytes()
                } else {
                    pieces[rng.below(pieces.len())].as_bytes().to_vec()
                }
            })
            .collect();

        let (mut kept, mut lost) = (0usize, 0usize);
        for _ in 0..40 {
            let mut current = Vec::new();
            let mut truth: Vec<Option<i32>> = Vec::new();
            for (index, line) in compiled.iter().enumerate() {
                let original = line_number(index);
                match rng.below(60) {
                    0 => {
                        for _ in 0..=rng.below(4) {
                            current.push(pieces[rng.below(pieces.len())].as_bytes().to_vec());
                            truth.push(None);
                        }
                        current.push(line.clone());
                        truth.push(original);
                    }
                    1 => {}
                    2 => {
                        let mut changed = line.clone();
                        changed.extend_from_slice(b" // editada");
                        current.push(changed);
                        truth.push(None);
                    }
                    _ => {
                        current.push(line.clone());
                        truth.push(original);
                    }
                }
            }

            let map = LineMap::between(&compiled, &current);
            let mapped: Vec<Option<i32>> = (0..current.len())
                .map(|index| map.to_compiled(line_number(index).unwrap()))
                .collect();
            // Linhas confirmadas: iguais e na posição da construção.
            let confirmed: Vec<Option<i32>> = mapped
                .iter()
                .zip(&truth)
                .map(|(got, expected)| (got == expected).then_some(*got).flatten())
                .collect();
            for (index, got) in mapped.iter().enumerate() {
                match (truth[index], got) {
                    // Se a gêmea vizinha levou o mapeamento, o código está
                    // coberto; perda é código compilado que nenhuma linha alcança.
                    (Some(expected), None) => {
                        if map.to_current(expected).is_none() {
                            lost += 1;
                        }
                    }
                    (expected, Some(got)) => {
                        if expected == Some(*got) {
                            kept += 1;
                            continue;
                        }
                        let before = confirmed[..index].iter().rev().flatten().next();
                        let after = confirmed[index + 1..].iter().flatten().next();
                        assert!(
                            before.is_none_or(|b| b < got) && after.is_none_or(|a| got < a),
                            "linha {} foi para {got}, fora do trecho entre {before:?} e {after:?}",
                            index + 1
                        );
                        let at = usize::try_from(got - 1).unwrap();
                        assert_eq!(compiled[at], current[index], "texto diferente");
                    }
                    (None, None) => {}
                }
            }
        }
        // Perder uma linha é seguro, mas deixa um breakpoint válido sem
        // verificação à toa; tem de ser raro.
        assert!(
            lost * 1000 <= kept,
            "linhas perdidas demais: {lost} de {}",
            kept + lost
        );
    }
}
