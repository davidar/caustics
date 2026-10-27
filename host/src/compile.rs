//! Pipeline creation off the host's loop: a queue of compile jobs, the
//! worker threads that drain it, and the results they hand back.
//!
//! A pipeline built from a caustic's SPIR-V costs ~10 ms with a warm driver
//! cache and seconds without one (the shaper's entry points, ~1.5 MB of
//! SPIR-V each, are most of it; a software rasteriser takes minutes). A host
//! that built them on its own thread drew nothing, read no Wayland event and
//! no sidecar pipe until the last one was done, so here they are built
//! elsewhere and handed back one at a time.
//!
//! - **One job, one pipeline**, in the order they were queued, which is the
//!   host's order of need ([`crate::program::Program::new`] takes it).
//! - **Built means built and warmed.** A software rasteriser (lavapipe) only
//!   lowers the module when the pipeline is created and compiles it at the
//!   pipeline's first dispatch: 3 s and then 13 s for the canvas's shaper.
//!   So each pipeline is dispatched once on the worker, one workgroup over
//!   scratch bind groups of its own ([`Warm`]), and waited out, before it is
//!   handed back; the host's first real dispatch then finds it compiled. A
//!   driver that compiles at creation (RADV) pays microseconds for it.
//! - **Nothing is submitted behind a warming dispatch.** lavapipe runs its
//!   queue on one thread and compiles there, so a frame submitted while the
//!   shaper warms waits the whole compile (46 s, measured) in the host's
//!   device wait. [`Compiler::warming`] says one is in flight, and a host
//!   draws nothing until it has landed; its loop stays live meanwhile, and
//!   the landing's wake brings the frame. A frame submitted in the instant
//!   between that check and a worker's submit waits once; nothing is lost.
//! - **Each worker holds its own validation error scope.** wgpu's error
//!   scopes are per thread (a scope pushed on one thread never sees another's
//!   errors), so a rejected module on a worker is that job's result and
//!   nothing else's, and the host's own thread keeps no scope at all, as
//!   before: anything it does wrong is still the device's uncaptured error.
//! - **The result goes back through a channel and a wake.** The channel is
//!   this module's; the wake is the host's (a calloop ping in both hosts),
//!   called once per result, so the host's loop learns there is something to
//!   install without polling. [`Compiler::drain`] takes what has landed.
//! - **A job nobody wants any more is never built.** The owner withdraws a
//!   generation's queued jobs ([`Compiler::withdraw`]) when a newer one
//!   supersedes it; one already on a worker finishes, and its result is
//!   dropped by the owner, which knows it is stale.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use punchcutter_core::gpu::passthrough_pipeline;

/// What a host is told when a result is waiting: called from a worker
/// thread, once per pipeline built or refused.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// What a worker warms a pipeline with: one bind group per set, over
/// resources nothing else binds, all zero.
///
/// Zero is a dispatch that does nothing a caustic can see: no surface (a
/// draw's image is 1x1 and its size words are 0), no commands, no blocks to
/// shape, no rows; and whatever a pass writes lands in buffers no frame
/// reads. Every storage buffer holds at least one element of its reflected
/// stride, so an entry that reads element 0 of its state reads zeros.
pub struct Warm {
    pub queue: wgpu::Queue,
    pub groups: Vec<wgpu::BindGroup>,
}

/// The warming dispatches submitted and not yet waited out.
struct InFlight(AtomicUsize);

/// One pipeline to build.
pub struct Job {
    /// The program generation this belongs to (the owner's numbering).
    pub generation: u64,
    /// The owner's slot for the result.
    pub slot: usize,
    /// The pipeline's debug label and the module's entry point name.
    pub label: &'static str,
    pub entry: &'static str,
    pub workgroup: (u32, u32, u32),
    pub spirv: Vec<u8>,
}

/// One pipeline built, or refused.
pub struct Built {
    pub generation: u64,
    pub slot: usize,
    /// How long the device took over it, on the worker: the module and the
    /// pipeline, not the time it waited in the queue.
    pub took: Duration,
    /// The pipeline, or the device's reason for refusing it.
    pub result: Result<wgpu::ComputePipeline, String>,
}

struct Queue {
    jobs: VecDeque<Job>,
    /// Set when the owner is gone: the workers finish what they hold and
    /// stop.
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    work: Condvar,
    warming: InFlight,
}

/// The queue and its workers, for one device and one pipeline layout.
pub struct Compiler {
    shared: Arc<Shared>,
    results: Receiver<Built>,
    workers: usize,
}

impl Compiler {
    /// Start `workers` threads (at least one) building against `device` and
    /// `layout`, calling `wake` after each result.
    pub fn start(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        warm: Warm,
        workers: usize,
        wake: Wake,
    ) -> Compiler {
        let warm = Arc::new(warm);
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                closed: false,
            }),
            work: Condvar::new(),
            warming: InFlight(AtomicUsize::new(0)),
        });
        let (send, results) = mpsc::channel();
        let workers = workers.max(1);
        for n in 0..workers {
            let shared = Arc::clone(&shared);
            let device = device.clone();
            let layout = layout.clone();
            let send: Sender<Built> = send.clone();
            let wake = Arc::clone(&wake);
            let warm = Arc::clone(&warm);
            std::thread::Builder::new()
                .name(format!("compile-{n}"))
                .spawn(move || work(&shared, &device, &layout, &warm, &send, &*wake))
                .expect("a compile worker thread could not be started");
        }
        Compiler {
            shared,
            results,
            workers,
        }
    }

    /// How many threads build.
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Whether a warming dispatch is on the device's queue now: anything the
    /// host submits would wait behind it.
    pub fn warming(&self) -> bool {
        self.shared.warming.0.load(Ordering::Acquire) > 0
    }

    /// Queue jobs behind whatever is queued, in the order given.
    pub fn submit(&self, jobs: impl IntoIterator<Item = Job>) {
        let mut queue = self.shared.queue.lock().expect("the compile queue");
        queue.jobs.extend(jobs);
        drop(queue);
        self.shared.work.notify_all();
    }

    /// Drop every queued job of `generation` that no worker has started.
    pub fn withdraw(&self, generation: u64) -> usize {
        let mut queue = self.shared.queue.lock().expect("the compile queue");
        let before = queue.jobs.len();
        queue.jobs.retain(|job| job.generation != generation);
        before - queue.jobs.len()
    }

    /// Every result that has landed, without waiting.
    pub fn drain(&self) -> Vec<Built> {
        self.results.try_iter().collect()
    }

    /// The next result, waiting for it: for a host that runs its pipelines
    /// synchronously (the device tests). `None` only if every worker is gone.
    pub fn wait(&self) -> Option<Built> {
        self.results.recv().ok()
    }
}

impl Drop for Compiler {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.shared.queue.lock() {
            queue.closed = true;
            queue.jobs.clear();
        }
        self.shared.work.notify_all();
    }
}

/// One worker: take the front job, build and warm it inside this thread's
/// own error scope, send it back, wake the host.
fn work(
    shared: &Shared,
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    warm: &Warm,
    send: &Sender<Built>,
    wake: &(dyn Fn() + Send + Sync),
) {
    loop {
        let job = {
            let mut queue = shared.queue.lock().expect("the compile queue");
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                queue = shared.work.wait(queue).expect("the compile queue");
            }
        };
        let started = Instant::now();
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = passthrough_pipeline(
            device,
            layout,
            job.label,
            job.entry,
            &job.spirv,
            job.workgroup,
        );
        shared.warming.0.fetch_add(1, Ordering::AcqRel);
        warm_up(device, warm, &pipeline, job.label);
        shared.warming.0.fetch_sub(1, Ordering::AcqRel);
        let result = match pollster::block_on(scope.pop()) {
            Some(err) => Err(format!("{err}")),
            None => Ok(pipeline),
        };
        let built = Built {
            generation: job.generation,
            slot: job.slot,
            took: started.elapsed(),
            result,
        };
        if send.send(built).is_err() {
            // The owner is gone; so is the reason to build.
            return;
        }
        wake();
    }
}

/// One workgroup of `pipeline` over the scratch groups, submitted and waited
/// for: the dispatch a driver that compiles late compiles at.
fn warm_up(device: &wgpu::Device, warm: &Warm, pipeline: &wgpu::ComputePipeline, label: &str) {
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: None,
        });
        for (set, group) in warm.groups.iter().enumerate() {
            pass.set_bind_group(set as u32, group, &[]);
        }
        pass.set_pipeline(pipeline);
        pass.dispatch_workgroups(1, 1, 1);
    }
    let index = warm.queue.submit(Some(encoder.finish()));
    if let Err(err) = device.poll(wgpu::PollType::Wait {
        submission_index: Some(index),
        timeout: None,
    }) {
        tracing::warn!("pipeline {label}: the warming dispatch was not waited out: {err}");
    }
}

/// A duration as a witness line wants it: milliseconds under a second,
/// seconds above.
pub fn human(took: Duration) -> String {
    let secs = took.as_secs_f64();
    if secs < 0.01 {
        format!("{:.1} ms", secs * 1000.0)
    } else if secs < 1.0 {
        format!("{:.0} ms", secs * 1000.0)
    } else {
        format!("{secs:.2} s")
    }
}
