use super::*;

#[test]
fn a_witness_is_one_prefixed_line() {
    const WITNESS: Witness = Witness::new("caustics-demo");
    assert_eq!(WITNESS.name(), "caustics-demo");
    assert_eq!(
        WITNESS.render("started; 3 things"),
        b"caustics-demo: started; 3 things\n"
    );
    assert_eq!(WITNESS.render(""), b"caustics-demo: \n");
}

#[cfg(feature = "drain")]
mod drain {
    use std::sync::Arc;
    use std::time::Duration;

    use futures_lite::stream;
    use tokio::sync::Notify;
    use tokio::sync::mpsc::unbounded_channel;

    use crate::{drain, forward};

    /// More than the 64 zbus queues per stream, by a margin.
    const BURST: u32 = 200;

    /// The drain on its own: a
    /// stream fed 200 messages while its consumer is busy elsewhere is read to
    /// the end at once, and the consumer finds **one** notification waiting,
    /// not two hundred.
    #[tokio::test]
    async fn a_drained_stream_is_one_coalesced_notification() {
        let notify = Arc::new(Notify::new());
        let _drain = drain(stream::iter(0..BURST), notify.clone());

        // The consumer is busy; the drain is not.
        tokio::time::sleep(Duration::from_millis(50)).await;

        tokio::time::timeout(Duration::from_millis(100), notify.notified())
            .await
            .expect("one notification is waiting");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), notify.notified())
                .await
                .is_err(),
            "and only one: the burst coalesced"
        );
    }

    /// A drained stream that ends resolves `ended`, so a loop can still say so.
    #[tokio::test]
    async fn a_drained_stream_that_ends_says_so() {
        let notify = Arc::new(Notify::new());
        let mut drain = drain(stream::iter(0..3u32), notify);
        tokio::time::timeout(Duration::from_millis(100), drain.ended())
            .await
            .expect("the stream ended and the drain with it");
    }

    /// `forward` reads the whole burst while the consumer sleeps, translates
    /// every item, and closes the channel when the stream ends.
    #[tokio::test]
    async fn a_forwarded_stream_is_read_to_the_end_and_closes() {
        let (sink, mut values) = unbounded_channel();
        let _drain = forward(stream::iter(0..BURST), sink, |n| (n % 2 == 0).then_some(n));

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut seen = Vec::new();
        while let Some(n) = values.recv().await {
            seen.push(n);
        }
        assert_eq!(seen, (0..BURST).filter(|n| n % 2 == 0).collect::<Vec<_>>());
    }

    /// Dropping the guard stops the task: a stream that never ends is not read
    /// by anybody afterwards.
    #[tokio::test]
    async fn dropping_the_guard_stops_the_task() {
        let (sink, mut values) = unbounded_channel::<u32>();
        let drain = forward(stream::pending::<u32>(), sink, Some);
        drop(drain);
        let got = tokio::time::timeout(Duration::from_millis(100), values.recv())
            .await
            .expect("the aborted task dropped its sender");
        assert_eq!(got, None, "nothing was sent");
    }
}

mod inbox {
    use std::time::{Duration, Instant};

    use crate::inbox::{Inbox, Next};

    /// Items come through in order, a quiet reader is a tick at the period,
    /// and a reader that returns `None` is `Closed`.
    #[test]
    fn items_then_ticks_then_closed() {
        let (tx, rx) = std::sync::mpsc::channel::<Option<u32>>();
        let mut inbox = Inbox::spawn(Duration::from_millis(50), move || rx.recv().ok().flatten());
        tx.send(Some(1)).unwrap();
        tx.send(Some(2)).unwrap();
        assert_eq!(inbox.next(), Next::Item(1));
        assert_eq!(inbox.next(), Next::Item(2));
        let quiet = Instant::now();
        assert_eq!(inbox.next(), Next::Tick);
        assert!(
            quiet.elapsed() >= Duration::from_millis(30),
            "{:?}",
            quiet.elapsed()
        );
        tx.send(Some(3)).unwrap();
        assert_eq!(inbox.next(), Next::Item(3));
        tx.send(None).unwrap();
        // The reader ending is `Closed`, whatever the clock says.
        let mut saw = Vec::new();
        loop {
            match inbox.next() {
                Next::Closed => break,
                other => saw.push(other),
            }
        }
        assert!(saw.iter().all(|next| *next == Next::Tick), "{saw:?}");
    }

    /// What has arrived is taken whole and in order without a wait; nothing
    /// waiting is an empty batch at once, not a tick; a reader that ended
    /// leaves its last items to `waiting` and is `Closed` after them.
    #[test]
    fn waiting_takes_what_arrived_and_never_blocks() {
        let (tx, rx) = std::sync::mpsc::channel::<Option<u32>>();
        let mut inbox = Inbox::spawn(Duration::from_secs(3600), move || rx.recv().ok().flatten());
        let empty = Instant::now();
        assert!(inbox.waiting().is_empty());
        assert!(empty.elapsed() < Duration::from_secs(1), "it did not wait");

        for n in 1..=4 {
            tx.send(Some(n)).unwrap();
        }
        tx.send(None).unwrap();
        assert_eq!(inbox.next(), Next::Item(1));
        // The reader thread forwards as it goes; gather until it has, with a
        // deadline that only a wedged thread reaches.
        let mut batch = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while batch.len() < 3 && Instant::now() < deadline {
            batch.extend(inbox.waiting());
            std::thread::yield_now();
        }
        assert_eq!(batch, vec![2, 3, 4], "in order, all of them");
        assert_eq!(inbox.next(), Next::Closed);
        assert!(inbox.waiting().is_empty());
    }
}
