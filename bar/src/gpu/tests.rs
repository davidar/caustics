use super::*;

/// The bar's three driver-visible totals, read out of the reflection baked
/// beside the SPIR-V and pinned here — so a binding added to a caustic has to
/// be acknowledged somewhere a reviewer reads, with a number to report.
///
/// Density and binding order are not tested because they cannot be got wrong
/// at this layer: `layout_entries` panics at startup on a gap, naming the set,
/// the missing index and the neighbours.
#[test]
fn the_binding_count_is_what_the_report_says() {
    let bindings = baked_bindings();
    let (total, groups, storage) = binding_counts(&bindings);
    // Set 2 is one arena of verbatim OpenType bytes rather than a binding per
    // flattened shape table; set 0 carries the rows arena at binding 14 and
    // the answers arena at 15.
    assert_eq!(total, 23, "bindings across the groups");
    assert_eq!(groups, 3, "descriptor sets");
    assert_eq!(
        storage, 15,
        "and how many of the bindings are storage buffers"
    );
    // The sets are the three the pipeline layout binds, in the order it binds
    // them. `Gpu::new` asserts this too, at startup, on the real reflection.
    assert_sets(&bindings);
}

/// The reflection and the layout agree about the shapes the bar depends on:
/// the two storage images at 5 and 6, the array texture at 7, the sampler at
/// 8, and the two read-write buffers at the front of group 0.
///
/// Not a restatement of the reflection — these are the five classifications
/// `layout_entry` branches on, and the only ones whose being wrong is silent
/// rather than a validation error.
#[test]
fn the_kinds_the_layout_branches_on_are_the_ones_reflected() {
    let bindings = baked_bindings();
    let at = |set: u32, index: u32| {
        bindings
            .iter()
            .find(|binding| binding.set == set && binding.index == index)
            .unwrap_or_else(|| panic!("set {set} binding {index} is not reflected"))
    };

    // The state buffer is the rings, and the caustic writes them.
    assert_eq!(at(SET_BAR, 0).name, "state");
    assert!(matches!(
        at(SET_BAR, 0).kind,
        BindingKind::Storage {
            read_only: false,
            ..
        }
    ));
    // 1 is read-write: `bar_shape` writes the run buffer.
    assert_eq!(at(SET_BAR, 1).name, "runs");
    assert!(matches!(
        at(SET_BAR, 1).kind,
        BindingKind::Storage {
            read_only: false,
            ..
        }
    ));
    assert_eq!(at(SET_BAR, 2).name, "commands");
    assert!(matches!(
        at(SET_BAR, 2).kind,
        BindingKind::Storage {
            read_only: true,
            ..
        }
    ));
    assert!(matches!(at(SET_BAR, 3).kind, BindingKind::Uniform));

    // The visibility buffer is read *and* written; the swapchain image is
    // written only, and is the one whose texel format the session decides.
    assert_eq!(at(SET_BAR, 5).name, "ids");
    assert_eq!(
        at(SET_BAR, 5).kind,
        BindingKind::StorageTexture { write_only: false }
    );
    assert_eq!(at(SET_BAR, 6).name, "dst");
    assert_eq!(
        at(SET_BAR, 6).kind,
        BindingKind::StorageTexture { write_only: true }
    );
    // The image slots are a fixed-slot array; the preview is one
    // plain 2D texture.
    assert_eq!(at(SET_BAR, 7).kind, BindingKind::Texture { array: true });
    assert_eq!(at(SET_BAR, 8).kind, BindingKind::Sampler);
    assert_eq!(at(SET_BAR, 9).kind, BindingKind::Texture { array: false });

    // The literal pool, read-only: `bar_compose` looks a literal up in it and
    // writes the bytes somewhere else.
    assert_eq!(at(SET_BAR, 11).name, "literals");
    assert!(matches!(
        at(SET_BAR, 11).kind,
        BindingKind::Storage {
            read_only: true,
            ..
        }
    ));

    // **The caustic's own memory** (docs/abi.md, *The private buffer*): read-write, and the
    // one binding the embassy allocates by the stride the reflection reports
    // rather than by a constant of its own. It never writes or reads a word.
    assert_eq!(at(SET_BAR, 13).name, "private");
    assert!(matches!(
        at(SET_BAR, 13).kind,
        BindingKind::Storage {
            read_only: false,
            stride: Some(_)
        }
    ));
    assert!(
        private_stride(&bindings) > 4096,
        "the tables alone are tens of kilobytes"
    );

    // Group 2 opens with `shape.slang`'s scalars and is otherwise its tables —
    // all read-only but the strings buffer, which is read-write because
    // `bar_compose` writes the clock's bytes into it and one binding cannot be
    // two things.
    assert!(matches!(at(SET_SHAPE, 0).kind, BindingKind::Uniform));
    assert_eq!(at(SET_SHAPE, 1).name, "shapeOt");
    assert!(matches!(
        at(SET_SHAPE, 1).kind,
        BindingKind::Storage {
            read_only: true,
            ..
        }
    ));
    assert_eq!(at(SET_SHAPE, 2).name, "shapeUtf8");
    assert!(matches!(
        at(SET_SHAPE, 2).kind,
        BindingKind::Storage {
            read_only: false,
            ..
        }
    ));
    assert!(
        bindings
            .iter()
            .filter(|binding| binding.set == SET_SHAPE
                && binding.index > 0
                && binding.name != "shapeUtf8")
            .all(|binding| matches!(
                binding.kind,
                BindingKind::Storage {
                    read_only: true,
                    ..
                }
            ))
    );
}

/// A storage image the reflection names and this file does not know the texel
/// format of stops the bar rather than guessing one.
#[test]
#[should_panic(expected = "has no texel format for")]
fn a_third_storage_image_is_a_panic_not_a_guess() {
    storage_texture_format("shadow", wgpu::TextureFormat::Rgba8Unorm);
}

/// A gap in a set is named, not skipped: the set, the binding that is missing
/// and the two names it would have sat between.
#[test]
#[should_panic(expected = "set 0 has no binding 1")]
fn a_gap_in_a_set_names_itself_and_its_neighbours() {
    let holed: Vec<Binding> = baked_bindings()
        .into_iter()
        .filter(|binding| !(binding.set == SET_BAR && binding.index == 1))
        .collect();
    layout_entries(&holed, SET_BAR, wgpu::TextureFormat::Rgba8Unorm);
}

/// A reload whose caustic moved a binding is refused with the witness, not
/// swapped in under layouts that no longer describe it.
#[test]
fn a_moved_binding_is_the_witness_a_reload_refuses_on() {
    let baked = baked_bindings();
    assert_eq!(first_difference(&baked, &baked), None);

    let mut retyped = baked.clone();
    let samples = retyped
        .iter_mut()
        .find(|binding| binding.name == "samples")
        .expect("the samples frame is binding 10");
    samples.kind = BindingKind::Storage {
        read_only: false,
        stride: None,
    };
    let witness = first_difference(&baked, &retyped).expect("a retyped binding is a difference");
    assert!(witness.contains("set 0 binding 10"), "{witness}");
    assert!(witness.contains("samples"), "{witness}");

    let dropped: Vec<Binding> = baked
        .iter()
        .filter(|binding| binding.name != "textGlyphs")
        .cloned()
        .collect();
    let witness = first_difference(&baked, &dropped).expect("a dropped binding is a difference");
    assert!(witness.contains("textGlyphs"), "{witness}");
    assert!(witness.contains("is gone"), "{witness}");
}
