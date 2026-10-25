use super::*;

use caustics_bar_abi::BAR_ENTRIES;
use caustics_bar_abi::wire::format_code;

const RGBA8: &str = "DST_FORMAT_RGBA8";

fn module(generation: u32, index: usize, format: u32) -> Module {
    Module {
        generation,
        index: index as u32,
        count: BAR_ENTRIES.len() as u32,
        format,
        name: BAR_ENTRIES[index].to_owned(),
        // Four bytes of nothing: this file never looks inside, and the device
        // is the only thing that can say whether SPIR-V is SPIR-V.
        spirv: vec![0, 0, 0, 0],
        reflection: Reflection::default().to_json(),
    }
}

fn collecting() -> Modules {
    let mut modules = Modules::new(&BAR_ENTRIES);
    modules.expect(format_code(RGBA8));
    modules
}

/// The whole of the happy path: frames arrive one at a time, nothing happens
/// until the last one, and then the generation is whole and in `BAR_ENTRIES`
/// order.
#[test]
fn a_generation_is_ready_only_when_every_entry_point_has_arrived() {
    let mut modules = collecting();
    let format = format_code(RGBA8);
    for index in 0..BAR_ENTRIES.len() - 1 {
        match modules.collect(module(4, index, format)) {
            Collected::Waiting {
                generation,
                have,
                count,
            } => {
                assert_eq!(generation, 4);
                assert_eq!(have, index + 1);
                assert_eq!(count, BAR_ENTRIES.len());
            }
            _ => panic!("entry {index} completed a generation on its own"),
        }
    }
    let last = module(4, BAR_ENTRIES.len() - 1, format);
    let Collected::Ready(ready) = modules.collect(last) else {
        panic!("the last frame did not complete the generation");
    };
    assert_eq!(ready.generation, 4);
    assert_eq!(ready.len(), BAR_ENTRIES.len());
    assert_eq!(ready.spirv().len(), BAR_ENTRIES.len());
    assert!(
        ready
            .reflection()
            .expect("a readable reflection")
            .strings
            .is_empty()
    );
}

/// Out of order is ordinary — the wire says nothing about arrival order — and
/// what comes back is `BAR_ENTRIES` order regardless, because that is the order
/// `reload_all` builds the pipelines in.
#[test]
fn frames_out_of_order_still_land_in_dispatch_order() {
    let mut modules = collecting();
    let format = format_code(RGBA8);
    let mut order: Vec<usize> = (0..BAR_ENTRIES.len()).collect();
    order.reverse();
    let mut ready = None;
    for index in order {
        if let Collected::Ready(whole) = modules.collect(module(9, index, format)) {
            ready = Some(whole);
        }
    }
    let ready = ready.expect("every entry point's frame completed");
    for (module, entry) in ready.modules.iter().zip(BAR_ENTRIES) {
        assert_eq!(module.name, entry);
    }
}

/// A compiler that recompiled before the bar had the whole of the last
/// generation: the older one is stale and is dropped where it stands.
#[test]
fn a_newer_generation_abandons_an_incomplete_one() {
    let mut modules = collecting();
    let format = format_code(RGBA8);
    assert!(matches!(
        modules.collect(module(1, 0, format)),
        Collected::Waiting { .. }
    ));
    assert!(matches!(
        modules.collect(module(1, 1, format)),
        Collected::Waiting { .. }
    ));
    match modules.collect(module(2, 0, format)) {
        Collected::Waiting {
            generation, have, ..
        } => {
            assert_eq!(generation, 2);
            assert_eq!(have, 1, "generation 1's two frames did not go");
        }
        _ => panic!("a new generation completed on its first frame"),
    }
}

/// Three ways a generation is not this bar's, each one witness and no swap.
#[test]
fn a_generation_for_another_program_is_refused_and_never_held() {
    let format = format_code(RGBA8);

    // Before the device has a format there is nothing to compare against.
    let mut cold = Modules::new(&BAR_ENTRIES);
    assert!(matches!(
        cold.collect(module(1, 0, format)),
        Collected::Refused(_)
    ));

    // The other swapchain format: SPIR-V bakes the storage-image format in.
    let mut modules = collecting();
    let foreign = module(1, 0, format_code("DST_FORMAT_BGRA8"));
    assert!(matches!(modules.collect(foreign), Collected::Refused(_)));

    // A compiler whose entry list is not this bar's, both ways it can differ.
    let mut renamed = module(1, 0, format);
    renamed.name = "bar_something_else".to_owned();
    assert!(matches!(modules.collect(renamed), Collected::Refused(_)));
    let mut shorter = module(1, 0, format);
    shorter.count = 3;
    assert!(matches!(modules.collect(shorter), Collected::Refused(_)));

    // And none of them left anything behind: the good generation still needs
    // one frame per entry point and then completes.
    let mut ready = None;
    for index in 0..BAR_ENTRIES.len() {
        if let Collected::Ready(whole) = modules.collect(module(1, index, format)) {
            ready = Some(whole);
        }
    }
    assert!(ready.is_some());
}

/// The strings are the caustic's own words: merged across
/// the entry points, because reflection reports only what each one reaches —
/// and refused whole if any part of it cannot be read.
#[test]
fn the_literals_are_merged_across_the_entry_points() {
    let mut modules = collecting();
    let format = format_code(RGBA8);
    let mut ready = None;
    for index in 0..BAR_ENTRIES.len() {
        let mut frame = module(1, index, format);
        frame.reflection = Reflection {
            strings: [(index as u32, format!("word{index}"))].into(),
            ..Reflection::default()
        }
        .to_json();
        if let Collected::Ready(whole) = modules.collect(frame) {
            ready = Some(whole);
        }
    }
    let table = ready
        .expect("complete")
        .reflection()
        .expect("readable")
        .strings;
    assert_eq!(table.len(), BAR_ENTRIES.len());
    assert_eq!(table.get(&3).map(String::as_str), Some("word3"));

    let mut modules = collecting();
    let mut ready = None;
    for index in 0..BAR_ENTRIES.len() {
        let mut frame = module(2, index, format);
        if index == 2 {
            frame.reflection = "{ not json".to_owned();
        }
        if let Collected::Ready(whole) = modules.collect(frame) {
            ready = Some(whole);
        }
    }
    assert!(
        ready.expect("complete").reflection().is_err(),
        "half a table is not a table"
    );
}
