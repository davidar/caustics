# slangc 2026.14.1: a segfault on `[numthreads]` placed on a called helper

An editing slip in a large caustic inserted a helper function *between* an
entry point's attribute block and the entry point itself, leaving
`[shader("compute")]` and `[numthreads(8,8,1)]` on a `float3`-returning
helper. slangc segfaulted instead of diagnosing it. The file was minimised by
line-level delta debugging (about 700 lines to 110 automatically, then to 12
by hand from the surviving ingredients).

## Minimal reproduction (12 lines, exit 139)

```slang
RWTexture2D<float4> dst;

[numthreads(8, 8, 1)]
float helper()
{
    return 1.0;
}

void entry_point()
{
    dst[uint2(0, 0)] = float4(helper(), 0.0, 0.0, 1.0);
}
```

```
slangc repro.slang -target spirv -entry entry_point -stage compute -o out.spv
→ Segmentation fault (core dumped)
```

Control: the identical file with `[numthreads(8, 8, 1)]` moved onto
`entry_point` compiles cleanly. All three ingredients are required; drop any
one and it compiles or errors normally:

1. `[numthreads]` on a **non-entry** function (`[shader("compute")]` is not
   needed; `numthreads` alone suffices);
2. that function **called** from the actual `-entry` function;
3. a **resource access** in the entry (the `RWTexture2D` store; without the
   texture it compiles fine).

`-profile glsl_450` is irrelevant in the minimal form; it appeared
load-bearing in the original context by coincidence.

## Where it crashes

Not in Slang's front end. The core dump puts it in **spirv-tools'
AggressiveDCEPass**, bundled in `libslang-glslang`:

```
#0 spvtools::opt::AggressiveDCEPass::AddOperandsToWorkList(...)  [lambda in ForEachInId]
#1 spvtools::opt::Instruction::ForEachInId(...)
#2 spvtools::opt::AggressiveDCEPass::AddOperandsToWorkList(...)
```

Two defects stacked:

- **slang**: the front end accepts `[numthreads]` on a non-entry function
  without diagnosis and emits malformed-but-parseable SPIR-V for the module
  (plausibly an execution-mode attachment on a function that is not an
  `OpEntryPoint` target).
- **spirv-tools**: ADCE dereferences something null while walking that IR
  instead of rejecting it.

The friendlier fix is slang-side: attribute placement validation (reject or
warn on shader-stage and `numthreads` attributes on functions not used as
entry points). The spirv-tools hardening is the deeper fix.

## Status

- No workaround is needed in this tree: the trigger was an editing mistake,
  not a legitimate construct. A comment in `caustics/bar.slang` points here
  in case the pattern recurs.
- Not yet reported upstream (slang / SPIRV-Tools). This note is the report
  draft. The minimiser's method: an exit-139-specific oracle, greedy
  chunk-sweep ddmin at granularities 64 down to 1, then hand-derivation from
  the 1-minimal survivors.
