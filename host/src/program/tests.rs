use super::*;

fn binding(set: u32, index: u32, name: &str, kind: BindingKind) -> Binding {
    Binding {
        name: name.to_owned(),
        set,
        index,
        kind,
    }
}

/// A small program's bindings: two sets, one of each kind the layout
/// branches on that a test can build without a device.
fn program() -> Vec<Binding> {
    vec![
        binding(
            0,
            0,
            "state",
            BindingKind::Storage {
                read_only: false,
                stride: Some(16),
            },
        ),
        binding(0, 1, "inputs", BindingKind::Uniform),
        binding(
            0,
            2,
            "dst",
            BindingKind::StorageTexture { write_only: true },
        ),
        binding(
            1,
            0,
            "table",
            BindingKind::Storage {
                read_only: true,
                stride: None,
            },
        ),
    ]
}

fn any_format(_: &str) -> wgpu::TextureFormat {
    wgpu::TextureFormat::Rgba8Unorm
}

/// The counts the device's limits and its log line are made from.
#[test]
fn the_counts_are_bindings_sets_and_storage_buffers() {
    assert_eq!(binding_counts(&program()), (4, 2, 2));
    assert_sets(&program(), &[0, 1]);
}

/// A set the host does not build a layout for stops it.
#[test]
#[should_panic(expected = "the caustic declares descriptor sets")]
fn a_set_nobody_fills_is_a_panic() {
    assert_sets(&program(), &[0]);
}

/// The layout keeps the reflection's numbers and asks the host for a storage
/// image's format by the image's name.
#[test]
fn a_layout_is_the_reflection_in_binding_order() {
    let asked = std::cell::RefCell::new(Vec::new());
    let entries = layout_entries(&program(), 0, &|name| {
        asked.borrow_mut().push(name.to_owned());
        wgpu::TextureFormat::Bgra8Unorm
    });
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.binding)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(*asked.borrow(), vec!["dst".to_owned()]);
    assert!(matches!(
        entries[2].ty,
        wgpu::BindingType::StorageTexture {
            format: wgpu::TextureFormat::Bgra8Unorm,
            access: wgpu::StorageTextureAccess::WriteOnly,
            ..
        }
    ));
    assert!(matches!(
        entries[0].ty,
        wgpu::BindingType::Buffer {
            min_binding_size: Some(size),
            ..
        } if size.get() == 16
    ));
}

/// A gap is named with its neighbour rather than renumbered past.
#[test]
#[should_panic(expected = "set 0 has no binding 1: `dst` comes after `state`")]
fn a_gap_in_a_set_is_a_panic_naming_its_neighbour() {
    let holed: Vec<Binding> = program()
        .into_iter()
        .filter(|binding| binding.name != "inputs")
        .collect();
    layout_entries(&holed, 0, &any_format);
}

/// The reload's witness names the one thing that moved.
#[test]
fn the_first_difference_names_one_binding() {
    let baked = program();
    assert_eq!(first_difference(&baked, &baked), None);

    let mut retyped = baked.clone();
    retyped[3].kind = BindingKind::Storage {
        read_only: false,
        stride: None,
    };
    let witness = first_difference(&baked, &retyped).expect("a retyped binding differs");
    assert!(
        witness.starts_with("set 1 binding 0 was `table`"),
        "{witness}"
    );

    let grown: Vec<Binding> = baked
        .iter()
        .cloned()
        .chain([binding(1, 1, "more", BindingKind::Sampler)])
        .collect();
    assert_eq!(
        first_difference(&baked, &grown).as_deref(),
        Some("`more` at set 1 binding 1 is new")
    );
    assert_eq!(
        first_difference(&grown, &baked).as_deref(),
        Some("`more` at set 1 binding 1 is gone")
    );
}
