//! Cores literais do SA-MP/open.mp em código Pawn.
//!
//! Formato oficial (open.mp): `0xRRGGBBAA` — o alpha é o ÚLTIMO byte.
//! Confirmado pela doc de `SetPlayerColor`: vermelho = `0xFF0000FF`.
//!
//! Também aceita `0xRRGGBB` (6 dígitos, sem alpha): tratado como opaco (A=FF).
//!
//! E o formato de cor embutida em texto do SA-MP, `{RRGGBB}` (chat, textdraws,
//! `GameText`): 6 dígitos hex entre chaves, sempre opaco e sem alpha.
//!
//! Só varredura de texto e conversão de cor: a camada do editor liga isto ao
//! `DocumentColorProvider`.

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Cor com canais normalizados em 0..=1, como o editor espera.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RgbaColor {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

/// Quantidade de dígitos hex de um literal.
///
/// É `enum` e não número porque só 6 e 8 existem — e o formato original precisa
/// ser preservado na reescrita.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HexDigits {
    /// `0xRRGGBB` — sem alpha, tratado como opaco.
    Six,
    /// `0xRRGGBBAA` — o formato oficial do open.mp.
    Eight,
}

/// Um literal de cor encontrado no texto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorLiteral {
    /// Offset do início do literal no texto.
    pub start: usize,
    /// Offset do fim (exclusivo). Inclui o `± N` quando presente.
    pub end: usize,
    pub digits: HexDigits,
    /// Cor resultante, já com o alpha ajustado quando há `alpha_add`.
    pub color: RgbaColor,
    /// Idioma SA-MP de ajuste de alpha por aritmética: `0xRRGGBBAA + N` / `- N`.
    ///
    /// Guarda o operando N com sinal, para reconstruir a forma `base±N` na
    /// edição. Só é preenchido quando a soma afeta apenas o byte de alpha.
    pub alpha_add: Option<i32>,
    /// Formato `{RRGGBB}` do SA-MP. Sempre 6 dígitos, opaco. Guardado para
    /// reescrever no mesmo formato — senão viraria `0x...`.
    pub braces: bool,
}

fn byte_to_unit(b: u8) -> f64 {
    f64::from(b) / 255.0
}

/// Converte 0..=1 para 0..=255, com corte nas pontas.
///
/// O corte vem antes da conversão porque o editor pode entregar um canal fora
/// da faixa, e um cast direto viraria lixo.
fn unit_to_byte(u: f64) -> u8 {
    let scaled = (u * 255.0).round();
    if scaled <= 0.0 {
        return 0;
    }
    if scaled >= 255.0 {
        return 255;
    }
    // Os dois `return` acima já cortaram a faixa, então o valor cabe em `u8`.
    // O `allow` é o preço de o compilador não conseguir provar isso sozinho —
    // a alternativa seria uma busca linear, que trocaria clareza por nada.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        scaled as u8
    }
}

fn alpha_byte(color: &RgbaColor) -> u8 {
    unit_to_byte(color.alpha)
}

/// Decodifica os dígitos hex (6 ou 8) para RGBA normalizado.
#[must_use]
pub fn parse_hex_color(hex: &str) -> Option<RgbaColor> {
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some(RgbaColor {
        red: byte_to_unit(byte(0)?),
        green: byte_to_unit(byte(2)?),
        blue: byte_to_unit(byte(4)?),
        alpha: if hex.len() == 8 {
            byte_to_unit(byte(6)?)
        } else {
            1.0
        },
    })
}

/// `0x` seguido de exatamente 8 ou 6 dígitos hex, com fronteira de palavra
/// depois para não casar `0xF97804FFAB` (10 dígitos) como se fosse 8.
///
/// Os grupos 2 e 3, opcionais, são o operador `+`/`-` e um inteiro decimal — o
/// idioma de ajuste de alpha.
fn color_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"\b0x([0-9A-Fa-f]{8}|[0-9A-Fa-f]{6})\b(?:\s*([+-])\s*(\d+))?")
            .expect("regex de cor é constante e válida")
    })
}

/// Cor embutida do SA-MP: `{RRGGBB}`, exatamente 6 dígitos hex entre chaves.
///
/// Aparece dentro de strings de chat e textdraw, como `"{FF0000}Vermelho"`.
fn braces_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"\{([0-9A-Fa-f]{6})\}").expect("regex de chaves é constante e válida")
    })
}

/// Varre o texto e devolve todos os literais de cor encontrados.
#[must_use]
pub fn find_color_literals(text: &str) -> Vec<ColorLiteral> {
    let mut out = Vec::new();

    for c in color_regex().captures_iter(text) {
        let (Some(whole), Some(hex_match)) = (c.get(0), c.get(1)) else {
            continue;
        };
        let hex = hex_match.as_str();
        let Some(color) = parse_hex_color(hex) else {
            continue;
        };
        let digits = if hex.len() == 8 {
            HexDigits::Eight
        } else {
            HexDigits::Six
        };
        let literal_end = whole.start() + 2 + hex.len();

        // Idioma `0x...AA ± N`: ajusta o byte de alpha por aritmética. Só é
        // interpretado quando o literal tem alpha explícito (8 dígitos) E o
        // resultado cabe em 0..=255 — isto é, a soma afeta apenas o byte de
        // alpha, sem carry para o byte azul. Fora disso o resultado dependeria
        // dos outros canais e o swatch enganaria.
        if let (Some(op), Some(num), HexDigits::Eight) = (c.get(2), c.get(3), digits)
            && let Ok(n) = num.as_str().parse::<i32>()
        {
            let delta = if op.as_str() == "-" { -n } else { n };
            let result = i32::from(alpha_byte(&color)) + delta;
            // `try_from` no lugar de um cast com `allow`: a faixa 0..=255 é
            // exatamente o que torna a conversão válida, e deixá-la explícita
            // dispensa silenciar o compilador para provar isso.
            if let Ok(alpha) = u8::try_from(result) {
                out.push(ColorLiteral {
                    start: whole.start(),
                    end: whole.end(),
                    digits,
                    color: RgbaColor {
                        alpha: byte_to_unit(alpha),
                        ..color
                    },
                    alpha_add: Some(delta),
                    braces: false,
                });
                continue;
            }
        }

        // Sem aritmética interpretável: só o literal base, sem consumir o `± N`.
        out.push(ColorLiteral {
            start: whole.start(),
            end: literal_end,
            digits,
            color,
            alpha_add: None,
            braces: false,
        });
    }

    for b in braces_regex().captures_iter(text) {
        let (Some(whole), Some(hex)) = (b.get(0), b.get(1)) else {
            continue;
        };
        let Some(color) = parse_hex_color(hex.as_str()) else {
            continue;
        };
        out.push(ColorLiteral {
            start: whole.start(),
            end: whole.end(),
            digits: HexDigits::Six,
            color,
            alpha_add: None,
            braces: true,
        });
    }

    out
}

fn hex_byte(v: u8) -> String {
    format!("{v:02X}")
}

fn rgb_hex(color: &RgbaColor) -> String {
    format!(
        "{}{}{}",
        hex_byte(unit_to_byte(color.red)),
        hex_byte(unit_to_byte(color.green)),
        hex_byte(unit_to_byte(color.blue))
    )
}

/// Formata uma cor de volta para literal Pawn.
///
/// `prefer` mantém o formato original quando possível: um literal de 6 dígitos
/// que continua opaco volta como 6 dígitos; se o usuário introduziu
/// transparência, é promovido a 8 — senão o alpha seria descartado em silêncio.
#[must_use]
pub fn format_hex_color(color: &RgbaColor, prefer: HexDigits) -> String {
    let a = alpha_byte(color);
    if prefer == HexDigits::Six && a == 255 {
        format!("0x{}", rgb_hex(color))
    } else {
        format!("0x{}{}", rgb_hex(color), hex_byte(a))
    }
}

/// Reescreve uma cor preservando o idioma `base±N` de ajuste de alpha.
///
/// O literal base mantém o alpha original e o operando `N` é recalculado para
/// atingir o novo alpha. Assim, editar `0x9900CC00+20` não achata a expressão
/// em `0x9900CC14` — mantém a forma que o autor escreveu.
#[must_use]
pub fn format_alpha_add_color(color: &RgbaColor, base_alpha_byte: u8) -> String {
    let base = format!("0x{}{}", rgb_hex(color), hex_byte(base_alpha_byte));
    let n = i32::from(alpha_byte(color)) - i32::from(base_alpha_byte);
    match n {
        0 => base,
        n if n > 0 => format!("{base}+{n}"),
        n => format!("{base}-{}", n.abs()),
    }
}

/// Reescreve uma cor no formato `{RRGGBB}` do SA-MP.
///
/// Esse formato não carrega alpha; o canal é descartado, porque o texto do jogo
/// não o usa.
#[must_use]
pub fn format_braces_color(color: &RgbaColor) -> String {
    format!("{{{}}}", rgb_hex(color))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(r: u8, g: u8, b: u8, a: u8) -> RgbaColor {
        RgbaColor {
            red: byte_to_unit(r),
            green: byte_to_unit(g),
            blue: byte_to_unit(b),
            alpha: byte_to_unit(a),
        }
    }

    #[test]
    fn alpha_is_the_last_byte() {
        // Da doc de SetPlayerColor: vermelho opaco é 0xFF0000FF, não 0xFFFF0000.
        let c = parse_hex_color("FF0000FF").expect("válido");
        assert_eq!(c, rgba(255, 0, 0, 255));
    }

    #[test]
    fn six_digits_are_opaque() {
        let c = parse_hex_color("FF0000").expect("válido");
        assert_eq!(alpha_byte(&c), 255);
    }

    #[test]
    fn rejects_other_lengths() {
        for hex in ["FF", "FFFFF", "FFFFFFF", "FFFFFFFFF", ""] {
            assert!(parse_hex_color(hex).is_none(), "{hex}");
        }
    }

    #[test]
    fn rejects_non_hex_digits() {
        assert!(parse_hex_color("GGGGGG").is_none());
    }

    #[test]
    fn finds_both_literal_widths() {
        let found = find_color_literals("a = 0xFF0000FF; b = 0x00FF00;");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].digits, HexDigits::Eight);
        assert_eq!(found[1].digits, HexDigits::Six);
    }

    #[test]
    fn word_boundary_rejects_longer_runs() {
        // `0xF97804FFAB` tem 10 dígitos: casar os 8 primeiros daria um swatch
        // para um número que não é aquela cor.
        assert!(find_color_literals("x = 0xF97804FFAB;").is_empty());
    }

    #[test]
    fn alpha_arithmetic_is_applied_when_it_stays_in_range() {
        let found = find_color_literals("0x9900CC00 + 20");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].alpha_add, Some(20));
        assert_eq!(alpha_byte(&found[0].color), 20);
        // O literal consome a expressão inteira, para a edição reescrevê-la.
        assert_eq!(found[0].end, "0x9900CC00 + 20".len());
    }

    #[test]
    fn alpha_subtraction_is_applied() {
        let found = find_color_literals("0x990000FF - 15");
        assert_eq!(found[0].alpha_add, Some(-15));
        assert_eq!(alpha_byte(&found[0].color), 240);
    }

    #[test]
    fn arithmetic_that_would_carry_is_ignored() {
        // 0xFF + 1 estouraria para o byte azul: o resultado dependeria dos
        // outros canais e o swatch enganaria.
        let found = find_color_literals("0x9900CCFF + 1");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].alpha_add, None);
        // Só o literal base é consumido, sem o `+ 1`.
        assert_eq!(found[0].end, "0x9900CCFF".len());
    }

    #[test]
    fn arithmetic_is_ignored_on_six_digit_literals() {
        // Sem alpha explícito não há o que ajustar.
        let found = find_color_literals("0x9900CC + 20");
        assert_eq!(found[0].alpha_add, None);
        assert_eq!(found[0].end, "0x9900CC".len());
    }

    #[test]
    fn finds_samp_brace_colors() {
        let found = find_color_literals("\"{FF0000}Vermelho\"");
        assert_eq!(found.len(), 1);
        assert!(found[0].braces);
        assert_eq!(found[0].color, rgba(255, 0, 0, 255));
        assert_eq!(found[0].start, 1);
        assert_eq!(found[0].end, 9);
    }

    #[test]
    fn six_digits_stay_six_when_still_opaque() {
        // Preserva o que o autor escreveu, em vez de promover sem motivo.
        assert_eq!(
            format_hex_color(&rgba(255, 0, 0, 255), HexDigits::Six),
            "0xFF0000"
        );
    }

    #[test]
    fn six_digits_grow_to_eight_when_alpha_appears() {
        // Manter 6 descartaria a transparência em silêncio.
        assert_eq!(
            format_hex_color(&rgba(255, 0, 0, 128), HexDigits::Six),
            "0xFF000080"
        );
    }

    #[test]
    fn eight_digits_stay_eight_even_when_opaque() {
        assert_eq!(
            format_hex_color(&rgba(255, 0, 0, 255), HexDigits::Eight),
            "0xFF0000FF"
        );
    }

    #[test]
    fn alpha_add_form_is_preserved_on_rewrite() {
        // Editar a cor não deve achatar `base+N` num literal único.
        let out = format_alpha_add_color(&rgba(0x99, 0x00, 0xCC, 20), 0);
        assert_eq!(out, "0x9900CC00+20");
    }

    #[test]
    fn alpha_add_collapses_when_the_delta_is_zero() {
        let out = format_alpha_add_color(&rgba(0x99, 0x00, 0xCC, 0), 0);
        assert_eq!(out, "0x9900CC00");
    }

    #[test]
    fn brace_format_drops_the_alpha_channel() {
        // O texto do jogo não usa alpha nesse formato.
        assert_eq!(format_braces_color(&rgba(255, 0, 0, 128)), "{FF0000}");
    }

    #[test]
    fn round_trip_preserves_the_value() {
        let text = "0xAABBCCDD";
        let found = find_color_literals(text);
        assert_eq!(format_hex_color(&found[0].color, found[0].digits), text);
    }

    #[test]
    fn out_of_range_channels_are_clamped() {
        let c = RgbaColor {
            red: 2.0,
            green: -1.0,
            blue: 0.5,
            alpha: 1.0,
        };
        assert_eq!(format_hex_color(&c, HexDigits::Six), "0xFF0080");
    }
}
