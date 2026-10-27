//! The caustic on the device: its bind group layouts, its pipelines, and the
//! reload that swaps the pipelines whole or not at all.
//!
//! Two shapes here are deliberate:
//!
//! - **passthrough SPIR-V.** The caustic is Slang; wgpu takes the module
//!   through `create_shader_module_passthrough`, which performs no validation
//!   of its own, so every pipeline build sits inside a validation error scope
//!   and a reload is all-or-nothing ([`Program::reload`]).
//! - **the layouts are built from the compiler's reflection**, not from a list
//!   in the host ([`layout_entries`]), so there is no second copy of every
//!   binding on the CPU to keep dense, in order, and in agreement with the
//!   `[[vk::binding(n, g)]]` literals in the caustic. What is left to the host
//!   is the two things a reflection cannot carry, both keyed on the
//!   parameter's name: a storage image's texel format (the `storage_format`
//!   closure) and which resource to bind ([`group_entries`]'s closure).
//!
//! The entry points are the host's: an [`Entry`] per point, in dispatch order,
//! and a pipeline is named by its index in that list (the bar's list is the
//! ABI crate's `BAR_ENTRIES`).

use std::num::NonZeroU64;

use anyhow::{Result, bail};
use punchcutter_core::gpu::passthrough_pipeline;
use punchcutter_slang::{Binding, BindingKind, Reflection};

/// One compute entry point: its name in the module, the pipeline's debug
/// label, and its `[numthreads]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: &'static str,
    pub label: &'static str,
    pub workgroup: (u32, u32, u32),
}

/// A reflection baked beside a host's SPIR-V, read.
///
/// Panics rather than returns: a reflection the host cannot read means it
/// does not know what descriptors its own caustic wants, and there is no frame
/// worth drawing after that. It is baked at build time by the same crate
/// that reads it, so this can only fail on a build whose two halves
/// disagree — which is a build to fix, not a session to limp through.
pub fn read_baked(json: &str) -> Reflection {
    Reflection::from_json(json).unwrap_or_else(|err| {
        panic!("the reflection baked beside the SPIR-V cannot be read: {err}")
    })
}

/// What the reflection says a caustic asks a driver for: bindings, descriptor
/// sets, and how many of the bindings are storage buffers — the two a driver
/// has a limit on, and the third for the log line.
///
/// Counted rather than written down, because the number moves whenever a
/// caustic adds a buffer.
pub fn binding_counts(bindings: &[Binding]) -> (usize, usize, usize) {
    let sets: std::collections::BTreeSet<u32> = bindings.iter().map(|b| b.set).collect();
    let storage = bindings
        .iter()
        .filter(|binding| matches!(binding.kind, BindingKind::Storage { .. }))
        .count();
    (bindings.len(), sets.len(), storage)
}

/// The sets the reflection declares are exactly the ones the pipeline layout
/// binds, or the host stops here.
///
/// A set too many (or a missing one) means a caustic grew a descriptor space
/// nothing on the CPU fills, which the pipeline layout would accept and the
/// first dispatch would read as garbage.
pub fn assert_sets(bindings: &[Binding], expected: &[u32]) {
    let found: std::collections::BTreeSet<u32> = bindings.iter().map(|b| b.set).collect();
    let expected: std::collections::BTreeSet<u32> = expected.iter().copied().collect();
    if found != expected {
        panic!(
            "the caustic declares descriptor sets {found:?}, but the host builds layouts for \
             {expected:?}; a set with no layout is a dispatch reading descriptors nobody wrote."
        );
    }
}

/// One descriptor set's layout entries, in binding order, from the reflection.
///
/// **wgpu-hal numbers a Vulkan binding by an entry's position in this list**,
/// so a gap renumbers every binding after it and the `[[vk::binding(n, g)]]`
/// literals in the caustics — which cannot be expressions — would then name
/// different buffers than the descriptors do, with no error anywhere. A gap is
/// a panic at startup, which makes the hazard unshippable rather than tested
/// for.
///
/// `storage_format` is the one fact about a binding the reflection cannot
/// carry: a storage image's texel format, by the image's name.
pub fn layout_entries(
    all: &[Binding],
    set: u32,
    storage_format: &dyn Fn(&str) -> wgpu::TextureFormat,
) -> Vec<wgpu::BindGroupLayoutEntry> {
    let group: Vec<&Binding> = all.iter().filter(|binding| binding.set == set).collect();
    let mut entries = Vec::with_capacity(group.len());
    for (position, binding) in group.iter().enumerate() {
        if binding.index != position as u32 {
            // `position` is the binding number that should have been here, so
            // it names the hole; the neighbours say where to look for it.
            let before = position
                .checked_sub(1)
                .and_then(|earlier| group.get(earlier))
                .map_or("the start of the set", |earlier| earlier.name.as_str());
            panic!(
                "set {set} has no binding {position}: `{}` comes after `{}` but binds at {}. \
                 wgpu-hal numbers a Vulkan binding by its entry's position in the layout, so a \
                 gap silently renumbers every binding after it — the caustic and the \
                 descriptors would name different buffers with nothing to report it.",
                binding.name, before, binding.index
            );
        }
        entries.push(layout_entry(binding, storage_format));
    }
    entries
}

/// One reflected binding as a wgpu layout entry. Everything is `COMPUTE`: a
/// caustic has no other stage.
fn layout_entry(
    binding: &Binding,
    storage_format: &dyn Fn(&str) -> wgpu::TextureFormat,
) -> wgpu::BindGroupLayoutEntry {
    let ty = match binding.kind {
        BindingKind::Uniform => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            // The struct's size is pinned far more strictly than a floor here
            // would: `bar/src/abi/tests.rs` compares Slang's own field offsets
            // against `offset_of!` on the Rust half, every build.
            min_binding_size: None,
        },
        BindingKind::Storage { read_only, stride } => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            // One element, which is what the caustic indexes by. The buffers
            // are all bound whole.
            min_binding_size: stride.and_then(|stride| NonZeroU64::new(u64::from(stride))),
        },
        BindingKind::StorageTexture { write_only } => wgpu::BindingType::StorageTexture {
            access: if write_only {
                wgpu::StorageTextureAccess::WriteOnly
            } else {
                wgpu::StorageTextureAccess::ReadWrite
            },
            format: storage_format(&binding.name),
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        BindingKind::Texture { array } => wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: if array {
                wgpu::TextureViewDimension::D2Array
            } else {
                wgpu::TextureViewDimension::D2
            },
            multisampled: false,
        },
        BindingKind::Sampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
    };
    wgpu::BindGroupLayoutEntry {
        binding: binding.index,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

/// One descriptor set's layout.
pub fn create_layout(
    device: &wgpu::Device,
    label: &str,
    bindings: &[Binding],
    set: u32,
    storage_format: &dyn Fn(&str) -> wgpu::TextureFormat,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &layout_entries(bindings, set, storage_format),
    })
}

/// One descriptor set's bind-group entries, **by name**, from the same
/// reflection the layout was built from (docs/abi.md, *Bindings and the oracle*).
///
/// The number is the caustic's; this reads it rather than transcribing it, so
/// a binding that moves moves in one file. The `[[vk::binding]]` literals stay — an attribute argument cannot be
/// a module constant — and the reflection they produce is what is read here.
/// `resource` is the host's: the buffer or texture a reflected name is
/// handed.
pub fn group_entries<'a>(
    bindings: &[Binding],
    set: u32,
    mut resource: impl FnMut(&str) -> wgpu::BindingResource<'a>,
) -> Vec<wgpu::BindGroupEntry<'a>> {
    bindings
        .iter()
        .filter(|binding| binding.set == set)
        .map(|binding| wgpu::BindGroupEntry {
            binding: binding.index,
            resource: resource(&binding.name),
        })
        .collect()
}

/// The first place two binding lists disagree, named — or `None` when they are
/// the same list.
///
/// Both are sorted by `(set, index)`, so this is a merge: a binding present in
/// one and not the other is named with where it sits, and one present in both
/// is compared whole (a `StructuredBuffer` that became `RWStructuredBuffer`, a
/// texture that grew an array dimension, a renamed parameter). The witness
/// goes into a log line a person has to act on, so it names one thing rather than
/// summarising the diff.
pub fn first_difference(old: &[Binding], new: &[Binding]) -> Option<String> {
    let where_at = |binding: &Binding| format!("set {} binding {}", binding.set, binding.index);
    let mut olds = old.iter().peekable();
    let mut news = new.iter().peekable();
    loop {
        match (olds.peek(), news.peek()) {
            (None, None) => return None,
            (Some(was), None) => {
                return Some(format!("`{}` at {} is gone", was.name, where_at(was)));
            }
            (None, Some(now)) => {
                return Some(format!("`{}` at {} is new", now.name, where_at(now)));
            }
            (Some(was), Some(now)) => match (was.set, was.index).cmp(&(now.set, now.index)) {
                std::cmp::Ordering::Less => {
                    return Some(format!("`{}` at {} is gone", was.name, where_at(was)));
                }
                std::cmp::Ordering::Greater => {
                    return Some(format!("`{}` at {} is new", now.name, where_at(now)));
                }
                std::cmp::Ordering::Equal => {
                    if was != now {
                        return Some(format!(
                            "{} was `{}` {:?} and is now `{}` {:?}",
                            where_at(was),
                            was.name,
                            was.kind,
                            now.name,
                            now.kind
                        ));
                    }
                    olds.next();
                    news.next();
                }
            },
        }
    }
}

/// Every pipeline of `entries`, from `spirv` in the same order, inside one
/// validation error scope: either every one of them is good, or the caller
/// keeps what it had.
pub fn build_pipelines(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    entries: &[Entry],
    spirv: &[&[u8]],
) -> Result<Vec<wgpu::ComputePipeline>> {
    if spirv.len() != entries.len() {
        bail!(
            "{} modules for {} entry points; a program is one module per entry point",
            spirv.len(),
            entries.len()
        );
    }
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipelines: Vec<wgpu::ComputePipeline> = entries
        .iter()
        .zip(spirv)
        .map(|(entry, spv)| {
            // The driver compiles the module here, on this thread, and the
            // time is the driver's: milliseconds from a warm shader cache,
            // seconds from a cold one, minutes on a software renderer. The
            // line says which pipeline a slow start was waiting on.
            let started = std::time::Instant::now();
            let pipeline = passthrough_pipeline(
                device,
                layout,
                entry.label,
                entry.name,
                spv,
                entry.workgroup,
            );
            tracing::info!(
                "pipeline {}: built in {:.3} s",
                entry.name,
                started.elapsed().as_secs_f64()
            );
            pipeline
        })
        .collect();
    if let Some(err) = pollster::block_on(scope.pop()) {
        bail!("pipeline creation rejected: {err}");
    }
    Ok(pipelines)
}

/// Why [`Program::reload`] kept the program it had.
#[derive(Debug)]
pub enum Refused {
    /// The new compile's bindings are not the ones the layouts were built
    /// from; the witness names the first difference. The layouts cannot be
    /// rebuilt under a running host's bind groups, so this one needs a
    /// restart.
    Bindings(String),
    /// The device rejected a pipeline.
    Pipelines(anyhow::Error),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Bindings(witness) => write!(f, "bindings changed ({witness})"),
            Refused::Pipelines(err) => write!(f, "{err:#}"),
        }
    }
}

impl std::error::Error for Refused {}

/// A caustic on the device: one bind group layout per descriptor set, the
/// pipeline layout over them, one pipeline per entry point, and the bindings
/// the layouts were built from.
pub struct Program {
    /// By set number: the set at position `n` of the pipeline layout is set
    /// `n`.
    layouts: Vec<wgpu::BindGroupLayout>,
    pipeline_layout: wgpu::PipelineLayout,
    entries: &'static [Entry],
    /// One per entry, in the entries' order; swapped together or not at all.
    pipelines: Vec<wgpu::ComputePipeline>,
    /// The bindings the layouts above were built from, as the baked
    /// reflection declared them. A reload compares its own compile's against
    /// these and refuses rather than swap in modules the layouts do not
    /// describe.
    bindings: Vec<Binding>,
}

impl Program {
    /// Build the layouts from `bindings` and the pipelines from `spirv`.
    ///
    /// `sets` is every descriptor set's label, set 0 first: the reflection
    /// must declare exactly those sets, or this panics ([`assert_sets`]).
    /// `storage_format` names a storage image's texel format by the image's
    /// name. `entries` and `spirv` are one module per entry point, in
    /// dispatch order.
    pub fn new(
        device: &wgpu::Device,
        bindings: Vec<Binding>,
        sets: &[&str],
        pipeline_label: &str,
        storage_format: &dyn Fn(&str) -> wgpu::TextureFormat,
        entries: &'static [Entry],
        spirv: &[&[u8]],
    ) -> Result<Program> {
        let numbers: Vec<u32> = (0..sets.len() as u32).collect();
        assert_sets(&bindings, &numbers);
        let layouts: Vec<wgpu::BindGroupLayout> = sets
            .iter()
            .zip(&numbers)
            .map(|(label, set)| create_layout(device, label, &bindings, *set, storage_format))
            .collect();
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(pipeline_label),
            bind_group_layouts: &layouts.iter().map(Some).collect::<Vec<_>>(),
            immediate_size: 0,
        });
        let pipelines = build_pipelines(device, &pipeline_layout, entries, spirv)?;
        Ok(Program {
            layouts,
            pipeline_layout,
            entries,
            pipelines,
            bindings,
        })
    }

    /// Set `set`'s layout.
    pub fn layout(&self, set: u32) -> &wgpu::BindGroupLayout {
        &self.layouts[set as usize]
    }

    /// The pipeline layout every pipeline of this program is built against,
    /// for a host that loads one more (the bar's oracle does).
    pub fn pipeline_layout(&self) -> &wgpu::PipelineLayout {
        &self.pipeline_layout
    }

    /// The pipeline of the entry at `index` in the entry list.
    pub fn pipeline(&self, index: usize) -> &wgpu::ComputePipeline {
        &self.pipelines[index]
    }

    /// The bindings the layouts were built from.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Rebuild every pipeline from freshly compiled SPIR-V, or change
    /// nothing. Nothing but the pipelines is touched, so a host keeps its
    /// buffers, and with them the caustic's state, across a reload.
    ///
    /// `reflection` is that same compile's reflection. The layouts were
    /// built from the *baked* one and cannot be rebuilt without rebuilding
    /// the pipeline layout and every bind group hanging off it, so a caustic
    /// that moved, added or retyped a binding is refused here rather than
    /// swapped in: what a live reload would otherwise produce is a dispatch
    /// whose descriptors and whose `[[vk::binding]]` literals disagree — a
    /// device-lost on one driver, silently wrong buffers on another.
    pub fn reload(
        &mut self,
        device: &wgpu::Device,
        reflection: &Reflection,
        spirv: &[&[u8]],
    ) -> Result<(), Refused> {
        if let Some(witness) = first_difference(&self.bindings, &reflection.bindings) {
            return Err(Refused::Bindings(witness));
        }
        self.pipelines = build_pipelines(device, &self.pipeline_layout, self.entries, spirv)
            .map_err(Refused::Pipelines)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
