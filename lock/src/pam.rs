//! The authentication: one PAM transaction per conversation, on a thread of
//! its own, asking through the conductor.
//!
//! PAM's conversation callback is synchronous: `pam_authenticate` calls it
//! and waits for what the user typed. So a transaction runs on a thread, and
//! its [`Conversation`] turns each message into an [`Step`] for the
//! conductor and, for an ask, blocks on the reply the conductor sends when
//! the host's `Answer` arrives. The conductor owns the wire; the thread owns
//! nothing but its PAM handle.
//!
//! [`Pam`] is what an authenticator is, so the conductor runs the same way
//! over [`LinuxPam`] (the real thing, `pam_start` with the configured
//! service) and [`DryRun`] (one ask and a comparison, for the tests and the
//! nested gate).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::mpsc;

use caustics_bar_abi::wire::Secret;
use pam_sys::{PamConversation, PamFlag, PamHandle, PamMessage, PamResponse, PamReturnCode};
use tokio::sync::mpsc::UnboundedSender;

use crate::conductor::Event;
use crate::witness;

/// What came back for an ask.
pub enum Reply {
    /// The typed bytes. Zeroed when dropped.
    Text(Secret),
    /// The user dismissed the prompt (Escape).
    Cancelled,
    /// The host or the conductor is gone: nobody will answer.
    Gone,
}

/// How a transaction ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// PAM said the user is who they say: unlock.
    Success,
    /// PAM said no, with its message for why (`pam_strerror`): the
    /// conversation's `PROMPT_ERROR`. A count is kept; the message is shown
    /// to the user and never witnessed with anything typed.
    Failed(String),
    /// An ask was dismissed, or nobody was left to answer it: no verdict on
    /// the user at all.
    Dismissed,
}

/// One thing a transaction's thread tells the conductor.
pub enum Step {
    /// Ask, echo off (a password) or on; the reply goes back on `reply`.
    Ask {
        echo: bool,
        message: String,
        reply: mpsc::Sender<Reply>,
    },
    /// Show a line: `PAM_ERROR_MSG` when `error`, else `PAM_TEXT_INFO`.
    Show { error: bool, message: String },
    /// The transaction is over.
    Ended(Verdict),
}

/// The thread's half of one conversation: sends [`Step`]s under its id and
/// waits for replies.
pub struct Conversation {
    id: u32,
    events: UnboundedSender<Event>,
}

impl Conversation {
    pub fn new(id: u32, events: UnboundedSender<Event>) -> Conversation {
        Conversation { id, events }
    }

    fn send(&self, step: Step) -> bool {
        self.events
            .send(Event::Attempt { id: self.id, step })
            .is_ok()
    }

    /// Ask, and wait for the reply. Blocks the calling thread, which is the
    /// transaction's own.
    pub fn ask(&mut self, echo: bool, message: &str) -> Reply {
        let (reply, answer) = mpsc::channel();
        if !self.send(Step::Ask {
            echo,
            message: message.to_owned(),
            reply,
        }) {
            return Reply::Gone;
        }
        answer.recv().unwrap_or(Reply::Gone)
    }

    /// Show a line, ask nothing.
    pub fn show(&mut self, error: bool, message: &str) {
        self.send(Step::Show {
            error,
            message: message.to_owned(),
        });
    }

    /// Say how it ended. The conversation is spent.
    pub fn end(self, verdict: Verdict) {
        self.send(Step::Ended(verdict));
    }
}

/// An authenticator: one transaction for `user`, asking through `conv`. Runs
/// on a thread of its own and may block for as long as the user takes.
pub trait Pam: Send + Sync + 'static {
    fn authenticate(&self, user: &str, conv: &mut Conversation) -> Verdict;
}

/// The dry run's message for a wrong secret: what `pam_strerror` says for
/// `PAM_AUTH_ERR`, so a gate that reads it reads what the real one shows.
pub const DRY_RUN_FAILURE: &str = "Authentication failure";

/// The dry run's one ask.
pub const DRY_RUN_ASK: &str = "Password: ";

/// `CAUSTICS_LOCK_DRY_RUN_SECRET`'s authenticator: one echo-off ask, and the
/// answer compared with the bytes. The comparison is not constant-time, and
/// need not be: the secret is in this process's environment, which is no
/// secret, and a dry run guards nothing.
pub struct DryRun {
    secret: Secret,
}

impl DryRun {
    pub fn new(secret: Secret) -> DryRun {
        DryRun { secret }
    }
}

impl Pam for DryRun {
    fn authenticate(&self, _user: &str, conv: &mut Conversation) -> Verdict {
        match conv.ask(false, DRY_RUN_ASK) {
            Reply::Text(typed) if typed.as_bytes() == self.secret.as_bytes() => Verdict::Success,
            Reply::Text(_) => Verdict::Failed(DRY_RUN_FAILURE.to_owned()),
            Reply::Cancelled | Reply::Gone => Verdict::Dismissed,
        }
    }
}

/// Linux-PAM, with `service`'s stack (`/etc/pam.d/<service>`).
///
/// A transaction is `pam_start` → `pam_authenticate` → on success
/// `pam_setcred(PAM_REFRESH_CRED)` (a Kerberos ticket, say, is renewed by the
/// unlock, as swaylock does it) → `pam_end`. There is no `pam_acct_mgmt`: the
/// session already exists, and an account check that fails (a password that
/// has just expired) must not keep its own user locked out of it. PAM's own
/// fail delay applies to a wrong answer; nothing here adds one.
pub struct LinuxPam {
    service: CString,
}

impl LinuxPam {
    /// `None` for a service name with a NUL in it.
    pub fn new(service: &str) -> Option<LinuxPam> {
        CString::new(service)
            .ok()
            .map(|service| LinuxPam { service })
    }
}

/// What the callback reaches through `appdata_ptr`.
struct Callback<'a> {
    conv: &'a mut Conversation,
    /// An ask was dismissed: the transaction's failure is a dismissal.
    dismissed: bool,
}

const SUCCESS: c_int = PamReturnCode::SUCCESS as c_int;
const CONV_ERR: c_int = PamReturnCode::CONV_ERR as c_int;
const BUF_ERR: c_int = PamReturnCode::BUF_ERR as c_int;

/// `PAM_PROMPT_ECHO_OFF`, `PAM_PROMPT_ECHO_ON`, `PAM_ERROR_MSG`,
/// `PAM_TEXT_INFO`, as Linux-PAM's `_pam_types.h` numbers them.
const PROMPT_ECHO_OFF: c_int = 1;
const PROMPT_ECHO_ON: c_int = 2;
const ERROR_MSG: c_int = 3;
const TEXT_INFO: c_int = 4;

/// `PAM_MAX_NUM_MSG`: the most messages Linux-PAM passes in one call.
const MAX_NUM_MSG: c_int = 32;

/// `pam_strerror(handle, code)`, or the number when PAM has no words for it.
///
/// # Safety
///
/// `handle` is a live handle from `pam_start`, or null.
unsafe fn strerror(handle: *mut PamHandle, code: c_int) -> String {
    if handle.is_null() {
        return format!("PAM error {code}");
    }
    // SAFETY: the caller's; `pam_strerror` returns a static string or null.
    let text = unsafe { pam_sys::raw::pam_strerror(handle, code) };
    if text.is_null() {
        return format!("PAM error {code}");
    }
    // SAFETY: a NUL-terminated string PAM owns, read before the next call.
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

impl Pam for LinuxPam {
    fn authenticate(&self, user: &str, conv: &mut Conversation) -> Verdict {
        let Ok(user) = CString::new(user) else {
            return Verdict::Failed("a user name with a NUL in it".to_owned());
        };
        let mut callback = Callback {
            conv,
            dismissed: false,
        };
        let pam_conv = PamConversation {
            conv: Some(converse),
            data_ptr: (&mut callback as *mut Callback<'_>).cast::<c_void>(),
        };
        let mut handle: *const PamHandle = std::ptr::null();
        // SAFETY: the strings are NUL-terminated and live for the call;
        // `pam_conv` and the `callback` it points at outlive the handle,
        // which `pam_end` below ends before either goes.
        let rc = unsafe {
            pam_sys::raw::pam_start(self.service.as_ptr(), user.as_ptr(), &pam_conv, &mut handle)
        };
        let handle = handle as *mut PamHandle;
        if rc != SUCCESS {
            // SAFETY: `handle` is null or what `pam_start` made.
            let why = unsafe { strerror(handle, rc) };
            if !handle.is_null() {
                // SAFETY: as above, ended once.
                unsafe { pam_sys::raw::pam_end(handle, rc) };
            }
            return Verdict::Failed(why);
        }
        // SAFETY: a live handle; the callback runs on this thread, inside
        // this call.
        let rc = unsafe { pam_sys::raw::pam_authenticate(handle, 0) };
        let verdict = if rc == SUCCESS {
            // SAFETY: as above.
            let cred = unsafe { pam_sys::raw::pam_setcred(handle, PamFlag::REFRESH_CRED as c_int) };
            if cred != SUCCESS {
                // SAFETY: as above.
                let why = unsafe { strerror(handle, cred) };
                witness(&format!(
                    "refreshing the credentials after the unlock: {why}"
                ));
            }
            Verdict::Success
        } else if callback.dismissed {
            Verdict::Dismissed
        } else {
            // SAFETY: as above.
            Verdict::Failed(unsafe { strerror(handle, rc) })
        };
        // SAFETY: the handle `pam_start` made, ended once.
        unsafe { pam_sys::raw::pam_end(handle, rc) };
        verdict
    }
}

/// PAM's conversation function: each message to the conductor, each ask's
/// reply into a response PAM frees. Never unwinds into C.
extern "C" fn converse(
    num_msg: c_int,
    msg: *mut *mut PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: PAM's contract for a conversation function, checked as far
        // as it can be in `converse_with`.
        unsafe { converse_with(num_msg, msg, resp, appdata) }
    }))
    .unwrap_or(CONV_ERR)
}

/// # Safety
///
/// Linux-PAM's convention: `msg` is an array of `num_msg` pointers to
/// messages, `resp` a place for an array of as many responses allocated with
/// `malloc`, which PAM frees, and `appdata` the [`Callback`] that
/// [`LinuxPam::authenticate`] gave `pam_start`.
unsafe fn converse_with(
    num_msg: c_int,
    msg: *mut *mut PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if num_msg <= 0 || num_msg > MAX_NUM_MSG || msg.is_null() || resp.is_null() || appdata.is_null()
    {
        return CONV_ERR;
    }
    // SAFETY: the caller's: `appdata` is the live `Callback`, used by this
    // thread alone.
    let callback = unsafe { &mut *appdata.cast::<Callback<'_>>() };
    let count = num_msg as usize;
    // SAFETY: a fresh zeroed array of `count` responses, every `resp` null.
    let replies = unsafe { libc::calloc(count, size_of::<PamResponse>()) }.cast::<PamResponse>();
    if replies.is_null() {
        return BUF_ERR;
    }
    for i in 0..count {
        // SAFETY: the caller's: `count` message pointers.
        let message = unsafe { *msg.add(i) };
        if message.is_null() {
            // SAFETY: `replies` and the `i` responses filled so far.
            unsafe { refuse(replies, count) };
            return CONV_ERR;
        }
        // SAFETY: a live message; its text NUL-terminated or null.
        let (style, text) = unsafe {
            let text = if (*message).msg.is_null() {
                String::new()
            } else {
                CStr::from_ptr((*message).msg)
                    .to_string_lossy()
                    .into_owned()
            };
            ((*message).msg_style, text)
        };
        match style {
            PROMPT_ECHO_OFF | PROMPT_ECHO_ON => {
                match callback.conv.ask(style == PROMPT_ECHO_ON, &text) {
                    Reply::Text(typed) => {
                        // SAFETY: `typed`'s bytes; the copy is PAM's to free.
                        let copy = unsafe { c_copy(typed.as_bytes()) };
                        drop(typed);
                        if copy.is_null() {
                            // SAFETY: as above.
                            unsafe { refuse(replies, count) };
                            return BUF_ERR;
                        }
                        // SAFETY: response `i` of `count`.
                        unsafe { (*replies.add(i)).resp = copy };
                    }
                    Reply::Cancelled => {
                        callback.dismissed = true;
                        // SAFETY: as above.
                        unsafe { refuse(replies, count) };
                        return CONV_ERR;
                    }
                    Reply::Gone => {
                        // SAFETY: as above.
                        unsafe { refuse(replies, count) };
                        return CONV_ERR;
                    }
                }
            }
            ERROR_MSG => callback.conv.show(true, &text),
            TEXT_INFO => callback.conv.show(false, &text),
            _ => {
                // SAFETY: as above.
                unsafe { refuse(replies, count) };
                return CONV_ERR;
            }
        }
    }
    // SAFETY: the caller's place for the array, now PAM's.
    unsafe { *resp = replies };
    SUCCESS
}

/// `bytes` as a `malloc`ed C string, or null when there is no memory, or
/// when the bytes hold a NUL and so cannot be one: PAM would read a prefix of
/// what was typed and call it the answer.
///
/// # Safety
///
/// None beyond the allocator's; the result is the caller's to hand to PAM or
/// to zero and free.
unsafe fn c_copy(bytes: &[u8]) -> *mut c_char {
    if bytes.contains(&0) {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh block of `len + 1` bytes, written whole below.
    let copy = unsafe { libc::malloc(bytes.len() + 1) }.cast::<u8>();
    if copy.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `copy` holds `len + 1` bytes and overlaps nothing.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len());
        *copy.add(bytes.len()) = 0;
    }
    copy.cast::<c_char>()
}

/// A conversation refused partway: every response made so far is zeroed and
/// freed, and the array with it. PAM gets nothing.
///
/// # Safety
///
/// `replies` is the `calloc`ed array of `count` responses, each `resp` null
/// or a [`c_copy`].
unsafe fn refuse(replies: *mut PamResponse, count: usize) {
    for i in 0..count {
        // SAFETY: the caller's.
        let resp = unsafe { (*replies.add(i)).resp };
        if resp.is_null() {
            continue;
        }
        // SAFETY: a NUL-terminated `c_copy`, zeroed in place, then freed.
        unsafe {
            let len = libc::strlen(resp);
            zeroize::Zeroize::zeroize(std::slice::from_raw_parts_mut(resp.cast::<u8>(), len));
            libc::free(resp.cast::<c_void>());
        }
    }
    // SAFETY: the caller's `calloc`ed array.
    unsafe { libc::free(replies.cast::<c_void>()) };
}

#[cfg(test)]
mod tests;
