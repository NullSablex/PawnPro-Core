//! Sincronização da pausa entre a thread da VM (hook) e a thread do adaptador.
//!
//! Quando o hook decide pausar, ele **bloqueia** em [`PauseGate::wait`] — o
//! servidor congela (esperado em dev local). A thread que atende o adaptador
//! chama [`PauseGate::resume`] com a ação escolhida (continuar ou um step),
//! liberando o hook. Lógica pura de sincronização: testável com threads, sem
//! TCP nem FFI.

use std::sync::{Condvar, Mutex};

use crate::control::StepMode;

/// Ação que tira a VM da pausa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// Voltar a rodar livremente (só para em breakpoint).
    Continue,
    /// Executar um passo no modo dado.
    Step(StepMode),
}

/// Portão de pausa: o hook espera, o adaptador libera. `None` = sem pedido ainda.
pub struct PauseGate {
    pending: Mutex<Option<Resume>>,
    cv: Condvar,
}

impl PauseGate {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            cv: Condvar::new(),
        }
    }

    /// Bloqueia até o adaptador chamar [`resume`](Self::resume); devolve a ação.
    /// Se o lock for envenenado (pânico em outra thread), assume `Continue` para
    /// não travar a VM para sempre.
    pub fn wait(&self) -> Resume {
        let Ok(mut guard) = self.pending.lock() else {
            return Resume::Continue;
        };
        while guard.is_none() {
            match self.cv.wait(guard) {
                Ok(g) => guard = g,
                Err(_) => return Resume::Continue,
            }
        }
        guard.take().unwrap_or(Resume::Continue)
    }

    /// Libera o hook que está em [`wait`](Self::wait), com a ação escolhida.
    pub fn resume(&self, action: Resume) {
        if let Ok(mut guard) = self.pending.lock() {
            *guard = Some(action);
            self.cv.notify_one();
        }
    }
}

impl Default for PauseGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn resume_unblocks_waiter() {
        let gate = Arc::new(PauseGate::new());
        let g2 = Arc::clone(&gate);
        let waiter = std::thread::spawn(move || g2.wait());

        // Dá um tempo para a thread entrar em wait, então libera.
        std::thread::sleep(Duration::from_millis(20));
        gate.resume(Resume::Step(StepMode::In));

        let action = waiter.join().unwrap();
        assert_eq!(action, Resume::Step(StepMode::In));
    }

    #[test]
    fn resume_before_wait_is_seen() {
        // Se o adaptador responder antes do hook entrar em wait, o pedido
        // pendente é consumido na hora (não perde o sinal).
        let gate = PauseGate::new();
        gate.resume(Resume::Continue);
        assert_eq!(gate.wait(), Resume::Continue);
    }
}
