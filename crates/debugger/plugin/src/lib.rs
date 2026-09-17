//! Plugin do servidor (Componente 2 do debugger) — `cdylib` carregado pelo
//! SA-MP/open.mp. Instala o debug hook do AMX, decide pausas em breakpoint/step
//! ([`control`]) e atende a sessão de depuração pelo soquete do núcleo
//! ([`bridge`]).
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
mod program;
mod runtime_error;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use pawnpro_dbg_protocol::transport::env;
use samp::plugin::SampPlugin;
use samp::{initialize_plugin, prelude::Amx};

/// O programa em depuração, lido do `.amx` que a sessão passou. Vazio sem
/// sessão: nenhuma VM recebe o hook.
static PROGRAM: OnceLock<program::Fingerprint> = OnceLock::new();

/// `true` depois que a VM do programa carregou pela primeira vez: só essa carga
/// espera a sessão configurar os breakpoints.
static PROGRAM_LOADED: AtomicBool = AtomicBool::new(false);

/// Marcador que identifica este binário como o plugin oficial do depurador.
///
/// Distingue o plugin de um homônimo qualquer com o mesmo nome. A extensão faz um *grep de bytes* no `.so`/`.dll` procurando
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

#[derive(Default)]
struct Debugger;

impl SampPlugin for Debugger {
    fn on_load(&mut self) {
        // Ancora o marcador no binário (ver `anchor_marker`); o `black_box`
        // impede que a chamada seja otimizada para fora.
        std::hint::black_box(anchor_marker());

        // Idioma das mensagens, do locale do editor (propagado pela sessão).
        // Antes de conectar: a falha de conexão já sai nele. Ausente ou
        // desconhecido → inglês.
        if let Ok(loc) = std::env::var(env::LOCALE) {
            hook::set_locale(crate::runtime_error::Locale::from_tag(&loc));
        }

        // Conecta na sessão de depuração que subiu este servidor, se houver.
        bridge::start(bridge::Session::from_env(|name| std::env::var(name).ok()));

        // Lê o `.amx` em depuração: o bloco de debug, para a inspeção saber os
        // símbolos, e a identidade, para reconhecer a VM dele entre as outras.
        if let Some(bytes) = std::env::var(env::AMX_DEBUG)
            .ok()
            .and_then(|p| std::fs::read(p).ok())
        {
            if let Some(fingerprint) = program::Fingerprint::read(&bytes) {
                let _ = PROGRAM.set(fingerprint);
            }
            if let Ok(dbg) = samp::debug::AmxDbg::from_amx(&bytes) {
                hook::load_debug(dbg);
            }
        }
    }

    fn on_amx_load(&mut self, amx: &Amx) {
        // Só a VM do programa em depuração recebe o hook. As outras — o
        // gamemode quando se depura um filterscript, e vice-versa — nem passam
        // pelo plugin: com os endereços começando em zero em cada VM, um
        // breakpoint pararia no código delas.
        if !is_program(amx) {
            return;
        }
        // A partir daqui a VM chama `on_debug_break` a cada linha (exige `.amx`
        // compilado com `-d2`/`-d3`).
        samp::plugin::enable_debug_hook(amx);

        // Monta o mapa de opcodes desta VM (inverso de `amx_opcodelist` quando a
        // imagem está relocada), usado para detectar erro de runtime antes do
        // abort. Feito uma vez por VM, na carga.
        hook::load_opcode_map(amx);

        // Na primeira carga do programa, segura até a sessão enviar os
        // breakpoints iniciais (`Configured`) — senão um breakpoint em código
        // de carga como `OnGameModeInit` passaria antes de a sessão conectar.
        // O prazo cobre uma sessão que conectou e não configura.
        if !PROGRAM_LOADED.swap(true, Ordering::SeqCst) {
            bridge::BRIDGE.wait_configured(std::time::Duration::from_secs(10));
        }
    }

    fn on_debug_break(&mut self, amx: &Amx) {
        // A VM bateu numa linha; o SDK roteia para cá. A decisão de pausar
        // (breakpoint/step), a inspeção e o bloqueio ficam no `hook`.
        hook::on_break(amx);
    }
}

/// `true` se a VM carregada é o programa em depuração.
fn is_program(amx: &Amx) -> bool {
    let Some(expected) = PROGRAM.get() else {
        return false;
    };
    let Some(header) = amx.header() else {
        return false;
    };
    let base = header.as_ptr().cast::<u8>();
    // SAFETY: `base` é o início da imagem carregada, que tem pelo menos o
    // cabeçalho; a identidade só lê até o início do código, que o próprio
    // cabeçalho diz onde fica e que está dentro da imagem.
    let first: [u8; program::HEADER_LEN] =
        unsafe { std::ptr::read_unaligned(base.cast::<[u8; program::HEADER_LEN]>()) };
    let Ok(code) = usize::try_from(program::Fingerprint::code_start(&first)) else {
        return false;
    };
    if code < program::HEADER_LEN {
        return false;
    }
    let image = unsafe { std::slice::from_raw_parts(base, code) };
    program::Fingerprint::read(image).as_ref() == Some(expected)
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
