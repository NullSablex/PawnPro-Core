//! Dados que alimentam as páginas da extensão.
//!
//! Não desenha nada: a interface é responsabilidade da extensão, que tem a API
//! do editor. Aqui ficam as tabelas e regras que precisam ser as mesmas em todo
//! lugar — cores verificadas por contraste, por exemplo.

pub mod accent;
pub mod colors;
pub mod locale;
pub mod themes;
