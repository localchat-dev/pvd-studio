use super::*;
use pvd_core::{RecordingBuilder, frame_sources::FrameDirectory};
use pvd_protocol::{Endian, EventGroup, Header, Name};

const MIXED_CLOCKS: [u64; 5] = [
    9_000_000_000_000_000_000,
    10,
    9_000_000_007_000_000_000,
    14_000_000_010,
    9,
];

fn recording(timestamps: [u64; 5]) -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 3,
        timestamp_denominator: 7,
        flags: 1,
        endian: Endian::Little,
    });
    let mut events: Vec<_> = [
        (1, "frame"),
        (2, "ApexFrame"),
        (3, "Actor"),
        (4, "Counter"),
        (5, "PvdU32"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    let class = Name {
        namespace: 0,
        name: 3,
    };
    events.extend([
        Event::CreateClass { name: class },
        Event::CreateInstance { class, id: 7 },
    ]);
    for (index, timestamp) in timestamps.into_iter().enumerate() {
        events.extend([
            Event::SetPropertyValue {
                id: 7,
                property: 4,
                datatype: Name {
                    namespace: 0,
                    name: 5,
                },
                items: 1,
                data: [17u32, 19, 23, 29, 31][index].to_le_bytes().into(),
            },
            Event::EndSection {
                id: 100,
                name: [2, 1, 2, 1, 1][index],
                timestamp,
            },
        ]);
    }
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 888,
            offset: 0,
            events,
        })
        .unwrap();
    builder.finish().unwrap()
}

fn position(recording: &Recording, index: usize) -> String {
    let mut bytes = Vec::new();
    write_frame_position(&mut bytes, recording, index).unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn csv_keeps_independent_clocks_and_exact_source_keys() {
    let recording = recording(MIXED_CLOCKS);
    let mut bytes = Vec::new();
    assert_eq!(
        write_property_csv(&mut bytes, &recording, 7, "Counter").unwrap(),
        5
    );
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        concat!(
            "frame,time_seconds,object_id,generation,property,value,section,source_id,source_frame\n",
            "0,0.000000000,0x7,0,\"Counter\",\"U64(17)\",ApexFrame,0x64,0\n",
            "1,0.000000000,0x7,0,\"Counter\",\"U64(19)\",frame,0x64,0\n",
            "2,3.000000000,0x7,0,\"Counter\",\"U64(23)\",ApexFrame,0x64,1\n",
            "3,6.000000000,0x7,0,\"Counter\",\"U64(29)\",frame,0x64,1\n",
            "4,,0x7,0,\"Counter\",\"U64(31)\",frame,0x64,2\n",
        )
    );
    assert_eq!(
        position(&recording, 0),
        "frame_source: ApexFrame 0x64\nsource_frame: 0\nframe: 0\nseconds: 0.000000000\n"
    );
    assert_eq!(
        position(&recording, 1),
        "frame_source: frame 0x64\nsource_frame: 0\nframe: 1\nseconds: 0.000000000\n"
    );
    assert_eq!(
        position(&recording, 4),
        "frame_source: frame 0x64\nsource_frame: 2\nframe: 4\nseconds: unavailable\n"
    );
}

#[test]
fn legacy_csv_keeps_recording_ordinals_without_inventing_a_source_id() {
    let mut recording = recording([
        11,
        7_000_000_011,
        14_000_000_011,
        21_000_000_011,
        28_000_000_011,
    ]);
    recording.frame_sources =
        FrameDirectory::from_legacy(&recording.frames, Default::default()).unwrap();
    let mut bytes = Vec::new();
    assert_eq!(
        write_property_csv(&mut bytes, &recording, 7, "Counter").unwrap(),
        5
    );
    let text = String::from_utf8(bytes).unwrap();
    for (index, (line, seconds)) in text
        .lines()
        .skip(1)
        .zip([
            "0.000000000",
            "3.000000000",
            "6.000000000",
            "9.000000000",
            "12.000000000",
        ])
        .enumerate()
    {
        let cells: Vec<_> = line.split(',').collect();
        assert_eq!(cells.len(), 9);
        assert_eq!(cells[0], index.to_string());
        assert_eq!(cells[1], seconds);
        assert_eq!(cells[6], "recording");
        assert_eq!(cells[7], "");
        assert_eq!(cells[8], index.to_string());
    }
    assert_eq!(
        position(&recording, 1),
        "frame_source: recording\nsource_frame: 1\nframe: 1\nseconds: 3.000000000\n"
    );
}

#[test]
fn missing_source_metadata_rejects_csv_before_writing_rows() {
    let mut recording = recording(MIXED_CLOCKS);
    recording.frame_sources = FrameDirectory::default();
    let mut bytes = Vec::new();
    let failure = write_property_csv(&mut bytes, &recording, 7, "Counter").unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("inconsistent recording frame-source directory")
    );
    assert!(bytes.is_empty());
    assert_eq!(position(&recording, 0), "frame: 0\nseconds: unavailable\n");
}
