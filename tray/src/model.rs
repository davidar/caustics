//! What the bus says, before it is a frame.
//!
//! An item's address, its icon, its status, and the dbusmenu tree behind it.
//! They live here, not in the bar, because they describe **the session bus**:
//! the bar's own model knows only rows.
//!
//! Everything here is plain data with no zbus type in it, which is what lets
//! `tray/tests/` build a tree by hand and what keeps [`crate::rows`] a pure
//! function of state.

/// A tray item's address: the bus name that owns it and the object path it
/// lives at. The watcher hands out three spellings of the same thing —
/// `:1.137/StatusNotifierItem`, a bare `:1.1280`, and an ayatana path — and
/// [`TrayAddress::parse`] normalises all three.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrayAddress {
    pub bus_name: String,
    pub path: String,
}

impl TrayAddress {
    /// Parse a watcher-supplied address in any of the three forms.
    pub fn parse(s: &str) -> Self {
        match s.find('/') {
            Some(i) => Self {
                bus_name: s[..i].to_owned(),
                path: s[i..].to_owned(),
            },
            None => Self {
                bus_name: s.to_owned(),
                path: "/StatusNotifierItem".to_owned(),
            },
        }
    }

    /// The key this sidecar holds items under: `bus_name` and `path` joined
    /// back together, so that parsing the key returns the same address.
    pub fn key(&self) -> String {
        format!("{}{}", self.bus_name, self.path)
    }
}

/// SNI `Status`. `Passive` means "I exist but have nothing to say", and such
/// an item is left out of the frame entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrayStatus {
    #[default]
    Passive,
    Active,
    NeedsAttention,
}

/// An icon as the bus describes it. `Themed` becomes a name in the frame's
/// blob (the bar resolves it through the icon theme); `Pixmap` becomes a key
/// in the blob and a `Pixmap` frame (docs/abi.md, *Pixmap*). Pixmap bytes are **straight (non-premultiplied) RGBA8**,
/// row-major, top-down — the SNI wire format is ARGB32 in network byte order
/// and [`crate::item::convert_pixmap`] converts.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Icon {
    #[default]
    None,
    /// `theme_path` is the item's own icon directory, set only on a name
    /// **nothing had** — no theme, no own directory, no pixels beside it
    /// (step 4 of [`crate::icon`]'s chain). The directory does not survive
    /// the wire (docs/abi.md, *Rows* carries a name and nothing else); it is
    /// kept here so the witness can say where the name was looked for.
    Themed {
        name: String,
        theme_path: Option<String>,
    },
    Pixmap {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

/// One StatusNotifierItem, as its task last read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayItem {
    pub address: TrayAddress,
    pub title: String,
    pub icon: Icon,
    pub status: TrayStatus,
    /// The item exports a usable `Menu` object path. The layout arrives
    /// separately, so this can be `true` while `menu` is still `None`.
    pub has_menu: bool,
}

/// One item's dbusmenu, as `GetLayout(0, -1, …)` last answered.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Menu {
    pub revision: u32,
    /// The root's children, in order, submenus nested.
    pub items: Vec<MenuItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Toggle {
    #[default]
    None,
    Checkmark(bool),
    Radio(bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MenuItem {
    /// com.canonical.dbusmenu item id; what `Event(id, "clicked", …)` takes.
    pub id: i32,
    /// With the `_` mnemonic marker stripped.
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub separator: bool,
    pub toggle: Toggle,
    pub icon: Icon,
    /// A row with children is a submenu; the tree is nested, not flat.
    pub children: Vec<MenuItem>,
}
