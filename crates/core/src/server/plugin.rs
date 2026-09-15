//! Verificação do plugin de depuração antes de iniciar uma sessão.
//!
//! Quando algo está errado o servidor recusa o plugin no boot e escreve o
//! motivo no meio de dezenas de linhas de carga, onde ninguém vê. Conferir
//! antes deixa o editor dizer exatamente o que falta.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::types::ServerType;

use super::config::{detect_server_executable, detect_server_type};

/// Nome base do plugin de depuração, sem extensão.
pub const DEBUG_PLUGIN_NAME: &str = "pawnpro_debug";

/// Distingue o plugin oficial de um homônimo qualquer. Procurado nos bytes do
/// binário, sem parsear ELF/PE.
const DEBUG_PLUGIN_MARKER: &[u8] = b"PAWNPRO_DEBUG_MARKER";

/// Arquitetura de um executável ou biblioteca.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    X86,
    X64,
    Unknown,
}

/// Como o plugin está — ou deveria estar — instalado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebugInstallKind {
    /// SA-MP: `plugins/` mais a linha `plugins` no `server.cfg`.
    Samp,
    /// open.mp nativo, o recomendado: `components/`, auto-descoberto.
    OmpComponent,
    /// open.mp legado: `plugins/` mais `legacy_plugins` no `config.json`.
    OmpLegacy,
}

/// Divergência de arquitetura entre o plugin e o servidor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchMismatch {
    pub plugin: Architecture,
    pub server: Architecture,
}

/// O que está pronto e o que falta para depurar.
///
/// Os quatro bools codificam um estado só, com combinações impossíveis —
/// `plugin_file_present` e `plugin_name_clash` nunca são ambos `true`. Um enum
/// diria isso melhor, mas a struct é o contrato com a extensão, que lê os
/// campos separados: a troca fica para quando o TypeScript que a consome
/// migrar.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugPreflight {
    /// `true` se nada impede a depuração.
    pub ok: bool,
    /// O binário oficial foi encontrado num local válido.
    pub plugin_file_present: bool,
    /// Existe um arquivo com o nome do plugin que **não** é o oficial —
    /// provavelmente um homônimo.
    pub plugin_name_clash: bool,
    /// O plugin está registrado, quando o modo exige.
    pub plugin_registered: bool,
    pub server_type: ServerType,
    /// Onde instalar o plugin neste servidor.
    pub recommended_path: PathBuf,
    pub install_kind: DebugInstallKind,
    /// Arquiteturas diferentes: o servidor recusa o plugin no boot, e sem
    /// este aviso a depuração falharia em silêncio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch_mismatch: Option<ArchMismatch>,
}

/// `true` se o binário é o plugin oficial.
#[must_use]
pub fn is_official_debug_plugin(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    bytes
        .windows(DEBUG_PLUGIN_MARKER.len())
        .any(|w| w == DEBUG_PLUGIN_MARKER)
}

/// Arquitetura de um binário, pelo cabeçalho ELF ou PE.
///
/// O SA-MP e o open.mp legado são de 32 bits, e um plugin de 64 não carrega
/// neles. Basta o cabeçalho: em ELF o byte 4 é a classe; em PE, o campo
/// `Machine` após a assinatura.
#[must_use]
pub fn architecture_of(path: &Path) -> Architecture {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Architecture::Unknown;
    };
    let mut head = [0u8; 64];
    if file.read_exact(&mut head).is_err() {
        return Architecture::Unknown;
    }

    if head[..4] == [0x7f, b'E', b'L', b'F'] {
        return match head[4] {
            1 => Architecture::X86,
            2 => Architecture::X64,
            _ => Architecture::Unknown,
        };
    }

    if &head[..2] == b"MZ" {
        // `e_lfanew`, no offset 0x3C, aponta para a assinatura PE.
        let pe_offset = u32::from_le_bytes([head[0x3c], head[0x3d], head[0x3e], head[0x3f]]);
        if file.seek(SeekFrom::Start(u64::from(pe_offset))).is_err() {
            return Architecture::Unknown;
        }
        let mut sig = [0u8; 6];
        if file.read_exact(&mut sig).is_err() || &sig[..4] != b"PE\0\0" {
            return Architecture::Unknown;
        }
        return match u16::from_le_bytes([sig[4], sig[5]]) {
            0x014c => Architecture::X86,
            0x8664 => Architecture::X64,
            _ => Architecture::Unknown,
        };
    }

    Architecture::Unknown
}

/// Estado do arquivo do plugin numa pasta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginProbe {
    /// Existe e tem o marcador.
    Official,
    /// Existe, mas é outro binário com o mesmo nome.
    Clash,
    Absent,
}

fn probe_plugin_file(cwd: &Path, dir: &str, file: &str) -> PluginProbe {
    let path = cwd.join(dir).join(file);
    if !path.exists() {
        return PluginProbe::Absent;
    }
    if is_official_debug_plugin(&path) {
        PluginProbe::Official
    } else {
        PluginProbe::Clash
    }
}

/// Nome do arquivo do plugin nesta plataforma.
fn plugin_file_name() -> String {
    let ext = if cfg!(windows) { ".dll" } else { ".so" };
    format!("{DEBUG_PLUGIN_NAME}{ext}")
}

/// Só reporta quando as duas são conhecidas e diferentes: com um formato que
/// não sabemos ler, o silêncio é melhor que um alarme falso.
fn check_architecture(cwd: &Path, plugin_path: &Path) -> Option<ArchMismatch> {
    let exe = detect_server_executable(cwd)?;
    let plugin = architecture_of(plugin_path);
    let server = architecture_of(&exe);
    if plugin == Architecture::Unknown || server == Architecture::Unknown || plugin == server {
        return None;
    }
    Some(ArchMismatch { plugin, server })
}

/// `true` se o `server.cfg` lista o plugin na linha `plugins`.
fn registered_in_samp_cfg(cwd: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(cwd.join("server.cfg")) else {
        return false;
    };
    text.lines()
        .filter(|l| l.trim_start().to_lowercase().starts_with("plugins"))
        .any(|l| l.contains(DEBUG_PLUGIN_NAME))
}

/// `true` se o `config.json` lista o plugin em `pawn.legacy_plugins`.
fn registered_in_omp_legacy(cwd: &Path) -> bool {
    std::fs::read_to_string(cwd.join("config.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|json| {
            let list = json.get("pawn")?.get("legacy_plugins")?.as_array()?.clone();
            Some(
                list.iter()
                    .filter_map(Value::as_str)
                    .any(|p| p.contains(DEBUG_PLUGIN_NAME)),
            )
        })
        .unwrap_or(false)
}

/// Verifica se o servidor está pronto para depuração. Só lê.
///
/// - **SA-MP:** binário em `plugins/` e listado na linha `plugins`.
/// - **open.mp nativo:** `components/`, auto-descoberto — o recomendado.
/// - **open.mp legado:** `plugins/` mais `legacy_plugins`.
#[must_use]
pub fn check_debug_plugin(cwd: &Path) -> DebugPreflight {
    let file = plugin_file_name();
    let in_plugins = probe_plugin_file(cwd, "plugins", &file);
    let in_components = probe_plugin_file(cwd, "components", &file);
    let server_type = detect_server_type(cwd);

    if server_type != ServerType::Omp {
        let registered = registered_in_samp_cfg(cwd);
        let arch = check_architecture(cwd, &cwd.join("plugins").join(&file));
        return DebugPreflight {
            ok: in_plugins == PluginProbe::Official && registered && arch.is_none(),
            plugin_file_present: in_plugins == PluginProbe::Official,
            plugin_name_clash: in_plugins == PluginProbe::Clash,
            plugin_registered: registered,
            server_type,
            recommended_path: cwd.join("plugins").join(&file),
            install_kind: DebugInstallKind::Samp,
            arch_mismatch: arch,
        };
    }

    // Componente nativo do open.mp: auto-descoberto, sem registro.
    if in_components == PluginProbe::Official {
        let arch = check_architecture(cwd, &cwd.join("components").join(&file));
        return DebugPreflight {
            ok: arch.is_none(),
            plugin_file_present: true,
            plugin_name_clash: false,
            plugin_registered: true,
            server_type,
            recommended_path: cwd.join("components").join(&file),
            install_kind: DebugInstallKind::OmpComponent,
            arch_mismatch: arch,
        };
    }

    // Modo legado. Aqui `in_components` nunca é oficial — esse caso já retornou.
    let registered = registered_in_omp_legacy(cwd);
    let arch = check_architecture(cwd, &cwd.join("plugins").join(&file));
    DebugPreflight {
        ok: in_plugins == PluginProbe::Official && registered && arch.is_none(),
        plugin_file_present: in_plugins == PluginProbe::Official,
        plugin_name_clash: in_plugins == PluginProbe::Clash || in_components == PluginProbe::Clash,
        plugin_registered: registered,
        server_type,
        // Recomenda o caminho de componente mesmo aqui: é a forma preferida.
        recommended_path: cwd.join("components").join(&file),
        install_kind: if in_plugins == PluginProbe::Official {
            DebugInstallKind::OmpLegacy
        } else {
            DebugInstallKind::OmpComponent
        },
        arch_mismatch: arch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            p.push(format!("pawnpro-plug-{tag}-{nanos}"));
            std::fs::create_dir_all(&p).expect("criar temp");
            Self(p)
        }
        fn file(&self, rel: &str, body: &[u8]) -> PathBuf {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("criar dir");
            }
            std::fs::write(&path, body).expect("escrever");
            path
        }
        /// Um binário com o marcador do plugin oficial.
        fn official_plugin(&self, dir: &str) -> PathBuf {
            let name = plugin_file_name();
            let mut body = b"qualquer coisa antes ".to_vec();
            body.extend_from_slice(DEBUG_PLUGIN_MARKER);
            body.extend_from_slice(b" e depois");
            self.file(&format!("{dir}/{name}"), &body)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_marker_identifies_the_official_plugin() {
        let tmp = TempDir::new("marker");
        let official = tmp.official_plugin("plugins");
        assert!(is_official_debug_plugin(&official));
    }

    #[test]
    fn a_homonym_binary_is_not_the_official_plugin() {
        // Distinguir os dois é o que evita prometer depuração que não funciona.
        let tmp = TempDir::new("homonym");
        let fake = tmp.file(&format!("plugins/{}", plugin_file_name()), b"outro plugin");
        assert!(!is_official_debug_plugin(&fake));
    }

    #[test]
    fn a_missing_file_is_not_the_official_plugin() {
        assert!(!is_official_debug_plugin(Path::new("/nao/existe.so")));
    }

    #[test]
    fn elf_architecture_is_read_from_the_class_byte() {
        let tmp = TempDir::new("elf");
        let mut e32 = vec![0u8; 64];
        e32[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        e32[4] = 1;
        assert_eq!(architecture_of(&tmp.file("e32", &e32)), Architecture::X86);

        let mut e64 = vec![0u8; 64];
        e64[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        e64[4] = 2;
        assert_eq!(architecture_of(&tmp.file("e64", &e64)), Architecture::X64);
    }

    #[test]
    fn pe_architecture_is_read_from_the_machine_field() {
        let tmp = TempDir::new("pe");
        let mut pe = vec![0u8; 128];
        pe[0] = b'M';
        pe[1] = b'Z';
        // `e_lfanew` aponta para a assinatura PE.
        pe[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
        pe[64..68].copy_from_slice(b"PE\0\0");
        pe[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        assert_eq!(architecture_of(&tmp.file("pe64", &pe)), Architecture::X64);
    }

    #[test]
    fn an_unknown_format_is_not_guessed() {
        // Um alarme falso de arquitetura seria pior que o silêncio.
        let tmp = TempDir::new("unknown");
        assert_eq!(
            architecture_of(&tmp.file("x", &[0u8; 64])),
            Architecture::Unknown
        );
        assert_eq!(
            architecture_of(Path::new("/nao/existe")),
            Architecture::Unknown
        );
    }

    #[test]
    fn a_truncated_file_is_not_guessed() {
        let tmp = TempDir::new("short");
        assert_eq!(
            architecture_of(&tmp.file("s", b"MZ")),
            Architecture::Unknown
        );
    }

    #[test]
    fn samp_needs_the_plugin_registered_in_server_cfg() {
        let tmp = TempDir::new("samp-reg");
        tmp.file("server.cfg", b"port 7777\n");
        tmp.official_plugin("plugins");
        let pre = check_debug_plugin(&tmp.0);
        assert_eq!(pre.server_type, ServerType::Samp);
        assert!(pre.plugin_file_present);
        assert!(
            !pre.plugin_registered,
            "sem a linha `plugins` não está registrado"
        );
        assert!(!pre.ok);
    }

    #[test]
    fn samp_is_ready_when_file_and_registration_are_present() {
        let tmp = TempDir::new("samp-ok");
        tmp.file("server.cfg", b"plugins pawnpro_debug\n");
        tmp.official_plugin("plugins");
        let pre = check_debug_plugin(&tmp.0);
        assert!(pre.plugin_registered);
        assert!(pre.ok);
        assert_eq!(pre.install_kind, DebugInstallKind::Samp);
    }

    #[test]
    fn a_name_clash_is_reported_separately() {
        // Não é "faltando": o usuário precisa saber que o arquivo está lá mas
        // é outro binário.
        let tmp = TempDir::new("clash");
        tmp.file("server.cfg", b"plugins pawnpro_debug\n");
        tmp.file(&format!("plugins/{}", plugin_file_name()), b"outro");
        let pre = check_debug_plugin(&tmp.0);
        assert!(pre.plugin_name_clash);
        assert!(!pre.plugin_file_present);
        assert!(!pre.ok);
    }

    #[test]
    fn the_openmp_component_needs_no_registration() {
        // É auto-descoberto: exigir registro seria instrução errada.
        let tmp = TempDir::new("omp-comp");
        tmp.file("config.json", br#"{"pawn":{}}"#);
        tmp.official_plugin("components");
        let pre = check_debug_plugin(&tmp.0);
        assert_eq!(pre.server_type, ServerType::Omp);
        assert_eq!(pre.install_kind, DebugInstallKind::OmpComponent);
        assert!(pre.plugin_registered);
        assert!(pre.ok);
    }

    #[test]
    fn the_openmp_legacy_mode_needs_legacy_plugins() {
        let tmp = TempDir::new("omp-legacy");
        tmp.file(
            "config.json",
            br#"{"pawn":{"legacy_plugins":["pawnpro_debug"]}}"#,
        );
        tmp.official_plugin("plugins");
        let pre = check_debug_plugin(&tmp.0);
        assert_eq!(pre.install_kind, DebugInstallKind::OmpLegacy);
        assert!(pre.plugin_registered);
        assert!(pre.ok);
    }

    #[test]
    fn the_component_path_is_recommended_even_in_legacy_mode() {
        // É a forma preferida no open.mp.
        let tmp = TempDir::new("omp-rec");
        tmp.file("config.json", br#"{"pawn":{}}"#);
        let pre = check_debug_plugin(&tmp.0);
        assert!(
            pre.recommended_path
                .to_string_lossy()
                .contains("components")
        );
    }
}
