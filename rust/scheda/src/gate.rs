//! The gate of an open Vault: every call passes it, `Vault.Close` shuts it.
//!
//! A call enters the gate before it does anything and holds its [`Pass`]
//! until it returns. Each blocking step of the call carries a pass of its
//! own into the blocking pool, so a step that outlives its call (the call
//! was dropped, and a thread of the pool cannot be stopped) is still inside
//! the gate. Shutting the gate refuses new calls (`FAILED_PRECONDITION`) and
//! waits, without a thread, until no call and no step is inside: after
//! that the Vault holds no lock a step took.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use scheda_api::v1::VaultErrorCode;
use scheda_blocking::{Blocking, blocking};
use tokio::sync::Notify;

use crate::error::{Error, Fault};
use crate::vault::Vault;

/// The count of what is inside, and whether the gate is shut.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    state: Mutex<State>,
    idle: Notify,
}

#[derive(Debug, Default)]
struct State {
    shut: bool,
    inside: usize,
}

impl Gate {
    /// The state of the gate. It holds two plain numbers that no panic can
    /// leave half written, so a poisoned lock is read as a clean one.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts one more inside, the gate open or not: a step of a call
    /// that is inside already.
    fn again(&self) {
        self.state().inside += 1;
    }

    /// Counts one more inside, unless the gate is shut.
    fn enter(&self) -> Result<(), Error> {
        let mut state = self.state();
        if state.shut {
            return Err(closed());
        }
        state.inside += 1;
        Ok(())
    }

    fn leave(&self) {
        let mut state = self.state();
        state.inside -= 1;
        if state.inside == 0 {
            self.idle.notify_waiters();
        }
    }

    /// Refuses new calls, then waits until nothing is inside.
    pub(crate) async fn shut(&self) {
        self.state().shut = true;
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            // Registered before the count is read, so a call that leaves
            // between the two still wakes this one.
            idle.as_mut().enable();
            if self.state().inside == 0 {
                return;
            }
            idle.await;
        }
    }
}

/// The failure of a call to a closed Vault.
pub(crate) fn closed() -> Error {
    Fault::new(
        VaultErrorCode::FailedPrecondition,
        "the Vault is closed; open it again",
    )
    .into()
}

/// One call, or one step of a call, inside the gate of a Vault, until it
/// is dropped. It holds the Vault, so the steps it runs reach it.
#[derive(Debug)]
pub(crate) struct Pass {
    vault: Arc<Vault>,
}

impl Pass {
    /// A call to `vault`: `FAILED_PRECONDITION` once the Vault is closed.
    pub(crate) fn enter(vault: &Arc<Vault>) -> Result<Pass, Error> {
        vault.gate().enter()?;
        Ok(Pass {
            vault: Arc::clone(vault),
        })
    }

    /// The Vault of the call.
    pub(crate) fn vault(&self) -> &Arc<Vault> {
        &self.vault
    }

    /// Runs `step` on the blocking pool with a pass of its own, so the
    /// gate counts the step until it ends, even when this call is dropped
    /// first.
    ///
    /// # Errors
    ///
    /// What `step` returns, and the failures of [`blocking`].
    pub(crate) async fn step<T, F>(&self, step: F) -> Result<T, Error>
    where
        F: FnOnce(&Blocking, &Vault) -> Result<T, Error> + Send + 'static,
        T: Send + 'static,
    {
        self.vault.gate().again();
        let pass = Pass {
            vault: Arc::clone(&self.vault),
        };
        blocking(move |cx| step(cx, &pass.vault)).await
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        self.vault.gate().leave();
    }
}
