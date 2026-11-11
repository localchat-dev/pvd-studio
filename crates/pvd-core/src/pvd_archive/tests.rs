use super::*;
use flate2::Compression;
use flate2::write::DeflateEncoder;
use std::io::{Cursor, Write};

struct Input<'a> {
    name: &'a str,
    payload: &'a [u8],
    method: u16,
    descriptor: Option<bool>,
    encoded: Option<&'a [u8]>,
    extra: &'a [u8],
}

impl<'a> Input<'a> {
    fn new(name: &'a str, payload: &'a [u8], method: u16) -> Self {
        Self {
            name,
            payload,
            method,
            descriptor: None,
            encoded: None,
            extra: &[],
        }
    }
}

struct Bundle {
    bytes: Vec<u8>,
    local: Vec<usize>,
    data: Vec<usize>,
    central: Vec<usize>,
    descriptor: Vec<usize>,
    end: usize,
}

fn sdk_payload(length: usize) -> Vec<u8> {
    let mut payload = vec![0x5a; length.max(28)];
    payload[..4].copy_from_slice(&pvd_protocol::STREAM_MAGIC.to_le_bytes());
    payload[4..8].copy_from_slice(&1u32.to_le_bytes());
    payload
}

fn bundle(inputs: &[Input<'_>]) -> Bundle {
    let mut result = Bundle {
        bytes: Vec::new(),
        local: Vec::new(),
        data: Vec::new(),
        central: Vec::new(),
        descriptor: Vec::new(),
        end: 0,
    };
    let mut encoded = Vec::new();
    for input in inputs {
        let bytes = if let Some(encoded) = input.encoded {
            encoded.to_vec()
        } else if input.method == 8 {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(input.payload).unwrap();
            encoder.finish().unwrap()
        } else {
            input.payload.to_vec()
        };
        let crc = crc32fast::hash(input.payload);
        let flags = 0x800 | if input.descriptor.is_some() { 8 } else { 0 };
        result.local.push(result.bytes.len());
        u32_push(&mut result.bytes, 0x0403_4b50);
        for value in [20, flags, input.method, 0, 0] {
            u16_push(&mut result.bytes, value);
        }
        for value in [crc, bytes.len() as u32, input.payload.len() as u32] {
            u32_push(
                &mut result.bytes,
                if input.descriptor.is_some() { 0 } else { value },
            );
        }
        u16_push(&mut result.bytes, input.name.len() as u16);
        u16_push(&mut result.bytes, input.extra.len() as u16);
        result.bytes.extend_from_slice(input.name.as_bytes());
        result.bytes.extend_from_slice(input.extra);
        result.data.push(result.bytes.len());
        result.bytes.extend_from_slice(&bytes);
        result.descriptor.push(result.bytes.len());
        if let Some(signed) = input.descriptor {
            if signed {
                u32_push(&mut result.bytes, 0x0807_4b50);
            }
            for value in [crc, bytes.len() as u32, input.payload.len() as u32] {
                u32_push(&mut result.bytes, value);
            }
        }
        encoded.push(bytes);
    }
    let directory_offset = result.bytes.len();
    for (index, input) in inputs.iter().enumerate() {
        result.central.push(result.bytes.len());
        u32_push(&mut result.bytes, 0x0201_4b50);
        let flags = 0x800 | if input.descriptor.is_some() { 8 } else { 0 };
        for value in [0x314, 20, flags, input.method, 0, 0] {
            u16_push(&mut result.bytes, value);
        }
        for value in [
            crc32fast::hash(input.payload),
            encoded[index].len() as u32,
            input.payload.len() as u32,
        ] {
            u32_push(&mut result.bytes, value);
        }
        for value in [input.name.len() as u16, input.extra.len() as u16, 0, 0, 0] {
            u16_push(&mut result.bytes, value);
        }
        u32_push(&mut result.bytes, 0o100644 << 16);
        u32_push(&mut result.bytes, result.local[index] as u32);
        result.bytes.extend_from_slice(input.name.as_bytes());
        result.bytes.extend_from_slice(input.extra);
    }
    result.end = result.bytes.len();
    u32_push(&mut result.bytes, 0x0605_4b50);
    for value in [0, 0, inputs.len() as u16, inputs.len() as u16] {
        u16_push(&mut result.bytes, value);
    }
    u32_push(&mut result.bytes, (result.end - directory_offset) as u32);
    u32_push(&mut result.bytes, directory_offset as u32);
    u16_push(&mut result.bytes, 0);
    result
}

fn u16_push(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn u32_push(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn set_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn set_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn read(bytes: &[u8]) -> Result<Archive, Error> {
    read_with_progress(&mut Cursor::new(bytes), &Limits::default(), |_, _| true)
}

fn rejects(bytes: &[u8], expected: &str) {
    let message = read(bytes).unwrap_err().to_string();
    assert!(
        message.contains(expected),
        "{message:?} does not contain {expected:?}"
    );
}

#[test]
fn stored_and_deflated_members_preserve_exact_payload_and_safe_unicode_names() {
    let sdk = sdk_payload(145_283);
    let mut big_endian = sdk.clone();
    big_endian[..4].copy_from_slice(&pvd_protocol::STREAM_MAGIC.to_be_bytes());
    let mut native = vec![0; 16];
    native[..4].copy_from_slice(&crate::native::MAGIC.to_le_bytes());
    for payload in [&sdk, &big_endian, &native] {
        for method in [0, 8] {
            let input = bundle(&[Input::new("目录/clip.pxd2", payload, method)]);
            let archive = read(&input.bytes).unwrap();
            assert_eq!(archive.member_name, "目录/clip.pxd2");
            assert_eq!(&archive.bytes, payload);
        }
    }
}

#[test]
fn signed_and_unsigned_data_descriptors_are_checked() {
    let payload = sdk_payload(1000);
    for method in [0, 8] {
        for signed in [false, true] {
            let mut entry = Input::new("clip.pxd2", &payload, method);
            entry.descriptor = Some(signed);
            let mut input = bundle(&[entry]);
            assert_eq!(read(&input.bytes).unwrap().bytes, payload);
            input.bytes[input.descriptor[0] + if signed { 4 } else { 0 }] ^= 1;
            rejects(&input.bytes, "data descriptor");
        }
    }
}

#[test]
fn only_the_recording_payload_is_decompressed() {
    let payload = sdk_payload(40);
    let mut sidecar = Input::new("notes.txt", b"unread sidecar", 8);
    sidecar.encoded = Some(&[0xff, 0xff, 0xff]);
    let input = bundle(&[sidecar, Input::new("clip.pxd2", &payload, 0)]);
    assert_eq!(read(&input.bytes).unwrap().bytes, payload);
}

#[test]
fn archives_require_one_exact_recording_name_and_supported_inner_content() {
    let payload = sdk_payload(28);
    for name in ["notes.txt", "clip.PXD2", "pxd2"] {
        rejects(
            &bundle(&[Input::new(name, &payload, 0)]).bytes,
            "no unique .pxd2",
        );
    }
    rejects(
        &bundle(&[
            Input::new("first.pxd2", &payload, 0),
            Input::new("second.pxd2", &payload, 0),
        ])
        .bytes,
        "multiple .pxd2",
    );
    rejects(
        &bundle(&[
            Input::new("clip.pxd2", &payload, 0),
            Input::new("clip.pxd2", &payload, 0),
        ])
        .bytes,
        "member names are ambiguous",
    );
    for inner in [
        b"ordinary data".as_slice(),
        b"PK\x03\x04nested",
        b"PVDCAP03",
    ] {
        rejects(
            &bundle(&[Input::new("clip.pxd2", inner, 0)]).bytes,
            "not a supported SDK stream",
        );
    }
    rejects(
        &bundle(&[Input::new(
            "clip.pxd2",
            &pvd_protocol::STREAM_MAGIC.to_le_bytes(),
            0,
        )])
        .bytes,
        "header is truncated",
    );
}

#[test]
fn unsafe_names_and_special_files_are_rejected_without_extraction() {
    let payload = sdk_payload(28);
    for name in [
        "../clip.pxd2",
        "/clip.pxd2",
        "C:/clip.pxd2",
        "dir\\clip.pxd2",
        "./clip.pxd2",
        "dir//clip.pxd2",
        "bad\0clip.pxd2",
        "x:\\clip.pxd2",
    ] {
        rejects(
            &bundle(&[Input::new(name, &payload, 0)]).bytes,
            "member path",
        );
    }
    let long_name = format!("{}.pxd2", "a".repeat(260));
    rejects(
        &bundle(&[Input::new(&long_name, &payload, 0)]).bytes,
        "name length",
    );
    let mut input = bundle(&[Input::new("clip.pxd2", &payload, 0)]);
    set_u32(&mut input.bytes, input.central[0] + 38, 0o120777 << 16);
    rejects(&input.bytes, "special file");
}

#[test]
fn every_truncated_prefix_and_corrupt_directory_is_rejected() {
    let payload = sdk_payload(70);
    let input = bundle(&[Input::new("clip.pxd2", &payload, 8)]);
    for length in 0..input.bytes.len() {
        assert!(
            read(&input.bytes[..length]).is_err(),
            "accepted prefix {length}"
        );
    }
    for (offset, value, expected) in [
        (input.end + 8, 2u16, "entry counts"),
        (input.end + 10, u16::MAX, "ZIP64"),
        (input.end + 4, 1, "split ZIP"),
    ] {
        let mut corrupt = input.bytes.clone();
        set_u16(&mut corrupt, offset, value);
        rejects(&corrupt, expected);
    }
    let mut corrupt = input.bytes.clone();
    set_u32(&mut corrupt, input.end + 16, 0);
    rejects(&corrupt, "directory does not end");
    let mut trailing = input.bytes;
    trailing.push(0);
    rejects(&trailing, "end record");
}

#[test]
fn local_headers_names_and_crc_are_checked_against_the_directory() {
    let payload = sdk_payload(120);
    let input = bundle(&[Input::new("clip.pxd2", &payload, 0)]);
    for (offset, expected) in [
        (input.local[0] + 6, "local and central headers"),
        (input.local[0] + 14, "CRC or sizes"),
        (input.local[0] + 30, "names disagree"),
        (input.data[0] + 28, "CRC mismatch"),
    ] {
        let mut corrupt = input.bytes.clone();
        corrupt[offset] ^= 1;
        rejects(&corrupt, expected);
    }
    let mut corrupt = input.bytes;
    set_u32(&mut corrupt, input.local[0] + 14, 123);
    set_u32(&mut corrupt, input.central[0] + 16, 123);
    rejects(&corrupt, "CRC mismatch");
}

#[test]
fn deflate_must_finish_at_exact_input_and_output_lengths() {
    let payload = sdk_payload(100_000);
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&payload).unwrap();
    let encoded = encoder.finish().unwrap();
    let mut trailing = encoded.clone();
    trailing.push(0);
    let mut entry = Input::new("clip.pxd2", &payload, 8);
    entry.encoded = Some(&trailing);
    rejects(&bundle(&[entry]).bytes, "trailing compressed bytes");
    let mut entry = Input::new("clip.pxd2", &payload, 8);
    entry.encoded = Some(&encoded[..encoded.len() - 2]);
    rejects(&bundle(&[entry]).bytes, "truncated or invalid");
    let mut input = bundle(&[Input::new("clip.pxd2", &payload, 8)]);
    set_u32(&mut input.bytes, input.local[0] + 22, 28);
    set_u32(&mut input.bytes, input.central[0] + 24, 28);
    rejects(&input.bytes, "actual output exceeds");
    set_u32(&mut input.bytes, input.local[0] + 22, 100_001);
    set_u32(&mut input.bytes, input.central[0] + 24, 100_001);
    rejects(&input.bytes, "decoded size differs");
}

#[test]
fn unsupported_zip_features_and_overlapping_entries_are_rejected() {
    let payload = sdk_payload(28);
    let input = bundle(&[Input::new("clip.pxd2", &payload, 0)]);
    let mut encrypted = input.bytes.clone();
    set_u16(&mut encrypted, input.local[0] + 6, 0x801);
    set_u16(&mut encrypted, input.central[0] + 8, 0x801);
    rejects(&encrypted, "encryption");
    let mut method = input.bytes.clone();
    set_u16(&mut method, input.local[0] + 8, 99);
    set_u16(&mut method, input.central[0] + 10, 99);
    rejects(&method, "compression method");
    let mut extra = Input::new("clip.pxd2", &payload, 0);
    extra.extra = &[1, 0, 0, 0];
    rejects(&bundle(&[extra]).bytes, "ZIP64");
    let nested = bundle(&[Input::new("clip.pxd2", &payload, 0)]);
    let mut overlap = bundle(&[
        Input::new("first.txt", &nested.bytes[..nested.central[0]], 0),
        Input::new("clip.pxd2", &payload, 0),
    ]);
    set_u32(
        &mut overlap.bytes,
        overlap.central[1] + 42,
        overlap.data[0] as u32,
    );
    rejects(&overlap.bytes, "member ranges overlap");
}

#[test]
fn byte_directory_entry_and_output_budgets_cannot_be_relaxed() {
    let payload = sdk_payload(2000);
    let input = bundle(&[Input::new("clip.pxd2", &payload, 8)]);
    let defaults = Limits::default();
    for limits in [
        Limits {
            max_archive_bytes: input.bytes.len() as u64 - 1,
            ..defaults
        },
        Limits {
            max_member_bytes: 28,
            ..defaults
        },
        Limits {
            max_directory_bytes: 20,
            ..defaults
        },
        Limits {
            max_entries: 0,
            ..defaults
        },
        Limits {
            max_member_bytes: MAX_MEMBER_BYTES + 1,
            ..defaults
        },
    ] {
        assert!(read_with_progress(&mut Cursor::new(&input.bytes), &limits, |_, _| true).is_err());
    }
}

#[test]
fn cancellation_is_observed_during_metadata_and_decoding() {
    let payload = sdk_payload(BLOCK_BYTES * 4);
    for method in [0, 8] {
        let input = bundle(&[Input::new("clip.pxd2", &payload, method)]);
        for cancel_during_payload in [false, true] {
            let error = read_with_progress(
                &mut Cursor::new(&input.bytes),
                &Limits::default(),
                |completed, _| cancel_during_payload && completed < BLOCK_BYTES as u64,
            )
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::Interrupted
            );
        }
    }
}

#[test]
fn input_reads_are_chunked_and_signature_detection_is_only_a_hint() {
    struct Tracked {
        source: Cursor<Vec<u8>>,
        largest: usize,
    }
    impl Read for Tracked {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.largest = self.largest.max(bytes.len());
            self.source.read(bytes)
        }
    }
    impl Seek for Tracked {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.source.seek(position)
        }
    }
    let payload = sdk_payload(BLOCK_BYTES * 3);
    let input = bundle(&[Input::new("clip.pxd2", &payload, 0)]);
    let mut source = Tracked {
        source: Cursor::new(input.bytes),
        largest: 0,
    };
    assert_eq!(
        read_with_progress(&mut source, &Limits::default(), |_, _| true)
            .unwrap()
            .bytes,
        payload
    );
    assert!(source.largest <= BLOCK_BYTES);
    for signature in [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"] {
        assert!(has_zip_signature(signature));
        assert!(read(signature).is_err());
    }
    assert!(!has_zip_signature(b"PK"));
    assert!(!has_zip_signature(b"PVDCAP03"));
}
