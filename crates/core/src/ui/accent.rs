//! Paleta de cores de destaque das páginas da extensão.
//!
//! Fechada de propósito: o valor entra direto em CSS, e uma cor livre não
//! garantiria contraste nos temas claro e escuro. Sem relação com o realce de
//! sintaxe, que tem esquema próprio.

use serde::{Deserialize, Serialize};

/// Cor de destaque escolhida nas configurações.
///
/// `Auto` cai nas variáveis do tema do editor. Fechado porque cada tom foi
/// verificado por contraste.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentColor {
    /// Segue o tema do editor (o `''` da configuração).
    #[serde(rename = "")]
    #[default]
    Auto,
    Blue,
    Purple,
    Green,
    Amber,
    Pink,
    Teal,
}

/// Os três tons de uma cor de destaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccentPalette {
    /// Fundo de botões e preenchimento do estado ativo.
    pub base: &'static str,
    /// O mesmo tom, um passo mais escuro, para hover.
    pub hover: &'static str,
    /// Texto sobre `base` — escolhido pelo contraste, não pelo tema.
    pub on: &'static str,
}

impl AccentColor {
    /// Ordem de exibição na página de configurações.
    pub const ORDER: [Self; 6] = [
        Self::Blue,
        Self::Purple,
        Self::Green,
        Self::Amber,
        Self::Pink,
        Self::Teal,
    ];

    /// Chave usada na configuração e no CSS.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Auto => "",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Green => "green",
            Self::Amber => "amber",
            Self::Pink => "pink",
            Self::Teal => "teal",
        }
    }

    /// Tons desta cor, ou `None` no modo automático.
    ///
    /// Todos acima de 4.5:1 com texto branco (mínimo AA) e claros o bastante
    /// para não sumirem num tema escuro. O hover escurece: clarear reprovava
    /// três das seis no contraste.
    #[must_use]
    pub const fn palette(self) -> Option<AccentPalette> {
        let p = match self {
            Self::Auto => return None,
            Self::Blue => AccentPalette {
                base: "#0e639c",
                hover: "#0b5484",
                on: "#ffffff",
            },
            Self::Purple => AccentPalette {
                base: "#68417a",
                hover: "#583767",
                on: "#ffffff",
            },
            Self::Green => AccentPalette {
                base: "#2d7d46",
                hover: "#266a3b",
                on: "#ffffff",
            },
            Self::Amber => AccentPalette {
                base: "#8a5a00",
                hover: "#754c00",
                on: "#ffffff",
            },
            Self::Pink => AccentPalette {
                base: "#a63b6d",
                hover: "#8d325c",
                on: "#ffffff",
            },
            Self::Teal => AccentPalette {
                base: "#00707a",
                hover: "#005f67",
                on: "#ffffff",
            },
        };
        Some(p)
    }
}

/// Bloco CSS para injetar no `<style>` de uma `WebView`.
///
/// No modo automático as variáveis apontam para as do editor.
#[must_use]
pub fn accent_css(accent: AccentColor) -> String {
    let p = accent.palette();
    let base = p.map_or("var(--vscode-button-background, #007acc)", |p| p.base);
    let hover = p.map_or("var(--vscode-button-hoverBackground, #0062a3)", |p| p.hover);
    let on = p.map_or("var(--vscode-button-foreground, #fff)", |p| p.on);
    // Variáveis PRÓPRIAS, não as do editor: o editor injeta as dele no atributo
    // style do <html>, e declaração inline vence qualquer seletor — redefinir
    // --vscode-* num :root não teria efeito nenhum.
    format!(
        "
  :root {{
    --pp-accent: {base};
    --pp-accent-hover: {hover};
    --pp-accent-fg: {on};
  }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_colors_in_page_order() {
        assert_eq!(AccentColor::ORDER.len(), 6);
        let keys: Vec<_> = AccentColor::ORDER.iter().map(|a| a.key()).collect();
        assert_eq!(keys, ["blue", "purple", "green", "amber", "pink", "teal"]);
    }

    #[test]
    fn every_ordered_color_has_a_palette() {
        for a in AccentColor::ORDER {
            assert!(a.palette().is_some(), "{a:?}");
        }
    }

    #[test]
    fn auto_has_no_palette_and_falls_back_to_editor_theme() {
        assert!(AccentColor::Auto.palette().is_none());
        let css = accent_css(AccentColor::Auto);
        // As três apontam para o tema do editor. Os `#` que aparecem são os
        // fallbacks do próprio `var()`, não uma cor da paleta.
        assert!(css.contains("var(--vscode-button-background"));
        assert!(css.contains("var(--vscode-button-hoverBackground"));
        assert!(css.contains("var(--vscode-button-foreground"));
        for a in AccentColor::ORDER {
            let p = a.palette().expect("paleta");
            assert!(!css.contains(p.base), "modo automático não fixa {a:?}");
        }
    }

    #[test]
    fn hover_darkens_instead_of_lightening() {
        // Clarear reduziria o contraste com o texto branco por cima.
        for a in AccentColor::ORDER {
            let p = a.palette().expect("paleta");
            let lum = |hex: &str| -> u32 { u32::from_str_radix(&hex[1..], 16).expect("hex") };
            assert!(lum(p.hover) < lum(p.base), "{a:?} clareia no hover");
        }
    }

    #[test]
    fn css_declares_the_three_variables() {
        let css = accent_css(AccentColor::Blue);
        assert!(css.contains("--pp-accent: #0e639c"));
        assert!(css.contains("--pp-accent-hover: #0b5484"));
        assert!(css.contains("--pp-accent-fg: #ffffff"));
    }

    #[test]
    fn serde_uses_the_configuration_keys() {
        assert_eq!(
            serde_json::to_string(&AccentColor::Blue).unwrap(),
            "\"blue\""
        );
        // O modo automático é `''` na configuração do usuário.
        assert_eq!(serde_json::to_string(&AccentColor::Auto).unwrap(), "\"\"");
        let back: AccentColor = serde_json::from_str("\"\"").unwrap();
        assert_eq!(back, AccentColor::Auto);
    }
}
