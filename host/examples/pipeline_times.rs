//! How long the driver takes to build a caustic's pipelines, on the headless
//! device the oracle uses.
//!
//! ```text
//! VK_DRIVER_FILES=/usr/share/vulkan/icd.d/radeon_icd.x86_64.json \
//! MESA_SHADER_CACHE_DIR=<scratch> \
//! cargo run -p caustics-host --example pipeline_times -- \
//!     <program>.reflection.json <entry>.rgba8unorm.spv ...
//! ```
//!
//! The arguments are what a host's build script bakes into `OUT_DIR`: the
//! program's reflection, then any of its entry points' modules, each named
//! `<entry>.<format>.spv`. The pipelines are built through
//! [`Program::new`], the path every host starts on, and each one's time is
//! that path's own witness line; the totals for the device and the program
//! follow. Leaving a module off the list is how a start without that
//! pipeline is timed.
//!
//! `--dispatch` then dispatches each pipeline once, one workgroup over
//! zeroed buffers (the state every host starts from), and times it, twice.
//! A driver that compiles at pipeline creation shows two short dispatches;
//! one that compiles on first use (llvmpipe, under lavapipe, JITs a compute
//! shader when it is first launched) shows the compile in the first.
//!
//! `--parallel` builds each pipeline on a thread of its own, against the
//! same layout, which is what a host compiling off its main thread would
//! do; the program's time is then the slowest pipeline's, not the sum.
//!
//! `--pipeline-cache <file>` builds the same pipelines against a wgpu
//! pipeline cache seeded from `<file>` and writes the cache back to it
//! afterwards, which is what a host persisting its own cache would do.

use std::time::Instant;

use anyhow::{Context, Result, bail};
use caustics_host::device::Device;
use caustics_host::program::{Entry, Program, read_baked};
use caustics_host::surface::ID_FORMAT;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let dispatch = match args.iter().position(|arg| arg == "--dispatch") {
        Some(at) => {
            args.remove(at);
            true
        }
        None => false,
    };
    let parallel = match args.iter().position(|arg| arg == "--parallel") {
        Some(at) => {
            args.remove(at);
            true
        }
        None => false,
    };
    let cache_file = match args.iter().position(|arg| arg == "--pipeline-cache") {
        Some(at) => {
            args.remove(at);
            Some(std::path::PathBuf::from(args.remove(at)))
        }
        None => None,
    };
    let Some((reflection, modules)) = args.split_first() else {
        bail!("usage: pipeline_times [--pipeline-cache FILE] REFLECTION.json MODULE.spv ...");
    };
    let started = Instant::now();
    let reflection = read_baked(
        &std::fs::read_to_string(reflection).with_context(|| format!("reading {reflection}"))?,
    );
    let mut entries = Vec::new();
    let mut spirv = Vec::new();
    for module in modules {
        let path = std::path::Path::new(module);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.split('.').next())
            .with_context(|| format!("{module} is not named <entry>.<format>.spv"))?;
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        entries.push(Entry {
            name,
            label: name,
            workgroup: (1, 1, 1),
        });
        spirv.push(std::fs::read(path).with_context(|| format!("reading {module}"))?);
    }
    let entries: &'static [Entry] = Box::leak(entries.into_boxed_slice());
    let spirv: Vec<&[u8]> = spirv.iter().map(Vec::as_slice).collect();
    let storage_format = |name: &str| match name {
        "ids" => ID_FORMAT,
        _ => wgpu::TextureFormat::Rgba8Unorm,
    };
    let sets: std::collections::BTreeSet<u32> = reflection
        .bindings
        .iter()
        .map(|binding| binding.set)
        .collect();
    let labels: Vec<&str> = sets.iter().map(|_| "set").collect();

    let device = Device::open(
        caustics_host::device::instance(),
        None,
        &reflection.bindings,
        "pipeline-times",
    )?;
    let opened = started.elapsed();

    let built = Instant::now();
    match cache_file {
        None => {
            let (built, alone): (&[Entry], &[&[u8]]) = if parallel {
                (&[], &[])
            } else {
                (entries, &spirv)
            };
            let program = Program::new(
                &device.device,
                reflection.bindings.clone(),
                &labels,
                "pipeline-times-layout",
                &storage_format,
                built,
                alone,
            )?;
            let pipelines: Vec<wgpu::ComputePipeline> = if parallel {
                std::thread::scope(|scope| {
                    let workers: Vec<_> = entries
                        .iter()
                        .zip(&spirv)
                        .map(|(entry, spv)| {
                            let device = &device.device;
                            let layout = program.pipeline_layout();
                            scope.spawn(move || {
                                caustics_host::program::build_pipelines(
                                    device,
                                    layout,
                                    std::slice::from_ref(entry),
                                    &[spv],
                                )
                            })
                        })
                        .collect();
                    workers
                        .into_iter()
                        .map(|worker| worker.join().expect("a build thread panicked"))
                        .collect::<Result<Vec<_>>>()
                })?
                .into_iter()
                .flatten()
                .collect()
            } else {
                (0..entries.len())
                    .map(|index| program.pipeline(index).clone())
                    .collect()
            };
            if dispatch {
                dispatch_each(&device, &program, &pipelines, entries, sets.len() as u32);
            }
        }
        Some(file) => with_cache(
            &device,
            &reflection,
            &labels,
            &storage_format,
            entries,
            &spirv,
            &file,
        )?,
    }
    let built = built.elapsed();
    println!(
        "device open {:.3} s, pipelines {:.3} s, total {:.3} s",
        opened.as_secs_f64(),
        built.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Each pipeline dispatched once, one workgroup, twice over: the first
/// dispatch carries whatever compile the driver deferred to first use.
fn dispatch_each(
    device: &Device,
    program: &Program,
    pipelines: &[wgpu::ComputePipeline],
    entries: &[Entry],
    sets: u32,
) {
    let gpu = &device.device;
    let mut buffers = Vec::new();
    let mut views = Vec::new();
    let sampler = gpu.create_sampler(&wgpu::SamplerDescriptor::default());
    for binding in program.bindings() {
        use punchcutter_slang::BindingKind;
        match binding.kind {
            BindingKind::Uniform => buffers.push((
                binding.name.clone(),
                gpu.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&binding.name),
                    size: 64 << 10,
                    usage: wgpu::BufferUsages::UNIFORM,
                    mapped_at_creation: false,
                }),
            )),
            BindingKind::Storage { stride, .. } => buffers.push((
                binding.name.clone(),
                gpu.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&binding.name),
                    size: (4u64 << 20).max(u64::from(stride.unwrap_or(0)) * 4),
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                }),
            )),
            BindingKind::StorageTexture { .. } | BindingKind::Texture { .. } => {
                let (format, usage, layers) = match binding.kind {
                    BindingKind::StorageTexture { .. } => (
                        if binding.name == "ids" {
                            ID_FORMAT
                        } else {
                            wgpu::TextureFormat::Rgba8Unorm
                        },
                        wgpu::TextureUsages::STORAGE_BINDING,
                        1,
                    ),
                    BindingKind::Texture { array } => (
                        wgpu::TextureFormat::Rgba8Unorm,
                        wgpu::TextureUsages::TEXTURE_BINDING,
                        if array { 2 } else { 1 },
                    ),
                    _ => unreachable!(),
                };
                let texture = gpu.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&binding.name),
                    size: wgpu::Extent3d {
                        width: 64,
                        height: 64,
                        depth_or_array_layers: layers,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                });
                let dimension = if layers > 1 {
                    wgpu::TextureViewDimension::D2Array
                } else {
                    wgpu::TextureViewDimension::D2
                };
                views.push((
                    binding.name.clone(),
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        dimension: Some(dimension),
                        ..Default::default()
                    }),
                ));
            }
            BindingKind::Sampler => {}
        }
    }
    let groups: Vec<wgpu::BindGroup> = (0..sets)
        .map(|set| {
            let entries = caustics_host::program::group_entries(program.bindings(), set, |name| {
                if let Some((_, buffer)) = buffers.iter().find(|(n, _)| n == name) {
                    buffer.as_entire_binding()
                } else if let Some((_, view)) = views.iter().find(|(n, _)| n == name) {
                    wgpu::BindingResource::TextureView(view)
                } else {
                    wgpu::BindingResource::Sampler(&sampler)
                }
            });
            gpu.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pipeline-times-group"),
                layout: program.layout(set),
                entries: &entries,
            })
        })
        .collect();
    for round in ["first", "second"] {
        for (index, entry) in entries.iter().enumerate() {
            let started = Instant::now();
            let mut encoder = gpu.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipelines[index]);
                for (set, group) in groups.iter().enumerate() {
                    pass.set_bind_group(set as u32, group, &[]);
                }
                pass.dispatch_workgroups(1, 1, 1);
            }
            device.queue.submit([encoder.finish()]);
            gpu.poll(wgpu::PollType::wait_indefinitely())
                .expect("the device is lost");
            println!(
                "pipeline {}: {round} dispatch in {:.3} s",
                entry.name,
                started.elapsed().as_secs_f64()
            );
        }
    }
}

/// The same pipelines against a wgpu pipeline cache read from and written
/// back to `file`. The device has to have been opened with
/// `PIPELINE_CACHE`, which [`Device::open`] does not ask for, so this opens
/// its own on the same adapter.
fn with_cache(
    device: &Device,
    reflection: &punchcutter_slang::Reflection,
    labels: &[&str],
    storage_format: &dyn Fn(&str) -> wgpu::TextureFormat,
    entries: &[Entry],
    spirv: &[&[u8]],
    file: &std::path::Path,
) -> Result<()> {
    if !device
        .adapter
        .features()
        .contains(wgpu::Features::PIPELINE_CACHE)
    {
        bail!("this adapter has no PIPELINE_CACHE");
    }
    let mut limits = wgpu::Limits::default();
    limits.max_storage_buffers_per_shader_stage = device
        .adapter
        .limits()
        .max_storage_buffers_per_shader_stage
        .max(limits.max_storage_buffers_per_shader_stage);
    let (cached, _queue, _clock) = punchcutter_core::gpu::open_device(
        &device.adapter,
        &wgpu::DeviceDescriptor {
            label: Some("pipeline-times-cached"),
            required_features: device.features | wgpu::Features::PIPELINE_CACHE,
            required_limits: limits,
            ..Default::default()
        },
    )?;
    let data = std::fs::read(file).ok();
    println!(
        "pipeline cache: {} bytes read",
        data.as_ref().map_or(0, Vec::len)
    );
    // SAFETY: the data is what `get_data` wrote on this machine, or nothing;
    // `fallback` takes an empty cache over one the driver refuses.
    let cache = unsafe {
        cached.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
            label: Some("pipeline-times-cache"),
            data: data.as_deref(),
            fallback: true,
        })
    };
    let layouts: Vec<wgpu::BindGroupLayout> = (0..labels.len() as u32)
        .map(|set| {
            caustics_host::program::create_layout(
                &cached,
                "set",
                &reflection.bindings,
                set,
                storage_format,
            )
        })
        .collect();
    let layout = cached.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-times-layout"),
        bind_group_layouts: &layouts.iter().map(Some).collect::<Vec<_>>(),
        immediate_size: 0,
    });
    for (entry, spv) in entries.iter().zip(spirv) {
        let started = Instant::now();
        // SAFETY: the module is the build script's own compiler output, as
        // in `passthrough_pipeline`.
        let module = unsafe {
            cached.create_shader_module_passthrough(wgpu::ShaderModuleDescriptorPassthrough {
                label: Some(entry.label),
                entry_points: std::borrow::Cow::Owned(vec![wgpu::PassthroughShaderEntryPoint {
                    name: std::borrow::Cow::Borrowed(entry.name),
                    workgroup_size: entry.workgroup,
                }]),
                spirv: Some(wgpu::util::make_spirv_raw(spv)),
                ..Default::default()
            })
        };
        let _pipeline = cached.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry.label),
            layout: Some(&layout),
            module: &module,
            entry_point: Some(entry.name),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: Some(&cache),
        });
        println!(
            "pipeline {} (cached): built in {:.3} s",
            entry.name,
            started.elapsed().as_secs_f64()
        );
    }
    if let Some(data) = cache.get_data() {
        std::fs::write(file, &data)?;
        println!("pipeline cache: {} bytes written", data.len());
    }
    Ok(())
}
