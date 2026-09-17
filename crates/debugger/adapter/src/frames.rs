//! A pilha da última pausa.
//!
//! Chega pela conexão com o plugin e é consultada pela sessão, então as duas
//! dividem o mesmo cache. É por sessão, e não global: no núcleo, várias sessões
//! vivem no mesmo processo, e uma não pode ler a pilha da outra.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pawnpro_dbg_protocol::{Frame, Var};

/// Frames da última pausa. Índice 0 = topo (onde a VM parou).
#[derive(Clone, Default)]
pub struct FrameCache(Arc<Mutex<Vec<Frame>>>);

impl FrameCache {
    /// Os frames são dados simples: um panic no meio de uma escrita não deixa
    /// nada pela metade que valha recusar a leitura.
    fn lock(&self) -> MutexGuard<'_, Vec<Frame>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Troca a pilha pela de uma pausa nova.
    pub fn replace(&self, frames: Vec<Frame>) {
        *self.lock() = frames;
    }

    /// Esquece a pilha: o processo que a produziu não existe mais.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Todos os frames, para o `stackTrace`.
    #[must_use]
    pub fn all(&self) -> Vec<Frame> {
        self.lock().clone()
    }

    /// Variáveis em escopo no frame dado; vazio se o frame não existe.
    #[must_use]
    pub fn vars(&self, frame: usize) -> Vec<Var> {
        self.lock()
            .get(frame)
            .map(|f| f.vars.clone())
            .unwrap_or_default()
    }

    /// Reflete no cache uma célula editada — a variável `var` do `frame`, ou
    /// o elemento no caminho `path` dela —, para o painel mostrar o valor novo
    /// sem reler a VM (o plugin já o escreveu).
    #[allow(
        clippy::significant_drop_tightening,
        reason = "o nó alterado é emprestado da trava: ela não pode sair antes"
    )]
    pub fn update_path(&self, frame: usize, var: usize, path: &[usize], value: &str) {
        let mut frames = self.lock();
        let Some(mut node) = frames.get_mut(frame).and_then(|f| f.vars.get_mut(var)) else {
            return;
        };
        for &i in path {
            let Some(child) = node.children.get_mut(i) else {
                return;
            };
            node = child;
        }
        value.clone_into(&mut node.value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: &str) -> Frame {
        Frame {
            name: "main".into(),
            file: None,
            line: Some(1),
            vars: vec![Var {
                name: "x".into(),
                value: value.into(),
                children: Vec::new(),
            }],
        }
    }

    /// No núcleo, duas sessões vivem no mesmo processo: a pausa de uma não
    /// pode aparecer no painel da outra.
    #[test]
    fn sessions_do_not_share_frames() {
        let first = FrameCache::default();
        let second = FrameCache::default();
        first.replace(vec![frame("1")]);
        assert!(second.all().is_empty());
    }

    #[test]
    fn clones_share_the_same_frames() {
        let session = FrameCache::default();
        let link = session.clone();
        link.replace(vec![frame("1")]);
        session.update_path(0, 0, &[], "2");
        assert_eq!(link.vars(0)[0].value, "2");
    }

    #[test]
    fn clear_forgets_the_previous_pause() {
        let cache = FrameCache::default();
        cache.replace(vec![frame("1")]);
        cache.clear();
        assert!(cache.all().is_empty());
        assert!(cache.vars(0).is_empty());
    }
}
