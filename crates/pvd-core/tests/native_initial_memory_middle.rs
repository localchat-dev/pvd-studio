use pvd_core::{
    MemoryHistoryScope, Recording,
    native::export::{rewrite, trim_scene_frames},
};
use std::path::PathBuf;

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn wide(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn leading_start_opcode(bytes: &[u8]) -> usize {
    assert_eq!((word(bytes, 0), word(bytes, 4)), (0x36101bb0, 1));
    let metadata_end = 16 + word(bytes, 8) as usize + word(bytes, 12) as usize;
    let mut at = (metadata_end / 4096 + 1) * 4096 + 8;
    let pages = word(bytes, at) as usize;
    at += 4;
    let mut first_page = None;
    for _ in 0..pages {
        let first = wide(bytes, at) as usize;
        let position = wide(bytes, at + 8) as usize;
        let count = word(bytes, at + 16) as usize;
        if first <= 1 && 1 < first + count {
            first_page = Some((first, position, count));
        }
        at += 24;
    }
    let snapshots = word(bytes, at) as usize;
    at += 4 + snapshots * 24;
    let deleted = word(bytes, at) as usize;
    at += 4 + deleted * 16;
    let base = (at / 4096 + 1) * 4096;
    let (first, position, count) = first_page.unwrap();
    let page = base + position;
    assert_eq!(word(bytes, page) as usize, count);
    let event = page + 4 + count * 4 + word(bytes, page + 4 + (1 - first) * 4) as usize;
    assert_eq!(
        (
            word(bytes, event),
            word(bytes, event + 4),
            word(bytes, event + 8)
        ),
        (3, 2, 50)
    );
    let length = word(bytes, event + 12) as usize;
    assert!(length > 0 && length.is_multiple_of(26));
    assert_eq!(&bytes[event + 16..event + 18], [1, 15]);
    assert_eq!(word(bytes, event + 16 + length), 1);
    assert_eq!(word(bytes, event + 20 + length), 1);
    event + 16
}

#[test]
#[ignore = "requires the exact original rich paired native container"]
fn original_rich_snapshot_retrim_keeps_known_scope_and_refuses_unknown_leading_starts() {
    let directory = std::env::var_os("PVD_NATIVE_MEMORY_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native"));
    let path = directory.join("rich-paired-native.pxd2");
    let source = std::fs::read(&path).unwrap();
    assert_eq!(source.len(), 1_034_096);
    for (first, last) in [(1, 2), (30, 31), (60, 61)] {
        let mut initial = Vec::new();
        trim_scene_frames(&source, first, last, &mut initial, || true).unwrap();
        let mut output = Vec::new();
        trim_scene_frames(&initial, 1, 1, &mut output, || true).unwrap();
        let original = Recording::from_bytes(&initial).unwrap();
        let restored = Recording::from_bytes(&output).unwrap();
        assert_eq!(restored.frame_count(), 1);
        assert_eq!(
            restored.memory_history_scope(),
            MemoryHistoryScope::AfterInitialSnapshot
        );
        let actual = restored.state_at(0).unwrap();
        let expected = original.state_at(1).unwrap();
        assert_eq!(actual.objects, expected.objects);
        assert_eq!(actual.debug_draw, expected.debug_draw);
        assert_eq!(actual.errors, expected.errors);
        assert_eq!(actual.contacts, expected.contacts);
        restored.verify_replay().unwrap();
        let mut rewritten = Vec::new();
        rewrite(&output, &mut rewritten, || true).unwrap();
        assert_eq!(rewritten, output);

        let mut unknown = initial.clone();
        let opcode = leading_start_opcode(&unknown);
        unknown[opcode] = 2;
        assert_eq!(
            unknown.iter().zip(&initial).filter(|(a, b)| a != b).count(),
            1
        );
        let unknown_recording = Recording::from_bytes(&unknown).unwrap();
        assert_eq!(unknown_recording.frame_count(), 2);
        assert!(unknown_recording.analysis.legacy().unwrap().unmatched_stops > 0);
        let mut output = Vec::new();
        let failure = trim_scene_frames(&unknown, 1, 1, &mut output, || true).unwrap_err();
        assert!(
            failure.to_string().contains("known absolute starts"),
            "{failure}"
        );
        assert!(output.is_empty());
    }
    assert_eq!(std::fs::read(path).unwrap(), source);
}
