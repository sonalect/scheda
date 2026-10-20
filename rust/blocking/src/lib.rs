//! The gate between async code and blocking work.
//!
//! Blocking work (a file read and `fsync`ed, a git object written, a lock
//! file tried) must not run on a thread of an async runtime: it would hold
//! up every task that thread drives. This crate makes the compiler keep it
//! off them. A function that blocks takes a [`Blocking`] token as its first
//! argument, and the only way to hold one is to be inside [`blocking`],
//! which runs its step on tokio's blocking pool. An async function that
//! calls such a function directly has no token to pass, so it does not
//! compile.
//!
//! The token cannot be made, copied, or kept past its step. Each claim
//! below is a pair of examples that differ in one line: the first compiles
//! and runs, the second does not compile.
//!
//! A step is called through the helper:
//!
//! ```
//! use scheda_blocking::{Blocking, blocking};
//!
//! fn read(_cx: &Blocking) -> Result<u32, protocontract::Error> {
//!     Ok(7)
//! }
//!
//! async fn call() -> Result<u32, protocontract::Error> {
//!     blocking(|cx| read(cx)).await
//! }
//!
//! let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
//! assert_eq!(runtime.block_on(call()).unwrap(), 7);
//! ```
//!
//! and not without it, since no one outside this crate can make a token:
//!
//! ```compile_fail
//! use scheda_blocking::{Blocking, blocking};
//!
//! fn read(_cx: &Blocking) -> Result<u32, protocontract::Error> {
//!     Ok(7)
//! }
//!
//! async fn call() -> Result<u32, protocontract::Error> {
//!     read(&Blocking { _private: () })
//! }
//! ```
//!
//! A step returns what it made:
//!
//! ```
//! use scheda_blocking::blocking;
//!
//! async fn call() -> Result<(), protocontract::Error> {
//!     blocking(|_cx| Ok(())).await
//! }
//! ```
//!
//! but not the token, which lives only as long as the step:
//!
//! ```compile_fail
//! use scheda_blocking::blocking;
//!
//! async fn call() -> Result<(), protocontract::Error> {
//!     let _kept = blocking(|cx| Ok(cx)).await?;
//!     Ok(())
//! }
//! ```
//!
//! nor a copy of it:
//!
//! ```compile_fail
//! use scheda_blocking::blocking;
//!
//! async fn call() -> Result<(), protocontract::Error> {
//!     let _kept = blocking(|cx| Ok(*cx)).await?;
//!     Ok(())
//! }
//! ```

use std::any::Any;

use protocontract::{Error, Failure, RuntimeCode};
use tokio::runtime::Handle;

/// The proof that the code holding it runs on tokio's blocking pool, where
/// it may block. Only [`blocking`] makes one, and lends it to its step for
/// the length of the step.
///
/// A function that blocks takes `&Blocking` as its first argument, named
/// `cx` by convention, and passes it on to the blocking functions it calls.
#[derive(Debug)]
pub struct Blocking {
    _private: (),
}

/// Runs `step` on the blocking pool of the tokio runtime of the caller and
/// returns what it returns.
///
/// The step starts when the future is first polled. Once it has started it
/// runs to its end even if the future is dropped: a thread of the pool
/// cannot be stopped, and a step is the smallest piece of work that must
/// not be cut in two. A caller that drops the future does not learn the
/// outcome.
///
/// # Errors
///
/// What `step` returns; `NO_RUNTIME` when called outside a tokio runtime;
/// `PANICKED` when the step panicked, with its message; `CANCELLED` when
/// the runtime shut down before the step could run.
pub async fn blocking<T, F>(step: F) -> Result<T, Error>
where
    F: FnOnce(&Blocking) -> Result<T, Error> + Send + 'static,
    T: Send + 'static,
{
    let handle = Handle::try_current().map_err(|error| {
        Error::from(
            Failure::new(
                RuntimeCode::NoRuntime,
                "blocking work runs on the blocking pool of a tokio runtime; call it inside one",
            )
            .with_source(error),
        )
    })?;
    handle
        .spawn_blocking(move || step(&Blocking { _private: () }))
        .await
        .unwrap_or_else(|error| Err(join_failed(error)))
}

/// The failure of a step that did not return: it panicked, or the runtime
/// shut down first.
fn join_failed(error: tokio::task::JoinError) -> Error {
    if error.is_panic() {
        Error::new(
            RuntimeCode::Panicked,
            format!(
                "a blocking step panicked: {}",
                panic_message(error.into_panic().as_ref())
            ),
        )
    } else {
        Failure::new(
            RuntimeCode::Cancelled,
            "the runtime shut down before a blocking step ran",
        )
        .with_source(error)
        .into()
    }
}

/// The message of a panic: the text `panic!` was given, as a `&str` or a
/// `String` (what [`std::panic::panic_any`] carries for any other type has
/// none). Takes the payload a [`tokio::task::JoinError::into_panic`] or a
/// [`std::panic::catch_unwind`] returns.
///
/// ```
/// let payload = std::panic::catch_unwind(|| panic!("boom")).unwrap_err();
/// assert_eq!(scheda_blocking::panic_message(payload.as_ref()), "boom");
///
/// let other = std::panic::catch_unwind(|| std::panic::panic_any(7_u8)).unwrap_err();
/// assert_eq!(scheda_blocking::panic_message(other.as_ref()), "no message");
/// ```
#[must_use]
pub fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use tokio::sync::oneshot;

    use super::*;

    #[test]
    fn outside_a_runtime_the_step_does_not_run_and_the_call_is_no_runtime() {
        let (ran, seen) = mpsc::channel::<()>();
        let call = blocking(move |_cx| {
            let _ = ran.send(());
            Ok(())
        });
        let error = poll_once(call).unwrap_err();
        assert!(error.is(RuntimeCode::NoRuntime));
        assert!(seen.try_recv().is_err(), "the step did not run");
    }

    /// Polls `future` once on this thread, outside any runtime.
    fn poll_once<F: Future<Output = Result<(), Error>>>(future: F) -> Result<(), Error> {
        use std::pin::pin;
        use std::task::{Context, Poll, Waker};
        let mut future = pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(outcome) => outcome,
            Poll::Pending => panic!("the call waited outside a runtime"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_step_runs_on_a_thread_other_than_the_one_of_the_runtime() {
        let caller = thread::current().id();
        let runner = blocking(|_cx| Ok(thread::current().id())).await.unwrap();
        assert_ne!(caller, runner);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_panic_in_the_step_is_panicked_with_its_message() {
        let error = blocking(|_cx| -> Result<(), Error> { panic!("the disk is gone") })
            .await
            .unwrap_err();
        assert!(error.is(RuntimeCode::Panicked));
        assert!(error.to_string().contains("the disk is gone"), "{error}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_error_of_the_step_comes_back_as_it_was() {
        let error = blocking(|_cx| -> Result<(), Error> {
            Err(Error::new(RuntimeCode::Unbound, "nothing here"))
        })
        .await
        .unwrap_err();
        assert!(error.is(RuntimeCode::Unbound));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_step_whose_call_is_dropped_runs_to_its_end() {
        let (started, wait_started) = oneshot::channel::<()>();
        let (go, wait_go) = mpsc::channel::<()>();
        let (ended, wait_ended) = oneshot::channel::<()>();
        let call = tokio::spawn(blocking(move |_cx| {
            let _ = started.send(());
            let _ = wait_go.recv();
            let _ = ended.send(());
            Ok(())
        }));
        wait_started.await.unwrap();
        call.abort();
        let _ = call.await;
        go.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), wait_ended)
            .await
            .expect("the step ended")
            .unwrap();
    }
}
