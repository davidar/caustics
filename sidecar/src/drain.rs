//! The drain: one task per zbus signal stream, so no stream ever stalls the
//! connection (docs/architecture.md, *Rules*).

use std::sync::Arc;

use futures_lite::StreamExt as _;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

/// A task that reads one signal stream to its end and does nothing else,
/// folding every message into `notify`.
///
/// zbus's socket reader awaits room in **every** stream's channel before it
/// reads the next message (capacity 64, overflow off), so a stream nobody is
/// polling blocks the whole connection — method replies included. This task
/// awaits nothing but its stream, so it is always ready and no channel can
/// fill; `notify_one` holds at most one permit, so a burst of two hundred
/// signals is one re-read for whoever awaits the `Notify`.
///
/// Aborted when the returned guard drops, which is when the thing the stream
/// belongs to — an item task, a menu — is gone.
pub fn drain<S>(stream: S, notify: Arc<Notify>) -> Drain
where
    S: futures_lite::Stream + Send + Unpin + 'static,
    S::Item: Send,
{
    let mut stream = stream;
    Drain(tokio::spawn(async move {
        while stream.next().await.is_some() {
            notify.notify_one();
        }
    }))
}

/// [`drain`] for a stream whose **payload** is used rather than only its
/// arrival: every item goes through `translate`, and what comes out is sent on
/// an unbounded channel. The task still awaits nothing but its stream — an
/// unbounded send never waits — so the rule holds; a receiver that has gone
/// away costs a dropped value and nothing else.
///
/// The stream ending ends the task, which drops `sink`: a receiver that has
/// no other sender sees its channel close, and that is how the end of the
/// stream reaches whoever was reading it.
pub fn forward<S, T, F>(stream: S, sink: UnboundedSender<T>, translate: F) -> Drain
where
    S: futures_lite::Stream + Send + Unpin + 'static,
    S::Item: Send,
    T: Send + 'static,
    F: Fn(S::Item) -> Option<T> + Send + 'static,
{
    let mut stream = stream;
    Drain(tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            if let Some(value) = translate(item) {
                let _ = sink.send(value);
            }
        }
    }))
}

/// The guard [`drain`] returns: the task stops with it.
pub struct Drain(JoinHandle<()>);

impl Drain {
    /// Resolves when the stream has ended (or the task was aborted). A
    /// [`drain`]ed stream that ends notifies nobody, so a loop that must say
    /// "the signal stream ended" selects on this beside the `Notify`.
    ///
    /// Await it once: a finished `JoinHandle` is not to be polled again.
    pub async fn ended(&mut self) {
        let _ = (&mut self.0).await;
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        self.0.abort();
    }
}
