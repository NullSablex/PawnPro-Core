//! Plugin do servidor (Componente 2 do debugger) — `cdylib` carregado pelo
//! SA-MP/open.mp. Instala o debug hook do AMX, decide pausas em breakpoint/step
//! ([`control`]) e atende o adaptador via TCP ([`bridge`]).
//!
//! O ciclo de vida do plugin (Load/Unload/AmxLoad) vem pronto do crate `samp`
//! (`initialize_plugin!` + `SampPlugin`). A depuração da VM também é nativa do
//! SDK: `samp::plugin::enable_debug_hook` instala o hook e `on_debug_break`
//! recebe cada linha; o parser `AMX_DBG` é `samp::debug`, e a leitura/escrita de
//! células usa `Amx::read_cell`/`write_cell`. Este crate só carrega a lógica de
//! breakpoint/step e a ponte com o adaptador.

mod bridge;
mod control;
mod gate;
mod hook;
mod inspect;
mod runtime_error;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use samp::plugin::SampPlugin;
use samp::{initialize_plugin, prelude::Amx};

/// `false` até a primeira VM (gamemode) ser carregada. Só ela espera o adaptador
/// configurar os breakpoints — as demais VMs não bloqueiam a carga.
static FIRST_AMX_SEEN: AtomicBool = AtomicBool::new(false);

/// Marcador embutido no binário: permite identificar com certeza que este
/// arquivo é o plugin oficial do depurador (e não um homônimo qualquer com o
/// mesmo nome). A extensão faz um *grep de bytes* no `.so`/`.dll` procurando
/// esta string literal (não lê a tabela de exportação — seria preciso um parser
/// ELF/PE). Por isso o VALOR precisa aparecer cru no binário.
///
/// `#[used]` impede o compilador de descartar a constante (não é referenciada
/// no código); sem ele, o linker de cdylib remove dados mortos e o marcador
/// some do `.so` — quebrando o preflight. `#[no_mangle]` mantém o símbolo
/// estável. A string DEVE bater com `DEBUG_PLUGIN_MARKER` na extensão
/// (`src/core/server.ts`). NÃO renomear o valor — é contrato com a extensão.
#[used]
#[unsafe(no_mangle)]
pub static PAWNPRO_DEBUG_MARKER: [u8; 26] = *b"PAWNPRO_DEBUG_MARKER:0.2.0";

/// Mantém o marcador vivo até o link final. `#[used]` sozinho não basta para um
/// cdylib: o linker ainda pode descartar o dado por não ser referenciado nem
/// exportado, e o marcador some do `.so`. Ler a constante (via `read_volatile`,
/// que o otimizador não pode provar inútil) a partir do código vivo do plugin
/// cria uma dependência real que ancora a string no binário.
#[inline(never)]
fn anchor_marker() -> u8 {
    unsafe { core::ptr::read_volatile(&raw const PAWNPRO_DEBUG_MARKER[0]) }
}

/// Identificador de sessão padrão do canal plugin↔adaptador (socket local).
/// `PAWNPRO_DBG_SESSION` sobrescreve; plugin e adaptador derivam o mesmo nome.
const DEFAULT_SESSION: &str = "default";

#[derive(Default)]
struct Debugger;

impl SampPlugin for Debugger {
    fn on_load(&mut self) {
        // Ancora o marcador no binário (ver `anchor_marker`); o `black_box`
        // impede que a chamada seja otimizada para fora.
        std::hint::black_box(anchor_marker());

        // Sobe o canal (socket local) para o adaptador.
        let id =
            std::env::var("PAWNPRO_DBG_SESSION").unwrap_or_else(|_| DEFAULT_SESSION.to_string());
        bridge::start(id);

        // Idioma das mensagens de erro, do locale do editor (propagado pelo
        // adaptador). Ausente/desconhecido → inglês.
        if let Ok(loc) = std::env::var("PAWNPRO_DBG_LOCALE") {
            hook::set_locale(crate::runtime_error::Locale::from_tag(&loc));
        }

        // Carrega o bloco de debug do `.amx`, se o caminho foi informado, para a
        // inspeção saber os símbolos em escopo.
        if let Some(dbg) = load_debug_from_env() {
            hook::load_debug(dbg);
        }
    }

    fn on_amx_load(&mut self, amx: &Amx) {
        // Instala o debug hook do SDK nesta VM; a partir daqui ela chama
        // `on_debug_break` a cada linha (exige `.amx` compilado com `-d2`/`-d3`).
        samp::plugin::enable_debug_hook(amx);

        // Monta o mapa de opcodes desta VM (inverso de `amx_opcodelist` quando a
        // imagem está relocada), usado para detectar erro de runtime antes do
        // abort. Feito uma vez por VM, na carga.
        hook::load_opcode_map(amx);

        // Na PRIMEIRA VM (o gamemode), segura a carga até o adaptador enviar
        // os breakpoints iniciais (`Configured`) — senão um breakpoint em
        // código de carga como `OnGameModeInit` passaria antes de o adaptador
        // conectar. Timeout de segurança: se nada conectar, o servidor segue.
        // Só a primeira VM espera; filterscripts/outras não bloqueiam.
        if !FIRST_AMX_SEEN.swap(true, Ordering::SeqCst) {
            bridge::BRIDGE.wait_configured(std::time::Duration::from_secs(10));
        }
    }

    fn on_debug_break(&mut self, amx: &Amx) {
        // A VM bateu numa linha; o SDK roteia para cá. A decisão de pausar
        // (breakpoint/step), a inspeção e o bloqueio ficam no `hook`.
        hook::on_break(amx);
    }
}

/// Lê e parseia o bloco de debug do `.amx` apontado por `PAWNPRO_DBG_AMXDBG`
/// (o caminho do `.amx` compilado com `-d2`/`-d3`). Ausente/inválido → sem
/// inspeção. Usa `from_amx`, que extrai o bloco `AMX_DBG` do arquivo completo.
fn load_debug_from_env() -> Option<samp::debug::AmxDbg> {
    let path = PathBuf::from(std::env::var("PAWNPRO_DBG_AMXDBG").ok()?);
    let bytes = std::fs::read(path).ok()?;
    samp::debug::AmxDbg::from_amx(&bytes).ok()
}

initialize_plugin!(
    type: Debugger,
    natives: [],
);

#[cfg(test)]
mod tests {
    use super::*;

    /// O preflight da extensão (`isOfficialDebugPlugin` em `src/core/server.ts`)
    /// faz grep do prefixo `PAWNPRO_DEBUG_MARKER` no binário. Se este valor mudar
    /// sem alinhar a extensão, a depuração para de reconhecer o plugin oficial.
    #[test]
    fn marker_prefix_matches_extension_contract() {
        assert!(PAWNPRO_DEBUG_MARKER.starts_with(b"PAWNPRO_DEBUG_MARKER"));
    }
}
