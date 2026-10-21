# PhysX PVD communication stream evidence

## Baseline and source provenance

The first producer target is **PhysX SDK 3.4.2**, official repository commit
`5e42a5f112351a223c19c17bb331e6c55037b8eb`:
[PhysX-3.4](https://github.com/NVIDIAGameWorks/PhysX-3.4/tree/5e42a5f112351a223c19c17bb331e6c55037b8eb).
It is downloaded under `.deps/physx-3.4`; no installed PVD files are modified.

The decoder is based on these SDK source files, not PDB reconstruction:

- `PxShared/src/pvd/src/PxPvdCommStreamEvents.h`: stream header, event numbering, field serialization order.
- `PxShared/src/pvd/src/PxPvdCommStreamTypes.h`: native-endian integer encoding, length-prefixed arrays, NUL-terminated strings.
- `PxShared/src/pvd/src/PxPvdDataStream.cpp`: group framing, property messages, chunked arrays, appended string data.
- `PxShared/src/pvd/include/PxPvdObjectModelBaseTypes.h`: builtin data representations and names in the `physx3` namespace.
- `PhysX_3.4/Source/PhysX/src/PvdMetaDataPvdBinding.cpp`: scene frame boundaries, actor updates, shape/geometry relations and mesh properties.

PhysX 4.1.2 was downloaded at commit
`a2c0428acab643e60618c681b501e86f7fd558cc` for source comparison. The event header
has identical serialization definitions to the pinned 3.4 source; its only diff
is copyright wording/year. Additional **real 4.1.2 runtime tests** now pass;
see [the 4.1 producer](../tools/physx41-sample/README.md). The user-selected
first validation baseline remains 3.4.2.

## Byte framing

All fields use the producer's native endianness, detected through the magic.
There is no implicit C/C++ struct padding in the event envelope.

| Header offset | Type | Meaning |
| --- | --- | --- |
| 0 | u32 | Magic `837150850` (`0x31E5E882`) |
| 4 | u32 | Communication stream version `1` |
| 8 | u64 | Timestamp conversion numerator |
| 16 | u64 | Timestamp conversion denominator |
| 24 | u32 | Flags; bit 0 denotes 64-bit producer pointers |

Each group follows with `u32 payload_bytes`, `u32 event_count`, `u64 stream_id`,
`u64 timestamp`, then exactly `payload_bytes` event bytes. Every event starts
with one type byte. Types 1–28 are decoded by their exact SDK field schemas.
There is **no per-event byte length**. An unknown event type stops decoding;
skipping to an assumed next event would corrupt the stream.

Strings use a u32 byte length including their final NUL; zero represents an
empty string. Raw data uses a u32 byte count. Metadata arrays use u32 item
counts followed by individually serialized members. Property messages contain
producer-native memory layouts explicitly described by offset and byte size;
they are not safely inferred from host Rust layouts.

Builtin `PxTransform` stores quaternion x/y/z/w before position x/y/z.
`VoidPtr` follows the producer pointer-width flag. Object identifiers in the
event envelope and `ObjectRef` data are u64. String pointer addresses are never
dereferenced: SDK-appended string bytes are decoded separately.

## State and replay

`pvd-protocol` incrementally validates TCP/file bytes without Bevy or egui.
`pvd-core` resolves stream metadata into typed property changes and records
normalized actions independently of presentation ECS entities. Class and
property metadata, enum names, inheritance and unknown opaque payloads are
retained. Large arrays/blobs use shared immutable storage.

Only `EndSection(scene, "frame", timestamp)` creates a completed playback
frame. A snapshot therefore means all changes through that frame's end.
SDK 3.4 `ScbScenePvdClient.cpp` deliberately emits that marker **before** its
current dynamic-actor update block, aligning contact data with the preceding
actor pose. The independent SDK truth CSV samples immediately before
`simulate`, matching this observed convention. The decoder does not shift
frame markers to force post-simulation poses into the wrong frame.
Metadata-only captures expose an initial state and an explicit diagnostic.
Every 60th completed frame stores a checkpoint. Random and backward seeks
restore a checkpoint and apply forward changes; they never invert transforms.
New instances reuse an ID only after destruction and receive a new generation.
References bind to target generations; a stale reference does not silently
retarget a newly created object at the same pointer. Property navigation uses
`SceneState::resolve_reference` to enforce this rule.

`LiveRecording` incrementally decodes and applies bytes. A UI snapshot clones
the index/actions while sharing property array/blob storage. Snapshot cost is
still proportional to capture history and is a documented scaling limitation.
File indexing supports a progress/cancellation callback between 64 KiB reads.

## Limits and unsupported semantics

The present decoder caps event groups at 64 MiB, blobs at 32 MiB, array counts
at one million and events per group at one million. File indexing is capped at
512 MiB, including live captures. Retained history, checkpoint structures and
metadata have an additional 512 MiB estimated allocation budget; reference-
counted payloads are shared. Metadata strings are limited to 32 KiB. This is
also bounded to 1,024 simultaneous property transactions/message groups and
64 MiB of aggregate unfinished property data. Allocation accounting is
conservative, not a measured hard process RSS cap. Profile and memory
analysis share a separate 128 MiB allocation budget as well as record limits.
Allocation names share interned storage; redefining a handle preserves the names
of earlier live allocations. Reference arrays and per-frame renderer batches use
copy-on-write growable buffers, avoiding full-array copying on each append.
Captured float equality compares IEEE bits, including NaN payloads and signed
zero, so valid nonfinite custom data does not cause false replay mismatches.
The `PxPvdDataStream.h` contract specifies duplicate-free reference-array push
and remove operations. Repeated pushes retain one ordered member, while removal
deletes the target member. Reference generations are indexed by target ID,
including references nested within structured property arrays.
Invalid lengths, missing handles, malformed message offsets,
unfinished property transactions and updates of nonexistent objects report
errors. These limits are product limits rather than protocol limits.

The raw communication stream is distinct from PVD3's postprocessed database
containers. Filename suffixes do not establish compatibility. Until the
installed PVD3 successfully loads a fixture, development captures use
`.pvdstream`; a `.pxd` or `.pxd2` file is accepted only when its bytes have the
supported raw signature. Other signatures receive an explicit unsupported
format error. No custom internal format is presented as PVD3 compatibility.

The scene mapper implements actor/shape transforms, primitive box, sphere,
capsule and plane geometry, SDK-cooked triangle meshes, and convex hull fan
triangulation. Mesh scaling uses the SDK's rotated scaling axes and reverses
winding for a negative determinant. Raw mesh vertices/indices are validated
against an independently exported SDK JSON fixture.
Mesh extraction shares vertex/index storage for shapes referencing the same
resource generation and scale parameters. A scene extraction allows 100,000
shapes and 128 MiB of unique mesh data; each mesh allows 1,000,000 vertices and
3,000,000 indices. `render_scene()` returns diagnostics for geometry that cannot
be displayed or a reached limit; recorded properties remain available.

The renderer-event decoder covers all 11 event structures from
`PxPvdUserRenderImpl.h`, including points, lines, triangles, text and constraint
limits. Multiple renderer flushes accumulate until the global frame boundary;
draws then reset. The real rich fixture validates exactly one joint-frame
primitive per frame. Other primitive structures have boundary tests, not yet
equivalent live-SDK fixture coverage. Renderer objects use separate streams
without explicit scene ownership; asynchronous multi-scene alignment remains
unvalidated and emits a diagnostic. PVD contact arrays are decoded from the
source native layouts (64/32-bit pointer widths), preserving positions,
normals, shape references, separation and force availability.

The separate [interleaved Scene fixture](multiscene-sdk.md) verifies 144 merged
SDK frame boundaries, latest published Actor poses and per-Scene timing with
independent SDK/QPC observations. It does not validate renderer ownership or
align different Scenes to one physical simulation time; the renderer diagnostic
above remains applicable.

HeightField, articulation links, CPU Cloth points/constraints and CPU particles
have separate PhysX 3.4.2 fixtures; the compatibility matrix lists their tested
scope. Arbitrary custom structured values, aggregate origin shifts and full
PVD3 database containers require further work. Profile/memory analysis has a
separate decoder and real-SDK integration assertions.

The installed PVD3 Client save/load functions use the little-endian database
signature `0x36101BB0`, followed by metadata and paged object history. See
[the binary evidence](original-file-format.md). The stream decoder detects
this signature and reports that native metadata/history reading is unsupported;
it does not parse the length fields or treat them as SDK events. Fragmentation
and byte-order rejection tests for this diagnostic are synthetic boundary
tests, not native recording interoperability evidence.

## Verification levels

`cargo test -p pvd-protocol -p pvd-core` tests parser boundary failures,
arbitrary network fragmentation, unknown event handling, checkpoint equality,
object destruction/ID reuse, and transaction completion. Synthetic tests are
robustness tests only; they do not establish compatibility with PVD3.

Run `cargo test -p pvd-core --test sdk34 -- --ignored --nocapture` after producing
the fixtures. Tests verify 120 frames, 240 independent baseline actor poses,
names and quaternion values, real pointer reuse at frame 90, geometry sizes,
nonsequential seeks, and 997-byte streaming chunks. The rich fixture verifies
six shapes, cooked vertices and indices with nonuniform rotated scaling,
255 contact records and 120 joint-frame drawing events. These assertions have
passed for both official SDK 3.4.2 and 4.1.2.

The 4.1.2 TCP path is checked independently using its producer and matching SDK truth. Addresses, timings and hashes vary across runs; generated captures are not bundled with the repository.

## PhysX 3.3 evidence gap

The official [3.3.4 PVD guide](https://nvidiagameworks.github.io/PhysX/3.3/PhysXGuide/Manual/VisualDebugger.html)
uses `PxVisualDebuggerExt::createConnection` with a connection manager,
whereas 3.4 uses `PxCreatePvd` and `PxPvdTransport`. This proves an API change;
it does not establish byte-level protocol compatibility or incompatibility.
Real 3.0–3.3 captures and version-specific byte layouts remain unverified. API examples, shared source and the PVD product name cannot establish older protocol compatibility.
