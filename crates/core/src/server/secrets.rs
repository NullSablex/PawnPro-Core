//! Quais comandos não podem ir para o histórico.
//!
//! O histórico vai para `.pawnpro/state.json`, em texto claro e dentro do
//! projeto: um `login senha123` ali seria commitado junto. O comando ainda é
//! enviado; só não fica registrado.

use regex::Regex;
use std::sync::OnceLock;

/// Comandos cujo nome já indica credencial.
fn sensitive_commands() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"(?i)^(login|rcon_password|password|changepass(word)?|setpass(word)?)(\s|$)")
            .expect("regex de comandos sensíveis é constante e válida")
    })
}

/// Palavras que, num argumento, anunciam que o próximo termo é credencial.
///
/// Cobre `meucomando --senha 1234` e `auth token abc`.
fn secret_labels() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(
            r"(?i)^-{0,2}(pass|passwd|password|senha|pwd|token|key|chave|secret|segredo|auth|apikey)$",
        )
        .expect("regex de rótulos é constante e válida")
    })
}

/// Conservador de propósito: `kick 0` e `setpos 1.5 -2.0` são argumentos
/// comuns, e um falso positivo faria o histórico deixar de servir.
#[must_use]
pub fn looks_like_secret(term: &str) -> bool {
    // Em caracteres, não bytes: um acento contaria dobrado.
    if term.chars().count() < 8 {
        return false;
    }
    // Números, IP, coordenada: nada disso é credencial.
    if term
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ':' | '-'))
    {
        return false;
    }
    term.chars().any(|c| c.is_ascii_alphabetic()) && term.chars().any(|c| c.is_ascii_digit())
}

/// `true` se o comando traz credencial e não deve ser guardado.
///
/// Três camadas: o nome do comando, os comandos que o projeto declarou em
/// `server.history.sensitiveCommands`, e um argumento que se anuncie como
/// segredo ou pareça um.
#[must_use]
pub fn is_sensitive_command(cmd: &str, extras: &[String]) -> bool {
    // O prefixo `rcon` é opcional no painel e não faz parte do comando.
    let trimmed = cmd.trim();
    let text = strip_rcon_prefix(trimmed).trim();
    if text.is_empty() {
        return false;
    }

    if sensitive_commands().is_match(text) {
        return true;
    }

    let mut parts = text.split_whitespace();
    let Some(head) = parts.next() else {
        return false;
    };
    let head_lower = head.to_lowercase();
    if extras.iter().any(|e| e.trim().to_lowercase() == head_lower) {
        return true;
    }

    let args: Vec<&str> = parts.collect();
    for (i, arg) in args.iter().enumerate() {
        // `--senha 1234`: o rótulo entrega o próximo termo.
        if secret_labels().is_match(arg) && i + 1 < args.len() {
            return true;
        }
        // `--senha=1234` num termo só.
        if let Some((key, _)) = arg.split_once('=')
            && secret_labels().is_match(key)
        {
            return true;
        }
        if looks_like_secret(arg) {
            return true;
        }
    }
    false
}

/// Remove o `rcon ` ou `/rcon ` que o painel aceita como prefixo.
fn strip_rcon_prefix(cmd: &str) -> &str {
    let without_slash = cmd.strip_prefix('/').unwrap_or(cmd);
    let lower = without_slash.to_lowercase();
    if lower.starts_with("rcon ") || lower.starts_with("rcon\t") {
        return &without_slash[4..];
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_extras() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn commands_named_after_credentials_are_blocked() {
        for cmd in [
            "login senha123",
            "rcon_password abc",
            "password novo",
            "changepassword x",
            "changepass x",
            "setpassword x",
            "setpass x",
        ] {
            assert!(is_sensitive_command(cmd, &no_extras()), "{cmd}");
        }
    }

    #[test]
    fn the_rcon_prefix_does_not_hide_the_command() {
        // O painel aceita as duas formas; sem tirar o prefixo, `rcon login x`
        // passaria e a senha iria para o disco.
        assert!(is_sensitive_command("rcon login segredo", &no_extras()));
        assert!(is_sensitive_command("/rcon login segredo", &no_extras()));
        assert!(is_sensitive_command("RCON login segredo", &no_extras()));
    }

    #[test]
    fn ordinary_commands_are_kept() {
        // O custo de um falso positivo é o histórico deixar de servir.
        for cmd in ["gmx", "kick 0", "weather 11", "setpos 1.5 -2.0", "players"] {
            assert!(!is_sensitive_command(cmd, &no_extras()), "{cmd}");
        }
    }

    #[test]
    fn a_label_gives_away_the_next_term() {
        assert!(is_sensitive_command(
            "meucomando --senha 1234",
            &no_extras()
        ));
        assert!(is_sensitive_command("auth token abc", &no_extras()));
        assert!(is_sensitive_command("cmd --password x", &no_extras()));
    }

    #[test]
    fn a_label_with_equals_is_a_single_term() {
        assert!(is_sensitive_command("cmd --senha=1234", &no_extras()));
        assert!(is_sensitive_command("cmd apikey=abc123", &no_extras()));
    }

    #[test]
    fn a_dangling_label_without_a_value_is_not_enough() {
        // `cmd --senha` sozinho não tem o que esconder.
        assert!(!is_sensitive_command("cmd --senha", &no_extras()));
    }

    #[test]
    fn the_project_can_declare_its_own() {
        // `server.history.sensitiveCommands` do projeto.
        let extras = vec!["meuadmin".to_string()];
        assert!(is_sensitive_command("meuadmin qualquer", &extras));
        // A comparação é pelo primeiro termo, sem diferenciar maiúsculas.
        assert!(is_sensitive_command("MEUADMIN x", &extras));
        assert!(!is_sensitive_command("outro qualquer", &extras));
    }

    #[test]
    fn a_secret_looking_argument_is_blocked() {
        // Mistura letras e dígitos, e é longo: provavelmente credencial.
        assert!(looks_like_secret("abc12345"));
        assert!(is_sensitive_command("cmd abc12345xyz", &no_extras()));
    }

    #[test]
    fn short_or_numeric_arguments_are_not_secrets() {
        // Um IP, uma coordenada ou um número curto são argumentos comuns.
        for term in ["abc123", "127.0.0.1", "1.5", "-2.0", "12345678", "12:30:00"] {
            assert!(!looks_like_secret(term), "{term}");
        }
    }

    #[test]
    fn words_alone_are_not_secrets() {
        // Sem dígito não passa: nomes de jogador e de mapa são comuns.
        assert!(!looks_like_secret("nomedojogador"));
    }

    #[test]
    fn an_empty_command_is_not_sensitive() {
        assert!(!is_sensitive_command("", &no_extras()));
        assert!(!is_sensitive_command("   ", &no_extras()));
        assert!(!is_sensitive_command("rcon ", &no_extras()));
    }
}
