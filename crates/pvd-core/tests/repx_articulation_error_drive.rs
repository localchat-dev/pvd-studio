use pvd_core::{
    Recording, Value,
    repx::{self, Selection},
};
use pvd_protocol::{Decoder, Endian, Event, Limits, MessageArg, Name};
use std::{collections::BTreeMap, path::PathBuf};

const REQUESTS: [[u32; 4]; 18] = [
    [0, 0, 0, 0x3f800000],
    [0x3f800000, 0, 0, 0],
    [0x3eb851ec, 0x3ef5c28f, 0x3f4ccccd, 0],
    [0x3f8001a3, 0, 0, 0],
    [0x3f7ffcb9, 0, 0, 0],
    [0x80000000, 0, 0xbf800000, 0x80000000],
    [0x3f8002f2, 0, 0, 0],
    [0x3f8002f4, 0, 0, 0],
    [0x3f7ffa1c, 0, 0, 0],
    [0x3f7ffa18, 0, 0, 0],
    [0x3f800345, 0, 0, 0],
    [0x3f800349, 0, 0, 0],
    [0x3f7ff976, 0, 0, 0],
    [0x3f7ff96f, 0, 0, 0],
    [0, 0, 0, 0],
    [0x3f000000, 0, 0, 0],
    [0x40000000, 0, 0, 0],
    [0, 0, 0, 0x3f800000],
];
const SUPPORTED: [bool; 18] = [
    true, true, true, true, true, true, true, false, true, false, false, false, false, false,
    false, false, false, true,
];

#[derive(Clone, Default)]
struct WireJoint {
    target: Option<[u32; 4]>,
    drive: Option<u64>,
    written: [usize; 2],
}

fn field(joint: &mut WireJoint, name: &str, data: &[u8], ordinal: usize) {
    match name {
        "TargetOrientation" => {
            assert_eq!(data.len(), 16);
            joint.target = Some(std::array::from_fn(|index| {
                u32::from_le_bytes(data[index * 4..index * 4 + 4].try_into().unwrap())
            }));
            joint.written[0] = ordinal;
        }
        "DriveType" => {
            assert!(data.len() <= 8);
            let mut bytes = [0; 8];
            bytes[..data.len()].copy_from_slice(data);
            joint.drive = Some(u64::from_le_bytes(bytes));
            joint.written[1] = ordinal;
        }
        _ => {}
    }
}

fn message(
    joint: &mut WireJoint,
    arguments: &[MessageArg],
    strings: &BTreeMap<u32, String>,
    data: &[u8],
    ordinal: usize,
) {
    for arg in arguments {
        let start = arg.offset as usize;
        let end = start + arg.size as usize;
        assert!(end <= data.len());
        field(joint, &strings[&arg.property], &data[start..end], ordinal);
    }
}

fn wire_frames(bytes: &[u8], id: u64, scene: u64) -> (Vec<(WireJoint, usize)>, usize) {
    let mut decoder = Decoder::new(Limits::default());
    let groups = decoder.feed(bytes).unwrap();
    decoder.finish().unwrap();
    assert_eq!(decoder.header().unwrap().endian, Endian::Little);
    let mut strings = BTreeMap::new();
    let mut definitions = BTreeMap::<Name, Vec<MessageArg>>::new();
    let mut active = BTreeMap::new();
    let mut joint = WireJoint::default();
    let mut frames = Vec::new();
    let mut ordinal = 0;
    for group in groups {
        for event in group.events {
            ordinal += 1;
            match event {
                Event::StringHandle { handle, value } => {
                    strings.insert(handle, value);
                }
                Event::CreatePropertyMessage { name, args, .. } => {
                    definitions.insert(name, args);
                }
                Event::SetPropertyValue {
                    id: target,
                    property,
                    data,
                    ..
                } if target == id => {
                    field(&mut joint, &strings[&property], &data, ordinal);
                }
                Event::SetPropertyMessage {
                    id: target,
                    message: name,
                    data,
                } if target == id => {
                    message(&mut joint, &definitions[&name], &strings, &data, ordinal);
                }
                Event::BeginPropertyMessageGroup { message } => {
                    active.insert(group.stream_id, message);
                }
                Event::EndPropertyMessageGroup => {
                    active.remove(&group.stream_id);
                }
                Event::SendPropertyMessageFromGroup { id: target, data } if target == id => {
                    message(
                        &mut joint,
                        &definitions[&active[&group.stream_id]],
                        &strings,
                        &data,
                        ordinal,
                    );
                }
                Event::EndSection {
                    id: target, name, ..
                } if target == scene && strings.get(&name).map(String::as_str) == Some("frame") => {
                    frames.push((joint.clone(), ordinal));
                }
                _ => {}
            }
        }
    }
    (frames, ordinal)
}

fn bits(value: &serde_json::Value) -> [u32; 4] {
    let values = value.as_array().unwrap();
    assert_eq!(values.len(), 4);
    std::array::from_fn(|index| u32::try_from(values[index].as_u64().unwrap()).unwrap())
}

#[test]
#[ignore = "requires the same-run tools/repx-articulations --error-drive capture and oracle"]
fn error_drive_inner_subset_matches_requested_sdk_bits_and_committed_messages() {
    let directory = PathBuf::from(
        std::env::var_os("PVD_REPX_ERROR_DRIVE_DIRECTORY")
            .expect("set PVD_REPX_ERROR_DRIVE_DIRECTORY"),
    );
    let oracle: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("drive-oracle.json")).unwrap())
            .unwrap();
    assert_eq!(oracle["schema"], 1);
    assert_eq!(oracle["sdk_version"], serde_json::json!([3, 4, 2]));
    let rows = oracle["frames"].as_array().unwrap();
    assert_eq!(rows.len(), 18);
    let bytes = std::fs::read(directory.join("capture.pvdstream")).unwrap();
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(recording.frame_count(), 18);
    let id = oracle["joint"].as_u64().unwrap();
    let actor = oracle["actor"].as_u64().unwrap();
    let scene = oracle["scene"].as_u64().unwrap();
    let (raw, _) = wire_frames(&bytes, id, scene);
    assert_eq!(raw.len(), 18);
    let mut report = Vec::new();
    for frame in [17, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 1, 2, 3, 4, 5] {
        let row = &rows[frame];
        let requested = REQUESTS[frame];
        let drive = u64::from(!matches!(frame, 0 | 17));
        assert_eq!(bits(&row["target_bits"]), requested);
        assert_eq!(bits(&row["poststep_target_bits"]), requested);
        assert_eq!(row["drive"].as_u64().unwrap(), drive);
        assert_eq!(row["exportable"].as_bool().unwrap(), SUPPORTED[frame]);
        assert_eq!(
            row["sdk_is_unit"].as_bool().unwrap(),
            !matches!(frame, 11 | 13 | 14 | 15 | 16)
        );
        let (wire, end) = &raw[frame];
        assert_eq!(wire.target.unwrap(), requested);
        assert_eq!(wire.drive, Some(drive));
        assert!(
            wire.written
                .iter()
                .all(|ordinal| *ordinal > 0 && *ordinal < *end)
        );
        let before = usize::try_from(row["prestep_bytes"].as_u64().unwrap()).unwrap();
        let after = usize::try_from(row["poststep_bytes"].as_u64().unwrap()).unwrap();
        assert!(before < after && after <= bytes.len());
        let (previous, before_ordinal) = wire_frames(&bytes[..before], id, scene);
        let (completed, _) = wire_frames(&bytes[..after], id, scene);
        assert_eq!(previous.len(), frame);
        assert_eq!(completed.len(), frame + 1);
        assert_eq!(completed[frame].0.target, Some(requested));
        let state = recording.state_at(frame).unwrap();
        assert!(state.errors.is_empty());
        let joint = &state.objects[&id];
        assert_eq!(joint.properties["DriveType"], Value::U64(drive));
        let Value::Quat(target) = &joint.properties["TargetOrientation"] else {
            panic!("typed captured quaternion required")
        };
        assert_eq!((*target).map(f32::to_bits), requested);
        for selection in [Selection::Frame, Selection::Actor(actor)] {
            let exported = repx::export(&state, selection);
            if SUPPORTED[frame] {
                let exported = exported.unwrap();
                assert_eq!((exported.actors, exported.articulations), (2, 1));
                let text = exported
                    .xml
                    .split_once("<TargetOrientation>")
                    .unwrap()
                    .1
                    .split_once("</TargetOrientation>")
                    .unwrap()
                    .0;
                let actual: Vec<_> = text
                    .split_whitespace()
                    .map(|part| part.parse::<f32>().unwrap().to_bits())
                    .collect();
                assert_eq!(actual, requested);
                assert!(exported.xml.contains(if drive == 0 {
                    "<DriveType>eTARGET</DriveType>"
                } else {
                    "<DriveType>eERROR</DriveType>"
                }));
            } else {
                assert!(
                    exported
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("eERROR target")
                );
            }
        }
        report.push(serde_json::json!({"frame":frame,"requested_bits":requested,"wire_bits":wire.target,"drive":drive,"written":wire.written,"end_ordinal":end,"prestep_last_ordinal":before_ordinal,"emitted_before_step":wire.written.iter().all(|ordinal| *ordinal <= before_ordinal)}));
    }
    if let Some(path) = std::env::var_os("PVD_REPX_ERROR_DRIVE_REPORT") {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        serde_json::to_writer_pretty(file, &report).unwrap();
    }
}
