//! The texels behind the `Pixmap` keys a sidecar mints (docs/abi.md,
//! *Pixmap*): what a frame owes the bar, and what a `REQ_ICON` for one of them
//! is answered from.
//!
//! Held: every key the last frame names, and every key a frame stopped naming
//! within the last [`HOLD`]. The window is what keeps an icon that alternates
//! between a few pictures (a tray item's sync animation, twice a second) from
//! being sent again at every change, and a request inside it still answered.
//! It is a few seconds, longer than the bar holds an undrawn slot (a second
//! and a half, `IMAGE_LINGER_S` in the bar's tunables), so the bar lets a
//! picture go before this side does. Swept on each frame, so nothing needs a timer, and
//! bounded by what a few seconds of frames name, never by what the process
//! has ever sent. The clock is passed in, so a test advances it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a key no frame names any more is still held.
pub const HOLD: Duration = Duration::from_secs(5);

/// One sidecar's minted keys and their texels.
#[derive(Debug, Default)]
pub struct Minted {
    /// Named by the last frame.
    live: HashMap<String, Vec<u8>>,
    /// Named by an earlier frame and not the last, with when they stopped.
    dropped: HashMap<String, (Vec<u8>, Instant)>,
}

impl Minted {
    /// A frame naming `pixmaps`, built at `now`: the keys it owes the bar,
    /// in order — the ones held neither live nor in the window — which the
    /// caller sends, from [`Self::texels`], **before** the frame. First
    /// whatever has been in the window longer than [`HOLD`] is let go; then
    /// every key of `pixmaps` is live, and the last frame's others go into
    /// the window.
    pub fn owed(&mut self, pixmaps: Vec<(String, Vec<u8>)>, now: Instant) -> Vec<String> {
        self.dropped
            .retain(|_, (_, since)| now.saturating_duration_since(*since) < HOLD);
        let mut was = std::mem::take(&mut self.live);
        let mut owed = Vec::new();
        for (key, rgba) in pixmaps {
            if self.live.contains_key(&key) {
                continue;
            }
            if let Some(held) = was.remove(&key) {
                self.live.insert(key, held);
            } else if let Some((held, _)) = self.dropped.remove(&key) {
                self.live.insert(key, held);
            } else {
                owed.push(key.clone());
                self.live.insert(key, rgba);
            }
        }
        for (key, rgba) in was {
            self.dropped.insert(key, (rgba, now));
        }
        owed
    }

    /// The texels of a held key: live, or in the window. What a `REQ_ICON`
    /// naming it is answered with; `None` (silence) for any other name.
    pub fn texels(&self, key: &str) -> Option<&[u8]> {
        self.live
            .get(key)
            .or_else(|| self.dropped.get(key).map(|(rgba, _)| rgba))
            .map(Vec::as_slice)
    }

    /// Keys held, live and in the window.
    pub fn held(&self) -> usize {
        self.live.len() + self.dropped.len()
    }
}
