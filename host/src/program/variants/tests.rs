use super::*;

static SURFACES: [Variant; 2] = [
    Variant {
        value: 3,
        name: "lock",
    },
    Variant {
        value: 0,
        name: "background",
    },
];

/// With nothing specialised, slot `n` is entry `n`.
#[test]
fn nothing_specialised_is_the_entry_list() {
    let slots = Slots::new(&["a", "b", "c"], &[]).unwrap();
    assert_eq!(slots.len(), 3);
    for n in 0..3 {
        assert_eq!(slots.entry(n), n);
        assert_eq!(slots.find(n, None), Some(n));
    }
    assert_eq!(slots.order(&[2, 0, 1]), [2, 0, 1]);
    assert_eq!(slots.find(1, Some(0)), None);
}

/// A specialised entry is one slot per value, named by it, built in the
/// order the values are listed, and never found as built once.
#[test]
fn a_specialised_entry_is_a_slot_per_value() {
    let specialised = [Specialised {
        entry: 1,
        id: 0,
        values: &SURFACES,
    }];
    let slots = Slots::new(&["shape", "draw", "apply"], &specialised).unwrap();
    assert_eq!(slots.len(), 4);
    assert_eq!(slots.name(1), "draw[lock]");
    assert_eq!(slots.name(2), "draw[background]");
    assert_eq!(slots.find(1, Some(0)), Some(2));
    assert_eq!(slots.find(1, None), None);
    assert_eq!(slots.find(2, None), Some(3));
    assert_eq!(slots.order(&[2, 1, 0]), [3, 1, 2, 0]);
}

/// A host's mistakes in the list are refused.
#[test]
fn a_bad_list_is_refused() {
    let twice = [
        Specialised {
            entry: 0,
            id: 0,
            values: &SURFACES,
        },
        Specialised {
            entry: 0,
            id: 1,
            values: &SURFACES,
        },
    ];
    assert!(Slots::new(&["a"], &twice).is_err());
    let past = [Specialised {
        entry: 4,
        id: 0,
        values: &SURFACES,
    }];
    assert!(Slots::new(&["a"], &past).is_err());
    let empty = [Specialised {
        entry: 0,
        id: 0,
        values: &[],
    }];
    assert!(Slots::new(&["a"], &empty).is_err());
}
