# caustics-sysmon

The bar's sampling sidecar. It reads `/proc`, hwmon and drm sysfs once a tick
and writes **one binary frame per tick to stdout**: the exact bytes the bar
uploads to its samples buffer and the caustic draws from. It is a separate
process on purpose (*Why a separate process*, below).

```
caustics-sysmon [--refresh <ms>] [<chart>…]
```

The bar runs it with **no arguments at all**. Everything below is the
sidecar's own business; the bar has no configuration file and never passes
any of it on.

## Charts

One word per chart, in the order the frame will carry them:

| word | what it measures | series | unit (the frame's `ceiling`) |
| --- | --- | --- | --- |
| `cpu` | busy fraction over the tick, all CPUs, from `/proc/stat` | 1 | 0..1 (`1`) |
| `cpu-temp` | CPU package temperature from hwmon | 1 | °C (`100`) |
| `mem` | `(MemTotal − MemAvailable) / MemTotal` | 1 | 0..1 (`1`) |
| `net` | bytes/s down and up, summed over every interface but `lo` | 2 | B/s (`0` = auto) |
| `net:<iface>` | the same for one interface, e.g. `net:wlan0` | 2 | B/s (`0`) |
| `disk` | bytes/s read and written, summed over whole disks | 2 | B/s (`0` = auto) |
| `disk:<name>` | the same for one device, e.g. `disk:nvme0n1` | 2 | B/s (`0`) |
| `gpu` | busy percent: AMD first, NVIDIA second when both exist | 1 or 2 | 0..100 (`100`) |
| `gpu:amd` | the AMD device alone | 1 | 0..100 (`100`) |
| `gpu:nvidia` | the NVIDIA device alone | 1 | 0..100 (`100`) |

`disk` counts whole disks only. Partitions would double-count their disk, and
`zram*`, `loop*`, `dm-*`, `sr*`, `ram*` and `md*` are not disks. `net` and
`disk` sum because "all interfaces" is a sampling question, not a drawing one.

**With no chart words** the sidecar samples `$CAUSTICS_SYSMON_CHARTS`
(whitespace-separated) if that is set and non-empty, and failing that its
compiled-in default:

```
cpu cpu-temp mem net disk gpu
```

So the way to change what the bar shows is a systemd drop-in on the *bar's*
unit that sets the sidecar's environment:

```ini
# ~/.config/systemd/user/caustics-bar.service.d/charts.conf
[Service]
Environment=CAUSTICS_SYSMON_CHARTS=cpu cpu-temp mem net:wlan0 gpu
```

An unrecognised word is not an error: it becomes a row of gaps that still
occupies its index, so the rows after it do not shift. `--refresh` takes
100..600000 ms and defaults to 1000. A malformed option or number exits **2**;
`--help` prints the usage on stderr and exits 0.

## The frame

First a `Hello` frame carrying the ABI hash this binary was built against (the
bar refuses a sidecar from another build on that first frame, docs/abi.md
*Hello*); then one `Samples` frame per tick, written through
`caustics_bar_abi::wire::write` with a single `write_all`, and nothing else on
stdout ever. docs/abi.md, *The wire* is the normative description and
`bar/abi/src/wire.rs` the code:

- a 12-byte `WireHeader`: the magic `CAUS`, the kind, and the body's length,
  so a reader can skip a frame of a kind it does not know;
- then the `Samples` body: three counts and a reserved word, eight 16-byte
  `ChartRow`s, then `8 × 2 × 128` little-endian floats. **8336 bytes**, which
  is what the bar uploads to its `samples` buffer unchanged.

In the body:

- `t` is `CLOCK_MONOTONIC` seconds at the tick.
- `sample_count` is how many columns are valid, counting back from the last
  one: the history is oldest-first and **right-aligned**.
- Every row carries its `kind` (`CHART_CPU`, `CHART_TEMP`, `CHART_MEM`,
  `CHART_NET`, `CHART_DISK`, `CHART_GPU`, or `CHART_OTHER`), a fact about
  what the row measures. What a chart of that kind *looks* like is the
  caustic's business, not this program's.
- A **negative value is a gap**: a device asleep, a rate with no delta yet, a
  sensor that is not there. The first tick of every rate is a gap.

Any program that can write little-endian floats can be this program. A
third-party sidecar writes the same frames, and `caustics_bar_abi::wire` is the
code that does it. stdin is read the same way, and reaching EOF on it is this
program's notice that the bar is gone.

stderr is the witness channel and inherits the bar's journal: one line per
discovery at start, one per guard transition, one per chart it cannot
provide. The sidecar **exits 0 when stdin reaches EOF**. The bar holds the
write end, so the sidecar dies with it.

## Why a separate process

The bar's membership test (docs/architecture.md, *Sidecars*): a feeder lives
in the bar's process only if it owns a Wayland object on the bar's connection
or writes the bar's shm or GPU memory. This owns neither. It reads kernel
files and it dlopens NVML, and NVML has a hardware side effect, which is the
next section.

## The NVML lease

`Nvml::init` loads `libnvidia-ml.so.1`, which opens `/dev/nvidia*`. Opening
those nodes resumes a runtime-suspended discrete GPU, and holding them open
keeps it from suspending again. On a laptop that is watts, so the handle is
held under a lease:

1. The handle is **taken only while `…/power/runtime_status` reads
   `active`**: the card is already awake for somebody else's reasons. Any
   other value, or a file that cannot be read, samples as a gap and touches
   nothing.
2. While it is held, each sample asks NVML how many graphics and compute
   clients the device has (this process holds no context and is not one).
   Once the device has gone **three seconds without a client** the handle is
   given back, which closes `/dev/nvidia*` and lets runtime PM suspend the
   card. When NVML cannot count clients, a busy percent above zero counts as
   use.
3. After a release the lease **rests**: it reads `runtime_status` without
   touching NVML. The rest ends as soon as the card reads anything but
   `active` (it suspended, as intended, and rule 1 rules again), or after the
   card's `power/autosuspend_delay_ms` plus three seconds have passed with it
   still `active`, which means something else is keeping it awake and the
   lease may be taken again. An unreadable delay (some drivers answer EIO)
   is assumed to be 5000 ms; a rest never lasts more than two minutes.
4. An `Nvml::init` that fails while the gate says `active` (no driver, no
   library) is one stderr line and gaps for the rest of the run. It is never
   retried.

On a machine where something else is already using the discrete GPU (a
compositor, a browser) the client count never reaches zero and the handle
simply stays held. That is correct, and it is *this* process's fd table that
shows it, never the bar's:

```
$ ls -l /proc/$(pgrep -x caustics-sysmon)/fd | grep nvidia
… 10 -> /dev/nvidia0
… 11 -> /dev/nvidia0
… 12 -> /dev/nvidia-caps/nvidia-cap2
… 13 -> /dev/nvidia0
… 9  -> /dev/nvidiactl
```

## Running it by hand

It is a sampler, not a session, and it is safe to run anywhere.

```bash
# Six charts, twice a second, for five seconds: 12 + 8336 bytes a frame,
# after one Hello.
timeout 5 caustics-sysmon --refresh 500 | xxd | head
```
