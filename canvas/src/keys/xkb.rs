//! The seat's keymap through libxkbcommon, opened at run time
//! (`xkbcommon-dl`): the keymap the compositor sends, the modifiers it
//! sends, compose from the locale, and one [`Key`] per press.
//!
//! Opened rather than linked so that a build needs no development package
//! and a machine without the library still runs: there the host has no
//! keymap, reads Return, Escape and BackSpace by their codes, and types
//! nothing (one line says so).
//!
//! Every text buffer this passes a key's characters through is on the stack
//! and zeroed before it goes; the characters leave as a [`Typed`], which
//! zeroes itself.

use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::raw::c_char;

use xkbcommon_dl::{
    XkbCommon, XkbCommonCompose, xkb_compose_compile_flags, xkb_compose_state,
    xkb_compose_state_flags, xkb_compose_status, xkb_compose_table, xkb_context, xkb_context_flags,
    xkb_keymap, xkb_keymap_compile_flags, xkb_keymap_format, xkb_state, xkb_state_component,
};
use zeroize::Zeroize;

use super::{Key, key_of};

/// xkbcommon's keycodes are the kernel's plus eight.
const KEYCODE_OFFSET: u32 = 8;

/// A compiled keymap, its state and compose, or nothing yet.
pub struct Xkb {
    lib: &'static XkbCommon,
    compose_lib: Option<&'static XkbCommonCompose>,
    context: *mut xkb_context,
    keymap: *mut xkb_keymap,
    state: *mut xkb_state,
    compose_table: *mut xkb_compose_table,
    compose: *mut xkb_compose_state,
}

impl Xkb {
    /// The library and a context, or `None` when libxkbcommon cannot be
    /// opened.
    pub fn new() -> Option<Xkb> {
        let lib = xkbcommon_dl::xkbcommon_option()?;
        // SAFETY: a plain constructor; a null return is checked.
        let context = unsafe { (lib.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_FLAGS) };
        if context.is_null() {
            return None;
        }
        let compose_lib = xkbcommon_dl::xkbcommon_compose_option();
        let mut xkb = Xkb {
            lib,
            compose_lib,
            context,
            keymap: std::ptr::null_mut(),
            state: std::ptr::null_mut(),
            compose_table: std::ptr::null_mut(),
            compose: std::ptr::null_mut(),
        };
        xkb.open_compose();
        Some(xkb)
    }

    /// Compose from the locale (`LC_ALL`, `LC_CTYPE`, `LANG`, else `C`), as
    /// every toolkit reads it. A locale with no compose table composes
    /// nothing, which is not an error.
    fn open_compose(&mut self) {
        let Some(compose_lib) = self.compose_lib else {
            return;
        };
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "C".to_owned());
        let Ok(locale) = CString::new(locale) else {
            return;
        };
        // SAFETY: `context` is live; `locale` outlives the call; nulls are
        // checked.
        unsafe {
            let table = (compose_lib.xkb_compose_table_new_from_locale)(
                self.context,
                locale.as_ptr(),
                xkb_compose_compile_flags::XKB_COMPOSE_COMPILE_NO_FLAGS,
            );
            if table.is_null() {
                return;
            }
            let state = (compose_lib.xkb_compose_state_new)(
                table,
                xkb_compose_state_flags::XKB_COMPOSE_STATE_NO_FLAGS,
            );
            if state.is_null() {
                (compose_lib.xkb_compose_table_unref)(table);
                return;
            }
            self.compose_table = table;
            self.compose = state;
        }
    }

    /// The keymap the compositor sent (`wl_keyboard.keymap`,
    /// `xkb_v1`): `size` bytes of text in `fd`, its NUL included.
    pub fn set_keymap(&mut self, fd: OwnedFd, size: u32) -> Result<(), String> {
        let size = size as usize;
        if size == 0 {
            return Err("an empty keymap".to_owned());
        }
        // SAFETY: a private read-only map of the fd the compositor gave, of
        // the size it stated; unmapped below whatever happens.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                fd.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(format!("mmap: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: `map` is `size` readable bytes; the text ends at its NUL,
        // which the buffer form takes as the length less one.
        let keymap = unsafe {
            let bytes = std::slice::from_raw_parts(map.cast::<u8>(), size);
            let len = bytes.iter().position(|byte| *byte == 0).unwrap_or(size);
            let keymap = (self.lib.xkb_keymap_new_from_buffer)(
                self.context,
                map.cast::<c_char>(),
                len,
                xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            libc::munmap(map, size);
            keymap
        };
        if keymap.is_null() {
            return Err("libxkbcommon could not compile it".to_owned());
        }
        // SAFETY: `keymap` is live; the old keymap and state are released
        // once the new ones exist.
        unsafe {
            let state = (self.lib.xkb_state_new)(keymap);
            if state.is_null() {
                (self.lib.xkb_keymap_unref)(keymap);
                return Err("no state for it".to_owned());
            }
            self.release_keymap();
            self.keymap = keymap;
            self.state = state;
        }
        Ok(())
    }

    fn release_keymap(&mut self) {
        // SAFETY: each pointer is either null or one this owns.
        unsafe {
            if !self.state.is_null() {
                (self.lib.xkb_state_unref)(self.state);
            }
            if !self.keymap.is_null() {
                (self.lib.xkb_keymap_unref)(self.keymap);
            }
        }
        self.state = std::ptr::null_mut();
        self.keymap = std::ptr::null_mut();
    }

    /// `wl_keyboard.modifiers`, as sent.
    pub fn modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        if self.state.is_null() {
            return;
        }
        // SAFETY: `state` is live.
        unsafe {
            (self.lib.xkb_state_update_mask)(self.state, depressed, latched, locked, 0, 0, group);
        }
    }

    /// Whether a keymap has been compiled.
    pub fn ready(&self) -> bool {
        !self.state.is_null()
    }

    /// A press of the kernel key `code`, through the keymap and compose:
    /// what it means to a prompt, if anything. A press mid-compose means
    /// nothing until the sequence completes.
    pub fn press(&mut self, code: u32) -> Option<Key> {
        if self.state.is_null() {
            return None;
        }
        let keycode = code + KEYCODE_OFFSET;
        // SAFETY: `state` is live.
        let mut keysym = unsafe { (self.lib.xkb_state_key_get_one_sym)(self.state, keycode) };
        let ctrl = unsafe {
            (self.lib.xkb_state_mod_name_is_active)(
                self.state,
                xkbcommon_dl::XKB_MOD_NAME_CTRL.as_ptr().cast(),
                xkb_state_component::XKB_STATE_MODS_EFFECTIVE,
            ) > 0
        };
        let mut text = [0u8; 64];
        let mut len = 0usize;
        let mut composed = false;
        if let (Some(compose_lib), false) = (self.compose_lib, self.compose.is_null()) {
            // SAFETY: `compose` is live; `text` is 64 writable bytes.
            unsafe {
                (compose_lib.xkb_compose_state_feed)(self.compose, keysym);
                match (compose_lib.xkb_compose_state_get_status)(self.compose) {
                    xkb_compose_status::XKB_COMPOSE_COMPOSING => return None,
                    xkb_compose_status::XKB_COMPOSE_CANCELLED => {
                        (compose_lib.xkb_compose_state_reset)(self.compose);
                        return None;
                    }
                    xkb_compose_status::XKB_COMPOSE_COMPOSED => {
                        let n = (compose_lib.xkb_compose_state_get_utf8)(
                            self.compose,
                            text.as_mut_ptr().cast(),
                            text.len(),
                        );
                        len = (n.max(0) as usize).min(text.len() - 1);
                        keysym = (compose_lib.xkb_compose_state_get_one_sym)(self.compose);
                        (compose_lib.xkb_compose_state_reset)(self.compose);
                        composed = true;
                    }
                    xkb_compose_status::XKB_COMPOSE_NOTHING => {}
                }
            }
        }
        if !composed {
            // SAFETY: `state` is live; `text` is 64 writable bytes.
            let n = unsafe {
                (self.lib.xkb_state_key_get_utf8)(
                    self.state,
                    keycode,
                    text.as_mut_ptr().cast(),
                    text.len(),
                )
            };
            len = (n.max(0) as usize).min(text.len() - 1);
        }
        let key = key_of(keysym, &text[..len], ctrl);
        text.zeroize();
        key
    }
}

impl Drop for Xkb {
    fn drop(&mut self) {
        self.release_keymap();
        // SAFETY: each pointer is either null or one this owns.
        unsafe {
            if let Some(compose_lib) = self.compose_lib {
                if !self.compose.is_null() {
                    (compose_lib.xkb_compose_state_unref)(self.compose);
                }
                if !self.compose_table.is_null() {
                    (compose_lib.xkb_compose_table_unref)(self.compose_table);
                }
            }
            (self.lib.xkb_context_unref)(self.context);
        }
    }
}

#[cfg(test)]
impl Xkb {
    /// A US keymap compiled from the system's rules, as text: what a
    /// compositor would send. `None` without the rules on this machine.
    pub fn us_keymap_text(&self) -> Option<String> {
        let layout = CString::new("us").ok()?;
        let names = xkbcommon_dl::xkb_rule_names {
            rules: std::ptr::null(),
            model: std::ptr::null(),
            layout: layout.as_ptr(),
            variant: std::ptr::null(),
            options: std::ptr::null(),
        };
        // SAFETY: `context` is live, `names` and its strings outlive the
        // call, and every pointer is checked and released.
        unsafe {
            let keymap = (self.lib.xkb_keymap_new_from_names)(
                self.context,
                &names,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            if keymap.is_null() {
                return None;
            }
            let text = (self.lib.xkb_keymap_get_as_string)(
                keymap,
                xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
            );
            let owned = (!text.is_null()).then(|| {
                std::ffi::CStr::from_ptr(text)
                    .to_string_lossy()
                    .into_owned()
            });
            if !text.is_null() {
                libc::free(text.cast_mut().cast());
            }
            (self.lib.xkb_keymap_unref)(keymap);
            owned
        }
    }

    /// The mask of the modifier named `name` (`Control`, `Shift`).
    pub fn mod_mask(&self, name: &str) -> u32 {
        let Ok(name) = CString::new(name) else {
            return 0;
        };
        // SAFETY: `keymap` is live when `ready`.
        let index = unsafe { (self.lib.xkb_keymap_mod_get_index)(self.keymap, name.as_ptr()) };
        if index == xkbcommon_dl::XKB_MOD_INVALID {
            0
        } else {
            1 << index
        }
    }
}
