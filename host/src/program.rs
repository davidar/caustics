//! The caustic on the device: its bind group layouts, its pipelines, and the
//! reload that swaps the pipelines whole or not at all.
//!
//! Three shapes here are deliberate:
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
//! - **the pipelines are built off the host's loop** ([`crate::compile`]): the
//!   layouts are made at once, and each entry's pipeline is queued on the
//!   compile workers in the host's order of need and installed when it lands
//!   ([`Program::poll`]). A host asks [`Program::pipeline`] for what exists
//!   and skips the dispatches whose pipeline does not, so a cold driver cache
//!   delays what is drawn and never the loop. A reload is the same queue,
//!   staged beside the running program and swapped in whole.
//!
//! The entry points are the host's: an [`Entry`] per point, in dispatch order,
//! and a pipeline is named by its index in that list (the bar's list is the
//! ABI crate's `BAR_ENTRIES`).

use std::num::NonZeroU64;

use anyhow::{Context, Result, bail};
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
///
/// On the caller's thread and waited out: for a pipeline a test loads beside
/// a program (the bar's oracle loads `bar_check`). A host's own pipelines go
/// through [`Program`], which builds them off its loop.
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

/// A bind group over `set`'s bindings with a zeroed resource of its own for
/// each, for the warming dispatch ([`crate::compile::Warm`]): a uniform of
/// the largest size a uniform binding may be, a storage buffer of one
/// element of its stride and never under 64 KiB, a 1x1 image of each texture
/// binding's kind and format, a sampler.
fn scratch_group(
    device: &wgpu::Device,
    bindings: &[Binding],
    set: u32,
    layout: &wgpu::BindGroupLayout,
    storage_format: &dyn Fn(&str) -> wgpu::TextureFormat,
) -> wgpu::BindGroup {
    const STORAGE_FLOOR: u64 = 64 * 1024;
    const UNIFORM: u64 = 16 * 1024;
    enum Held {
        Buffer(wgpu::Buffer),
        View(wgpu::TextureView),
        Sampler(wgpu::Sampler),
    }
    let image = |format: wgpu::TextureFormat, usage: wgpu::TextureUsages, array: bool| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("caustics-warm-image"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(if array {
                    wgpu::TextureViewDimension::D2Array
                } else {
                    wgpu::TextureViewDimension::D2
                }),
                ..Default::default()
            })
    };
    let buffer = |size: u64, usage: wgpu::BufferUsages| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caustics-warm-buffer"),
            size,
            usage,
            mapped_at_creation: false,
        })
    };
    let held: Vec<(u32, Held)> = bindings
        .iter()
        .filter(|binding| binding.set == set)
        .map(|binding| {
            let resource = match binding.kind {
                BindingKind::Uniform => Held::Buffer(buffer(UNIFORM, wgpu::BufferUsages::UNIFORM)),
                BindingKind::Storage { stride, .. } => {
                    let stride = u64::from(stride.unwrap_or(4)).max(4);
                    let size = STORAGE_FLOOR.max(stride).next_multiple_of(stride);
                    Held::Buffer(buffer(size, wgpu::BufferUsages::STORAGE))
                }
                BindingKind::StorageTexture { .. } => Held::View(image(
                    storage_format(&binding.name),
                    wgpu::TextureUsages::STORAGE_BINDING,
                    false,
                )),
                BindingKind::Texture { array } => Held::View(image(
                    wgpu::TextureFormat::Rgba8Unorm,
                    wgpu::TextureUsages::TEXTURE_BINDING,
                    array,
                )),
                BindingKind::Sampler => {
                    Held::Sampler(device.create_sampler(&wgpu::SamplerDescriptor::default()))
                }
            };
            (binding.index, resource)
        })
        .collect();
    let entries: Vec<wgpu::BindGroupEntry<'_>> = held
        .iter()
        .map(|(index, held)| wgpu::BindGroupEntry {
            binding: *index,
            resource: match held {
                Held::Buffer(buffer) => buffer.as_entire_binding(),
                Held::View(view) => wgpu::BindingResource::TextureView(view),
                Held::Sampler(sampler) => wgpu::BindingResource::Sampler(sampler),
            },
        })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("caustics-warm-group"),
        layout,
        entries: &entries,
    })
}

/// Why a reload kept the program it had.
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

/// What one landed result did to the program ([`Program::poll`]).
#[derive(Debug)]
pub enum Landed {
    /// A pipeline of the running program is in, the entry at `entry`; `all`
    /// when it was the last one owed. A host draws what it now can.
    Live { entry: usize, all: bool },
    /// A pipeline of the running program was refused. Only the baked caustic
    /// is ever the running program before it is whole, so this is a build
    /// whose own SPIR-V does not load, which is a build to fix, as it was
    /// when the pipelines were built at bring-up.
    Broken(String),
    /// A reload's generation is whole, every pipeline built and validated,
    /// and is now the running program.
    Swapped { generation: u64 },
    /// A reload's generation was refused by the device; the running program
    /// is untouched.
    Refused { generation: u64, error: Refused },
}

/// One entry's pipeline in the running program.
enum Slot {
    Pending,
    Ready(wgpu::ComputePipeline),
}

/// A reload being built: its pipelines as they land, swapped in together
/// when the last does.
struct Staged {
    generation: u64,
    pipelines: Vec<Option<wgpu::ComputePipeline>>,
}

/// A caustic on the device: one bind group layout per descriptor set, the
/// pipeline layout over them, one pipeline per entry point (built on the
/// compile workers and installed as each lands), and the bindings the
/// layouts were built from.
pub struct Program {
    /// By set number: the set at position `n` of the pipeline layout is set
    /// `n`.
    layouts: Vec<wgpu::BindGroupLayout>,
    pipeline_layout: wgpu::PipelineLayout,
    entries: &'static [Entry],
    /// The entries' indices in the order a host needs them: what is queued
    /// first, at bring-up and on every reload.
    need: Vec<usize>,
    /// One per entry, in the entries' order.
    live: Vec<Slot>,
    /// The generation `live` is: 1 for the baked caustic, then one more per
    /// reload swapped in.
    generation: u64,
    /// The reload being built, if one is.
    staged: Option<Staged>,
    /// The last generation number handed out.
    issued: u64,
    /// When the running generation was queued, for the line that says it is
    /// whole.
    queued_at: std::time::Instant,
    compiler: crate::compile::Compiler,
    /// The bindings the layouts above were built from, as the baked
    /// reflection declared them. A reload compares its own compile's against
    /// these and refuses rather than swap in modules the layouts do not
    /// describe.
    bindings: Vec<Binding>,
}

impl Program {
    /// Build the layouts from `bindings` now, and queue the pipelines from
    /// `spirv` on the compile workers: this returns before any is built.
    ///
    /// `queue` carries each pipeline's warming dispatch ([`crate::compile`]).
    /// `sets` is every descriptor set's label, set 0 first: the reflection
    /// must declare exactly those sets, or this panics ([`assert_sets`]).
    /// `storage_format` names a storage image's texel format by the image's
    /// name. `entries` and `spirv` are one module per entry point, in
    /// dispatch order. `need` is every entry's index once, in the order the
    /// host wants them built: what its first frame needs first. `workers`
    /// threads build, and `wake` is called from one of them each time a
    /// result is waiting for [`Program::poll`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: Vec<Binding>,
        sets: &[&str],
        pipeline_label: &str,
        storage_format: &dyn Fn(&str) -> wgpu::TextureFormat,
        entries: &'static [Entry],
        spirv: &[&[u8]],
        need: &[usize],
        workers: usize,
        wake: crate::compile::Wake,
    ) -> Result<Program> {
        if spirv.len() != entries.len() {
            bail!(
                "{} modules for {} entry points; a program is one module per entry point",
                spirv.len(),
                entries.len()
            );
        }
        let mut sorted = need.to_vec();
        sorted.sort_unstable();
        if sorted != (0..entries.len()).collect::<Vec<_>>() {
            bail!(
                "the order of need {need:?} does not name each of the {} entry points once",
                entries.len()
            );
        }
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
        let warm = crate::compile::Warm {
            queue: queue.clone(),
            groups: (0..sets.len() as u32)
                .map(|set| {
                    scratch_group(
                        device,
                        &bindings,
                        set,
                        &layouts[set as usize],
                        storage_format,
                    )
                })
                .collect(),
        };
        let compiler =
            crate::compile::Compiler::start(device, &pipeline_layout, warm, workers, wake);
        let program = Program {
            layouts,
            pipeline_layout,
            entries,
            need: need.to_vec(),
            live: entries.iter().map(|_| Slot::Pending).collect(),
            generation: 1,
            staged: None,
            issued: 1,
            queued_at: std::time::Instant::now(),
            compiler,
            bindings,
        };
        program.queue(1, spirv);
        tracing::info!(
            "pipelines: {} queued on {} compile worker{}, in the order {}",
            entries.len(),
            program.compiler.workers(),
            if program.compiler.workers() == 1 {
                ""
            } else {
                "s"
            },
            program
                .need
                .iter()
                .map(|&index| entries[index].name)
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(program)
    }

    /// One generation's jobs, in the order of need.
    fn queue(&self, generation: u64, spirv: &[&[u8]]) {
        self.compiler
            .submit(self.need.iter().map(|&index| crate::compile::Job {
                generation,
                slot: index,
                label: self.entries[index].label,
                entry: self.entries[index].name,
                workgroup: self.entries[index].workgroup,
                spirv: spirv[index].to_vec(),
            }));
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

    /// The pipeline of the entry at `index` in the entry list, once it is
    /// built. A dispatch whose pipeline is not here yet is not recorded: the
    /// host skips the work that needs it and does it when it lands.
    pub fn pipeline(&self, index: usize) -> Option<&wgpu::ComputePipeline> {
        match &self.live[index] {
            Slot::Ready(pipeline) => Some(pipeline),
            Slot::Pending => None,
        }
    }

    /// Whether every one of these entries' pipelines is built.
    pub fn has(&self, indices: &[usize]) -> bool {
        indices
            .iter()
            .all(|&index| matches!(self.live[index], Slot::Ready(_)))
    }

    /// Whether a pipeline is being warmed on the device's queue
    /// ([`crate::compile`]): a host submits nothing until it lands, since its
    /// submission would wait behind a driver's whole compile.
    pub fn warming(&self) -> bool {
        self.compiler.warming()
    }

    /// Whether every pipeline of the running program is built.
    pub fn whole(&self) -> bool {
        self.live.iter().all(|slot| matches!(slot, Slot::Ready(_)))
    }

    /// Whether a reload is being built.
    pub fn reloading(&self) -> bool {
        self.staged.is_some()
    }

    /// The running program's generation: 1 for the one [`Program::new`]
    /// queued, then the number [`Program::reload`] returned for each one
    /// swapped in since.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The bindings the layouts were built from.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Queue a freshly compiled generation, to be swapped in whole once every
    /// one of its pipelines is built, or not at all; returns its number,
    /// which [`Landed::Swapped`] and [`Landed::Refused`] carry. Nothing but
    /// the pipelines is touched, so a host keeps its buffers, and with them
    /// the caustic's state, across a reload.
    ///
    /// A reload still being built when this is called is superseded: its
    /// queued jobs are withdrawn and its results, when the ones already on a
    /// worker land, are dropped.
    ///
    /// `reflection` is that same compile's reflection. The layouts were
    /// built from the *baked* one and cannot be rebuilt without rebuilding
    /// the pipeline layout and every bind group hanging off it, so a caustic
    /// that moved, added or retyped a binding is refused here, at once,
    /// rather than queued: what a live reload would otherwise produce is a
    /// dispatch whose descriptors and whose `[[vk::binding]]` literals
    /// disagree — a device-lost on one driver, silently wrong buffers on
    /// another.
    pub fn reload(&mut self, reflection: &Reflection, spirv: &[&[u8]]) -> Result<u64, Refused> {
        if let Some(witness) = first_difference(&self.bindings, &reflection.bindings) {
            return Err(Refused::Bindings(witness));
        }
        if spirv.len() != self.entries.len() {
            return Err(Refused::Pipelines(anyhow::anyhow!(
                "{} modules for {} entry points; a program is one module per entry point",
                spirv.len(),
                self.entries.len()
            )));
        }
        if let Some(old) = self.staged.take() {
            let withdrawn = self.compiler.withdraw(old.generation);
            tracing::info!(
                "pipelines: reload {} superseded before it was whole ({withdrawn} not yet started)",
                old.generation
            );
        }
        self.issued += 1;
        let generation = self.issued;
        self.staged = Some(Staged {
            generation,
            pipelines: self.entries.iter().map(|_| None).collect(),
        });
        self.queue(generation, spirv);
        Ok(generation)
    }

    /// Install every result that has landed, and say what each did. Called
    /// by the host when the wake it passed to [`Program::new`] fires.
    pub fn poll(&mut self) -> Vec<Landed> {
        self.compiler
            .drain()
            .into_iter()
            .filter_map(|built| self.land(built))
            .collect()
    }

    /// Block until every pipeline of the running program is built: for a
    /// host that runs its pipelines synchronously (the device tests).
    pub fn wait_ready(&mut self) -> Result<()> {
        while !self.whole() {
            let built = self
                .compiler
                .wait()
                .context("the compile workers are gone")?;
            if let Some(Landed::Broken(err)) = self.land(built) {
                bail!("{err}");
            }
        }
        Ok(())
    }

    /// Block until the reload being built is swapped in or refused: the
    /// synchronous reload, for the device tests.
    pub fn wait_staged(&mut self) -> Result<(), Refused> {
        while let Some(staged) = &self.staged {
            let generation = staged.generation;
            let built = self.compiler.wait().ok_or_else(|| {
                Refused::Pipelines(anyhow::anyhow!("the compile workers are gone"))
            })?;
            match self.land(built) {
                Some(Landed::Swapped { generation: done }) if done == generation => return Ok(()),
                Some(Landed::Refused {
                    generation: done,
                    error,
                }) if done == generation => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }

    fn land(&mut self, built: crate::compile::Built) -> Option<Landed> {
        let entry = &self.entries[built.slot];
        let took = crate::compile::human(built.took);
        if built.generation == self.generation {
            return match built.result {
                Ok(pipeline) => {
                    tracing::info!("pipeline {}: built in {took}", entry.name);
                    self.live[built.slot] = Slot::Ready(pipeline);
                    let all = self.whole();
                    if all {
                        tracing::info!(
                            "pipelines: all {} built, {} after they were queued",
                            self.entries.len(),
                            crate::compile::human(self.queued_at.elapsed())
                        );
                    }
                    Some(Landed::Live {
                        entry: built.slot,
                        all,
                    })
                }
                Err(err) => Some(Landed::Broken(format!(
                    "pipeline {} was refused: {err}",
                    entry.name
                ))),
            };
        }
        let Some(staged) = self.staged.as_mut() else {
            tracing::debug!(
                "pipeline {}: generation {} landed after it stopped mattering",
                entry.name,
                built.generation
            );
            return None;
        };
        if staged.generation != built.generation {
            tracing::debug!(
                "pipeline {}: generation {} landed after it stopped mattering",
                entry.name,
                built.generation
            );
            return None;
        }
        let generation = staged.generation;
        match built.result {
            Ok(pipeline) => {
                tracing::info!(
                    "pipeline {}: built in {took} (reload {generation})",
                    entry.name
                );
                staged.pipelines[built.slot] = Some(pipeline);
                if staged.pipelines.iter().any(Option::is_none) {
                    return None;
                }
                let staged = self.staged.take().expect("checked above");
                // Whatever of the running generation is still queued is
                // for a program that is no longer running.
                self.compiler.withdraw(self.generation);
                self.live = staged
                    .pipelines
                    .into_iter()
                    .map(|pipeline| Slot::Ready(pipeline.expect("every one landed")))
                    .collect();
                self.generation = generation;
                Some(Landed::Swapped { generation })
            }
            Err(err) => {
                self.staged = None;
                self.compiler.withdraw(generation);
                Some(Landed::Refused {
                    generation,
                    error: Refused::Pipelines(anyhow::anyhow!(
                        "pipeline creation rejected for {}: {err}",
                        entry.name
                    )),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests;
