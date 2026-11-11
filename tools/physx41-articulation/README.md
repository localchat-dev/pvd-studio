# PhysX 4.1 articulation fixture

This producer uses the pinned PhysX 4.1.2 SDK to create a fixed-base,
three-link reduced-coordinate chain with revolute and fixed joints. Twelve frames
cover sleep, wake, motion and destruction. SDK getters provide the same-run truth
for object identity, parent/child relationships, geometry, poses and velocities.

Build and validate through the [PhysX 4.1 entry point](../physx41-sample/README.md).
The producer accepts `capture.pvdstream capture.articulation.json`, or
`--host ADDRESS --port PORT truth.articulation.json` for an independent TCP run.
The received capture must have its own same-run `.articulation.json` sidecar.
Set `PVD_ARTICULATION41_CAPTURE` to that capture to run the ignored
`articulation41` core tests separately.

The SDK sends `PxArticulationBase`, `PxArticulationLink` and
`PxArticulationJointBase`. RC flags, joint type, motion and drive settings are not
published by this binding; the viewer does not reconstruct those missing fields.
This fixture does not validate PhysX 4.1 RepX export or native PVD3 containers.

Root properties are synchronized before EndSection, while Link pose/velocity
updates follow it. At the recorded frame boundary, the root therefore reflects
the current SDK sample and the links reflect the preceding sample, or their
initial values for frame zero. The tests check raw message positions and values
independently, then compare replay with the corresponding getter samples.
