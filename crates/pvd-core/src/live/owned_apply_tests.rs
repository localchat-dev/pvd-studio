use super::*;

struct DigestWriter(blake3::Hasher);
impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn digest(value: &impl Serialize) -> String {
    postcard::to_io(value, DigestWriter(blake3::Hasher::new()))
        .unwrap()
        .0
        .finalize()
        .to_hex()
        .to_string()
}
fn scene_digest(state: &SceneState) -> String {
    digest(&(
        &state.objects,
        &state.reference_lifetimes,
        &state.generations,
        &state.debug_draw,
        &state.contacts,
        &state.errors,
        &state.cameras,
    ))
}

fn state() -> SceneState {
    let mut state = SceneState::default();
    for id in [1, 2, 3] {
        state.objects.insert(
            id,
            Object {
                id,
                generation: 2,
                type_name: format!("Object{id}"),
                properties: [
                    ("Name".into(), Value::String(format!("named object {id}"))),
                    ("Target".into(), Value::Ref(3)),
                ]
                .into(),
            },
        );
        state.generations.insert(id, 2);
        state
            .reference_lifetimes
            .insert((id, "Target".into()), Arc::new([(3, 2)].into()));
        state.debug_draw.insert(
            id,
            Arc::new(vec![Primitive::Point {
                instance: id,
                position: [1.0, 2.0, 3.0],
                color: 0xffff_ffff,
            }]),
        );
        state.contacts.insert(
            id,
            vec![Contact {
                position: [1.0, 2.0, 3.0],
                normal: [0.0, 1.0, 0.0],
                shapes: [id, 3],
                separation: -1.0,
                normal_force: 2.0,
                faces: [0, 1],
                normal_force_available: true,
            }]
            .into(),
        );
    }
    state.errors.push(ErrorEvent {
        code: 8,
        message: "error".into(),
        file: "source.cpp".into(),
        line: 1,
        timestamp: 2,
    });
    state.cameras.insert(
        "Main".into(),
        SourceCamera {
            position: [1.0, 2.0, 3.0],
            up: [0.0, 1.0, 0.0],
            target: [0.0; 3],
        },
    );
    state
}

#[test]
fn owned_assignments_preserve_unique_shared_and_replacement_semantics() {
    let before = state();
    let mut after = before.clone();
    after.objects.remove(&1);
    after.objects.get_mut(&2).unwrap().generation = 3;
    after
        .objects
        .get_mut(&2)
        .unwrap()
        .properties
        .remove("Target");
    after.reference_lifetimes.retain(|(id, _), _| *id == 3);
    after.generations.remove(&1);
    after.generations.insert(2, 3);
    after.debug_draw.remove(&1);
    after.contacts.remove(&2);
    after.errors.clear();
    after.cameras.clear();
    after.cameras.insert(
        "Inspection".into(),
        SourceCamera {
            position: [0.0; 3],
            up: [0.0, 0.0, 1.0],
            target: [1.0, 0.0, 0.0],
        },
    );
    for full in [false, true] {
        for shared in [false, true] {
            let mut patch = SceneUpdate::capture(&after, &[1, 2, 3].into(), true);
            patch.replace_all = full;
            patch.removed_generations.insert(1);
            patch.cameras.insert("Main".into(), None);
            patch.cameras.insert(
                "Inspection".into(),
                after.cameras.get("Inspection").copied(),
            );
            let object_pointer = patch.objects[&2]
                .as_ref()
                .unwrap()
                .object
                .type_name
                .as_ptr();
            let retained = shared.then(|| patch.clone());
            let mut borrowed = before.clone();
            patch.apply(&mut borrowed);
            let mut owned = before.clone();
            patch.apply_owned(&mut owned);
            assert_eq!(scene_digest(&borrowed), scene_digest(&owned));
            assert_eq!(scene_digest(&owned), scene_digest(&after));
            if !shared {
                assert_eq!(object_pointer, owned.objects[&2].type_name.as_ptr());
            }
            if let Some(retained) = retained {
                let mut another = before.clone();
                retained.apply(&mut another);
                assert_eq!(scene_digest(&another), scene_digest(&after));
            }
        }
    }
}
