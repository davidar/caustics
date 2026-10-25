# A wire that grows

The wire today is in docs/abi.md, *The wire*: a 12-byte header, a body that
is a fixed-layout struct, and one hash over the whole contract that a
sidecar's first frame must match or the sidecar is killed. It is the right
contract for a set built from one tree and installed whole, and it has one
limit, stated in docs/roadmap.md, *Out-of-tree sidecars*: a sidecar from
another tree must be built against the exact revision the installed set was,
and any change to any struct in the tree breaks it. This document designs the
contract that replaces the hash with a rule, so that two ends built from
different trees work together to the extent of what they share. It is a
design, not yet built; when it is, the sections of docs/abi.md it names are
rewritten and this document becomes the reasoning behind them.

Nothing here touches the buffer contract (docs/abi.md, *The buffer*). The
caustic goes on reading the layouts of the tree it was compiled from. The
wire is the only place two builds meet.

## What the hash is for

docs/abi.md, *The ABI hash*, records why there is one number for the whole
contract and not a version per kind: a bar once ran beside a sidecar from an
older build which wrote a well-formed `Bytes` frame under a buffer number
that the newer tree had, in the meantime, given to a different buffer. The
frame's shape was right and its meaning was wrong, and no per-kind version
word could have said so. The hash says so by refusing everything that is
not the same tree.

That refusal is the whole cost. A comment edit is a new set. A sidecar that
only writes `Rows`, a frame whose layout has not changed in months, is
refused by a bar that changed a chart struct. A sidecar from another tree is
impossible rather than merely careful.

The incident has a narrower cure than the hash. The frame meant something
different because a **number was reused**: the buffer number changed hands.
If no name on the wire is ever given a second meaning, a well-formed frame
from an older or newer build means what its writer meant, and the only thing
the reader has to handle is a struct that is longer or shorter than its own.
That is a rule about editing the tree, enforceable by a test in the tree,
and it replaces a check between two trees at run time.

## The rule

**A name on the wire is given once and never reused, and a record only
grows at its end.** Four things are names:

- **Kind numbers** (`WIRE_*`). Appended; a retired kind keeps its number and
  is skipped like an unknown one.
- **Buffer numbers** (`BUF_*`), and every other value a frame carries that
  selects something on the other side: applet values, row roles, action
  kinds, request kinds, canvas commands, format codes. Appended; a retired
  value keeps its number and means "nothing" to a reader that no longer has
  the thing.
- **Flag bits.** Appended; a retired bit is written zero and read by nobody.
- **Field positions** in every wire struct. A field is never moved, never
  retyped, never removed. A retired field keeps its offset, is written zero
  and read by nobody. New fields go at the end, or into a word the struct
  already reserved for them (`_rsvdN` in docs/abi.md, *Layout rules*, keeps
  exactly this meaning).

A new field, value or bit has to mean, when it reads as **zero**, what the
frame meant before it existed, because zero is what an older writer's frame
reads as once the reader pads it (*The copy*). This is the one design
constraint the rule puts on new fields, and it has a known trap: `NO_INDEX`
is all ones, not zero, so an index added to an existing struct cannot use it
for "none". An added optional index is stored plus one, with zero as none, or
is paired with a flag bit that says it is present.

When a kind's meaning has to change in a way no appended field can express,
it gets a **new kind number** and the old one stays readable. That is the
escape hatch, and it is expected to be rare: in the wire's history every
change but the buffer-number incident was an added field, an added value or
an added kind.

### The ledger

The rule is enforced where the hash was computed: at build time, in the
contract crate. A **ledger**, a plain text file committed beside the wire's
declaration, lists every kind with its head struct and stride (*One shape*)
field by field (name, offset, type), every named value and every flag bit.
A test regenerates the ledger from the declarations and compares: every
line in the committed ledger must still be present, unchanged, at the same
position; new lines may only follow the old ones within their struct or
their value set. Any other difference fails the build of the tree, which is
the same moment the hash would have changed. The ledger is the layout test
docs/abi.md, *The two contracts*, already runs against Slang's reflection,
with memory.

The ledger is also the document a sidecar in another tree is written
against. It is generated from the same tables the Slang modules are rendered
from, so there is still one declaration.

## One shape

Every variable-size frame the wire has is the same shape: a **head** struct,
then a **table** of `count` records of one stride, then an **arena** of
bytes the head and the records point into. `Rows` is a header, `row_count`
rows and a blob of labels. `Image`, `Bytes` and `Module` are a header and a
blob, with an empty table. The fixed-size kinds are a head alone. The design
makes the shape explicit, so that the one piece of code that copies a frame
can handle a struct it has never seen a longer or shorter version of.

```
struct WireHeader {     // 24 B
    uint magic;         // 0   a new magic, so the old reader rejects the new frame at its first word
    uint kind;          // 4   WIRE_*, as today
    uint len;           // 8   body bytes following this header
    uint head;          // 12  bytes of the head struct at the start of the body
    uint count;         // 16  records in the table after the head
    uint stride;        // 20  bytes per record; 0 when count is 0
}
// body: head bytes, then count × stride bytes, then len − head − count × stride bytes of arena
```

The reader checks, before it knows the kind: `head ≤ len`, `count × stride ≤
len − head` without overflow, `stride` a multiple of 4 and `head` a multiple
of 4 (every wire word is 32 bits; docs/abi.md, *Layout rules*), and `len`
under the cap. A frame that fails is a broken writer, as a bad length is
today. Then, by kind, the reader knows its **own** head size and stride for
that kind, compiled in, and the per-kind caps it already has (`MAX_ROWS`,
the arena a region holds, `IMAGE_MAX_PX`). An unknown kind is skipped whole,
as today.

The reader rules in docs/abi.md, *Framing*, otherwise stand: a clean close
is EOF exactly at a header, a wrong magic is an error, a body is read as it
arrives, a kind that is known but whose counts do not fit its caps is
skipped with one witness.

## The copy

The reason the wire is fixed-layout structs and not a serialisation format
is the rule in docs/architecture.md, *Sidecars*, that the host copies bytes
and parses nothing: a frame body goes from the pipe to a GPU buffer and the caustic
reads it there. The design keeps that rule by making the copy a function of
the six header words and two compiled-in sizes, and nothing in the body:

- **The head**: copy `min(head, own_head)` bytes, zero-fill to `own_head`.
- **The table**: for each of `count` records, copy `min(stride, own_stride)`
  bytes, zero-fill to `own_stride`. When the strides are equal this is one
  copy; when they differ it is a strided copy of a few hundred records at
  most.
- **The arena**: copy verbatim, within the region's cap; truncate with the
  witness the reader already has.

Both directions of skew fall out. A newer writer's longer struct is
truncated to what this reader knows; by the rule, what it loses is fields it
has no meaning for. An older writer's shorter struct is padded with zero; by
the rule, zero means what the frame meant then. The caustic reads the
layout it was compiled with, which is the reader's own, and never sees a
sender's. Nothing on the GPU changes for this design.

The same copy serves the other direction. A host writes `Request` and
`Action` frames to a sidecar's stdin, and the sidecar reads them with the
same reader from the same crate, so a sidecar built from an older tree pads
a newer host's requests and a sidecar from a newer tree truncates an older
host's. The reader is one function; both ends have it.

## Hello

`Hello` stays a sidecar's first frame and the one thing, with the header,
that is stable forever. It stops being a gate on the whole tree and becomes
a declaration:

```
struct HelloHead {      // 16 B
    uint name_len;      // 0   the name in the arena, ≤ MAX_NAME_LEN
    uint set_lo;        // 4   the writer's tree fingerprint, for the log line only
    uint set_hi;        // 8
    uint flags;         // 12  reserved: written 0, read by nobody
}
struct HelloKind {      // 12 B, one per kind the sidecar writes
    uint kind;
    uint head;          // the head size it writes
    uint stride;        // the stride it writes, 0 for a kind with no table
}
// table: the kinds this sidecar writes; arena: the name
```

The host kills a child whose first frame is not a `Hello`, whose name is not
the one it spawned, or whose magic is not this wire's. It does not kill a
child for its fingerprint. It logs, once per kind, where the child's sizes
differ from its own, in words a reader of the journal can act on: which
sidecar, which kind, longer or shorter, and that the difference is ignored
or padded. That line is the whole visible cost of running mismatched builds,
and it is the line the hash used to replace with a refusal.

## What this does not change

- **Caps are the reader's policy**, as today: how many rows a frame may
  carry, how many bytes a region holds, how large a picture may be. Raising
  `MAX_ROWS` (docs/roadmap.md, *Wire and harness*) stops being a wire
  version and becomes a constant, since a count is not a layout.
- **One set is still how the tree is installed.** `bar/install.sh` installs
  the host and every sidecar together, and the fingerprint still names the
  set. The difference is that a mismatched pair now works to the extent of
  shared meaning instead of refusing, which is what a sidecar built
  elsewhere needs and what a half-finished upgrade on a developer's machine
  gets for free.
- **Framing stays the writer's responsibility**: header and body in one
  write, every frame whole-state.
- **The buffer contract** and the rendered Slang modules are untouched. The
  caustic's view of a frame is its own tree's struct, filled by the copy.
- **Strings** stay bytes in an arena that the caustic shapes; nothing here
  introduces text parsing to the host.

## What it is not

It is not a serialisation library, and the reason to design it rather than
adopt one is in the shape of the data: every frame is a table of 32-bit
words with an arena, and the GPU reads that layout directly. A
pointer-based format would need a reader on the GPU and a translation step
on the host, which is the parsing the architecture forbids. The one idea
borrowed from those formats is the one that matters, that a reader pads a
short record with zero and ignores a long one's tail, so that fields only
ever accumulate.

## Building it

One package, after the current canvas work, since it changes every writer
and the host's copy paths at once:

1. The header grows to 24 bytes under a new magic; `wire::write` takes head,
   table and arena as three slices; every frame type's builder produces
   them. The reader validates the shape, reads the body incrementally, and
   hands the copy routine a frame it has already bounded.
2. The host's per-kind copies (rows into a region, samples into their slot,
   the canvas head into `facts`) become the one copy with per-kind sizes.
3. `Hello` as above; the fingerprint becomes a log line.
4. The ledger file and its test; the `ABI_HASH` build step is retired in
   favour of the fingerprint it already computes, now informational.
5. docs/abi.md: *Framing*, *Hello*, *The ABI hash* (replaced by the rule and
   the ledger), *Layout rules* (the `_rsvdN` sentence becomes the rule),
   *Writing a sidecar* (pin the ledger entries you use, not a revision);
   docs/roadmap.md, *Out-of-tree sidecars*, loses its pin item.
6. The gate: the full nested run, plus one step that proves the point, a
   sidecar built from the previous set talking to the new host and the
   reverse, both logging the skew line and both drawing.

## Open questions

- **How far back does a reader pad?** A struct that grew by two reserved
  words over a year is one thing; a kind whose stride doubled is another.
  The copy handles both; the question is whether a very old writer's frame
  should be skipped with a witness past some ratio, or always padded. The
  design says always padded, since zero-filled fields are correct by the
  rule, and leaves a cap as policy if one is ever wanted.
- **Does the host ever tell a sidecar its own sizes?** A sidecar that wants
  to know whether a field it writes is read has no way to ask. A request
  kind that answers with the host's ledger entry for a kind can be added
  later under the rule itself; nothing is lost by not designing it now.
- **The ledger's form.** Generated text is the simplest thing a test can
  diff and a reader can open; a structured file would let other tools read
  it. Text first; the generator can emit both.
