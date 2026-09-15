//! As listas do assistente de nomes: arquivos `.ban` e `.allow`.
//!
//! Os termos moravam inline no `config.json` e passaram a viver em arquivos
//! próprios, que o desenvolvedor edita sem mexer na configuração. Este módulo
//! cuida da transição.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::manager::{ConfigManager, Scope};

/// Título do arquivo de nomes proibidos.
pub const BLOCKLIST_TITLE: &str = "PawnPro — nomes proibidos";
/// Título do arquivo de índices de laço tolerados.
pub const LOOP_INDICES_TITLE: &str = "PawnPro — índices de loop tolerados";

/// O arquivo é para o desenvolvedor editar; sem o cabeçalho ele teria de
/// adivinhar o formato.
#[must_use]
pub fn list_file_header(title: &str) -> String {
    format!(
        "# {title}\n\
         # Um termo por linha. Linhas em branco e iniciadas por # são ignoradas.\n\
         # Editável livremente — o PawnPro relê a cada alteração.\n\n"
    )
}

/// Os termos de um arquivo de lista, sem comentários nem linhas vazias.
///
/// Vazio quando o arquivo não existe, não é legível, ou passa de `max_bytes` —
/// o limite não restringe o que o desenvolvedor escreve, impede o core de
/// carregar na memória um arquivo absurdo. Era a engine quem lia isto; ela não
/// lê mais nada do disco.
#[must_use]
pub fn read_list_file(path: &Path, max_bytes: u64) -> Vec<String> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max_bytes) {
        return Vec::new();
    }
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Cria o arquivo, se ainda não existir.
///
/// Não sobrescreve: o existente foi editado pelo desenvolvedor. Falhar não é
/// fatal — a análise cai na lista inline.
pub fn seed_list_file(path: &Path, title: &str, items: &[String]) {
    if path.as_os_str().is_empty() || path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = format!("{}{}\n", list_file_header(title), items.join("\n"));
    let _ = std::fs::write(path, body);
}

/// Acrescenta termos sem duplicar o que já está lá.
///
/// Duplicar não quebraria a análise, mas encheria o arquivo que o
/// desenvolvedor precisa ler.
pub fn append_list_file(path: &Path, title: &str, items: &[String]) {
    if path.as_os_str().is_empty() || items.is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let present: Vec<String> = existing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(ToString::to_string)
        .collect();

    let fresh: Vec<&String> = items
        .iter()
        .filter(|t| !present.contains(&t.trim().to_string()))
        .collect();
    if fresh.is_empty() {
        return;
    }

    let base = if existing.is_empty() {
        list_file_header(title)
    } else {
        existing
    };
    let separator = if base.ends_with('\n') { "" } else { "\n" };
    let joined = fresh
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(path, format!("{base}{separator}{joined}\n"));
}

/// Garante que os dois arquivos existam, semeados com o que está na
/// configuração.
pub fn ensure_naming_files(manager: &ConfigManager) {
    let naming = &manager.get_all().analysis.naming;
    seed_list_file(
        Path::new(&naming.blocklist_file),
        BLOCKLIST_TITLE,
        &naming.blocklist,
    );
    seed_list_file(
        Path::new(&naming.loop_indices_file),
        LOOP_INDICES_TITLE,
        &naming.allow_short_in_loops,
    );
}

/// `true` se ainda há listas inline no `config.json` do projeto.
#[must_use]
pub fn has_inline_naming_lists(manager: &ConfigManager) -> bool {
    !manager.raw_project_naming_list("blocklist").is_empty()
        || !manager
            .raw_project_naming_list("allowShortInLoops")
            .is_empty()
}

/// Serve ao aviso de tamanho: uma lista enorme demora a gravar.
#[must_use]
pub fn inline_naming_bytes(manager: &ConfigManager) -> usize {
    let size = |items: Vec<String>| items.join("\n").len();
    size(manager.raw_project_naming_list("blocklist"))
        + size(manager.raw_project_naming_list("allowShortInLoops"))
}

/// Quantos termos foram movidos de cada lista.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationResult {
    pub blocklist: usize,
    pub loop_indices: usize,
}

/// O que a migração moveria, para o usuário conferir depois.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamingBackup {
    pub blocklist: Vec<String>,
    pub allow_short_in_loops: Vec<String>,
}

/// Salva um backup apenas dos termos a migrar.
///
/// Um backup do `config.json` inteiro faria o usuário procurar o que mudou.
#[must_use]
pub fn backup_naming_lists(manager: &ConfigManager, dest: &Path) -> Option<PathBuf> {
    let backup = NamingBackup {
        blocklist: manager.raw_project_naming_list("blocklist"),
        allow_short_in_loops: manager.raw_project_naming_list("allowShortInLoops"),
    };
    if backup.blocklist.is_empty() && backup.allow_short_in_loops.is_empty() {
        return None;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let json = serde_json::to_string_pretty(&backup).ok()?;
    std::fs::write(dest, format!("{json}\n")).ok()?;
    Some(dest.to_path_buf())
}

/// Move as listas inline para os arquivos e as remove do `config.json`.
///
/// Anexa em vez de sobrescrever: o arquivo pode ter termos que o desenvolvedor
/// acrescentou à mão.
///
/// # Errors
/// Falha ao remover a chave. Os arquivos já foram escritos nesse ponto, e uma
/// segunda tentativa não duplica nada.
pub fn migrate_naming_lists(
    manager: &mut ConfigManager,
) -> Result<MigrationResult, super::manager::ConfigError> {
    let naming = manager.get_all().analysis.naming.clone();

    let blocklist = manager.raw_project_naming_list("blocklist");
    let loop_indices = manager.raw_project_naming_list("allowShortInLoops");

    let mut result = MigrationResult::default();

    if !blocklist.is_empty() {
        append_list_file(
            Path::new(&naming.blocklist_file),
            BLOCKLIST_TITLE,
            &blocklist,
        );
        manager.delete_key("analysis.naming.blocklist", Scope::Project)?;
        result.blocklist = blocklist.len();
    }
    if !loop_indices.is_empty() {
        append_list_file(
            Path::new(&naming.loop_indices_file),
            LOOP_INDICES_TITLE,
            &loop_indices,
        );
        manager.delete_key("analysis.naming.allowShortInLoops", Scope::Project)?;
        result.loop_indices = loop_indices.len();
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    /// Limite usado nos testes que não estão medindo o limite.
    const LIST_LIMIT: u64 = 32 * 1024 * 1024;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            p.push(format!("pawnpro-naming-{tag}-{nanos}"));
            std::fs::create_dir_all(&p).expect("criar temp");
            Self(p)
        }
        fn write_project_config(&self, body: &str) {
            let dir = self.0.join("proj").join(".pawnpro");
            std::fs::create_dir_all(&dir).expect("criar dir");
            std::fs::write(dir.join("config.json"), body).expect("escrever");
        }
        fn manager(&self) -> ConfigManager {
            ConfigManager::new(&self.0.join("proj"), &self.0.join("home"))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn items(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_header_explains_the_format() {
        // O arquivo é para o dev editar; sem isto ele teria de adivinhar.
        let header = list_file_header("Teste");
        assert!(header.contains("# Teste"));
        assert!(header.contains("Um termo por linha"));
    }

    #[test]
    fn reading_skips_comments_and_blank_lines() {
        let tmp = TempDir::new("read");
        let path = tmp.0.join("lista.ban");
        std::fs::write(&path, "# comentário\n\n  termo1  \ntermo2\n").expect("escrever");
        assert_eq!(read_list_file(&path, LIST_LIMIT), ["termo1", "termo2"]);
    }

    #[test]
    fn seeding_does_not_overwrite_an_existing_file() {
        // Um arquivo existente foi editado pelo dev; regravá-lo apagaria o
        // trabalho dele.
        let tmp = TempDir::new("seed");
        let path = tmp.0.join("lista.ban");
        std::fs::write(&path, "meu-termo\n").expect("escrever");
        seed_list_file(&path, BLOCKLIST_TITLE, &items(&["outro"]));
        assert_eq!(read_list_file(&path, LIST_LIMIT), ["meu-termo"]);
    }

    #[test]
    fn seeding_creates_the_file_with_the_defaults() {
        let tmp = TempDir::new("seed-new");
        let path = tmp.0.join("sub").join("lista.ban");
        seed_list_file(&path, BLOCKLIST_TITLE, &items(&["tmp", "foo"]));
        assert_eq!(read_list_file(&path, LIST_LIMIT), ["tmp", "foo"]);
        let raw = std::fs::read_to_string(&path).expect("ler");
        assert!(raw.starts_with("# PawnPro"));
    }

    #[test]
    fn appending_keeps_what_the_developer_wrote() {
        // Perder edições seria pior que a duplicação que a migração evita.
        let tmp = TempDir::new("append");
        let path = tmp.0.join("lista.ban");
        std::fs::write(&path, "# cabeçalho\nmeu-termo\n").expect("escrever");
        append_list_file(&path, BLOCKLIST_TITLE, &items(&["novo"]));
        assert_eq!(read_list_file(&path, LIST_LIMIT), ["meu-termo", "novo"]);
    }

    #[test]
    fn appending_does_not_duplicate() {
        let tmp = TempDir::new("append-dup");
        let path = tmp.0.join("lista.ban");
        std::fs::write(&path, "termo\n").expect("escrever");
        append_list_file(&path, BLOCKLIST_TITLE, &items(&["termo", "outro"]));
        assert_eq!(read_list_file(&path, LIST_LIMIT), ["termo", "outro"]);
    }

    #[test]
    fn migration_moves_the_terms_and_clears_the_config() {
        let tmp = TempDir::new("migrate");
        let ban = tmp.0.join("proj").join(".pawnpro").join("nomes.ban");
        tmp.write_project_config(&format!(
            r#"{{"analysis":{{"naming":{{"blocklist":["x","y"],"blocklistFile":{:?}}}}}}}"#,
            ban.to_string_lossy()
        ));

        let mut manager = tmp.manager();
        assert!(has_inline_naming_lists(&manager));

        let result = migrate_naming_lists(&mut manager).expect("migrar");
        assert_eq!(result.blocklist, 2);
        assert_eq!(read_list_file(&ban, LIST_LIMIT), ["x", "y"]);
        // O arquivo passa a ser a fonte única: o inline sai do JSON.
        assert!(!has_inline_naming_lists(&manager));
    }

    #[test]
    fn migrating_twice_does_not_duplicate() {
        // A segunda passada não tem o que mover, e o `append` ignoraria
        // repetidos de qualquer forma.
        let tmp = TempDir::new("migrate-twice");
        let ban = tmp.0.join("proj").join(".pawnpro").join("nomes.ban");
        tmp.write_project_config(&format!(
            r#"{{"analysis":{{"naming":{{"blocklist":["x"],"blocklistFile":{:?}}}}}}}"#,
            ban.to_string_lossy()
        ));
        let mut manager = tmp.manager();
        migrate_naming_lists(&mut manager).expect("migrar");
        let second = migrate_naming_lists(&mut manager).expect("migrar de novo");
        assert_eq!(second.blocklist, 0);
        assert_eq!(read_list_file(&ban, LIST_LIMIT), ["x"]);
    }

    #[test]
    fn nothing_inline_means_nothing_to_migrate() {
        let tmp = TempDir::new("empty");
        tmp.write_project_config("{}");
        let mut manager = tmp.manager();
        assert!(!has_inline_naming_lists(&manager));
        assert_eq!(
            migrate_naming_lists(&mut manager).expect("migrar"),
            MigrationResult::default()
        );
    }

    #[test]
    fn the_backup_holds_only_what_moves() {
        // Um backup do config.json inteiro faria o usuário procurar o que mudou.
        let tmp = TempDir::new("backup");
        tmp.write_project_config(r#"{"locale":"ru","analysis":{"naming":{"blocklist":["x"]}}}"#);
        let manager = tmp.manager();
        let dest = tmp.0.join("backup.json");
        assert_eq!(backup_naming_lists(&manager, &dest), Some(dest.clone()));

        let raw = std::fs::read_to_string(&dest).expect("ler");
        assert!(raw.contains("\"x\""));
        // A configuração que não está sendo migrada fica de fora.
        assert!(!raw.contains("\"ru\""));
    }

    #[test]
    fn no_backup_when_there_is_nothing_to_save() {
        let tmp = TempDir::new("backup-empty");
        tmp.write_project_config("{}");
        let manager = tmp.manager();
        assert_eq!(backup_naming_lists(&manager, &tmp.0.join("b.json")), None);
    }

    #[test]
    fn the_byte_count_covers_both_lists() {
        let tmp = TempDir::new("bytes");
        tmp.write_project_config(
            r#"{"analysis":{"naming":{"blocklist":["abc"],"allowShortInLoops":["de"]}}}"#,
        );
        let manager = tmp.manager();
        assert_eq!(inline_naming_bytes(&manager), 5);
    }

    #[test]
    fn an_oversized_list_is_refused_instead_of_loaded() {
        // O limite existe para o core não carregar na memória um arquivo
        // absurdo; a lista cai no que estiver escrito na configuração.
        let tmp = TempDir::new("oversized");
        let path = tmp.0.join("nomes.ban");
        std::fs::write(&path, "termo\noutro\n").expect("escrever");
        assert!(read_list_file(&path, 4).is_empty());
        assert_eq!(read_list_file(&path, 4096), ["termo", "outro"]);
    }
}
