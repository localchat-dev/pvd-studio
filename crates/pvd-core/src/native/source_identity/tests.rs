use super::*;
use std::{cell::RefCell, io::Cursor, rc::Rc};

struct MutableSource {
    bytes: Rc<RefCell<Vec<u8>>>,
    position: u64,
}

impl Read for MutableSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let bytes = self.bytes.borrow();
        let begin = (self.position as usize).min(bytes.len());
        let size = output.len().min(bytes.len() - begin);
        output[..size].copy_from_slice(&bytes[begin..begin + size]);
        self.position += size as u64;
        Ok(size)
    }
}

impl Seek for MutableSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => self.bytes.borrow().len() as i128 + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        self.position =
            u64::try_from(position).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.position)
    }
}

#[test]
fn arbitrary_reads_match_the_fingerprinted_bytes_across_blocks() {
    let bytes: Vec<u8> = (0..BLOCK_BYTES * 3 + 17).map(|i| i as u8).collect();
    let mut input = Cursor::new(&bytes);
    let mut source = CheckedSource::new(&mut input, |_, _| true).unwrap();
    assert_eq!(source.digest(), blake3::hash(&bytes));
    for (begin, size) in [(BLOCK_BYTES - 9, 31), (13, 91), (BLOCK_BYTES * 3, 17)] {
        source.seek(SeekFrom::Start(begin as u64)).unwrap();
        let mut result = vec![0; size];
        source.read_exact(&mut result).unwrap();
        assert_eq!(result, bytes[begin..begin + size]);
    }
    source.recheck(|_, _| true).unwrap();
    assert!(source.seek(SeekFrom::Start(u64::MAX)).is_ok());
    assert_eq!(source.read(&mut [0; 1]).unwrap(), 0);
    assert!(source.seek(SeekFrom::End(-1 - bytes.len() as i64)).is_err());
}

#[test]
fn changed_uncached_bytes_are_rejected_before_they_reach_a_parser() {
    let bytes = Rc::new(RefCell::new(vec![7; BLOCK_BYTES + 1]));
    let mut input = MutableSource {
        bytes: bytes.clone(),
        position: 0,
    };
    let mut source = CheckedSource::new(&mut input, |_, _| true).unwrap();
    bytes.borrow_mut()[0] = 9;
    let mut result = [0; 1];
    assert!(
        source
            .read(&mut result)
            .unwrap_err()
            .to_string()
            .contains("source changed")
    );
    assert_eq!(result, [0]);
}

#[test]
fn recheck_detects_changes_to_cached_blocks_and_file_growth() {
    for grow in [false, true] {
        let bytes = Rc::new(RefCell::new(vec![7; 9]));
        let mut input = MutableSource {
            bytes: bytes.clone(),
            position: 0,
        };
        let mut source = CheckedSource::new(&mut input, |_, _| true).unwrap();
        assert_eq!(source.read(&mut [0; 1]).unwrap(), 1);
        if grow {
            bytes.borrow_mut().push(8);
        } else {
            bytes.borrow_mut()[0] = 8;
        }
        assert!(
            source
                .recheck(|_, _| true)
                .unwrap_err()
                .to_string()
                .contains("source changed")
        );
    }
}

#[test]
fn cancellation_and_truncation_interrupt_fingerprinting_and_recheck() {
    let bytes = Rc::new(RefCell::new(vec![7; BLOCK_BYTES + 1]));
    let mut input = MutableSource {
        bytes: bytes.clone(),
        position: 0,
    };
    assert!(CheckedSource::new(&mut input, |_, _| false).is_err());
    assert_eq!(input.position, 0);
    let mut source = CheckedSource::new(&mut input, |_, _| true).unwrap();
    assert!(
        source
            .recheck(|_, _| false)
            .unwrap_err()
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::Interrupted)
    );
    bytes.borrow_mut().truncate(1);
    assert!(source.recheck(|_, _| true).is_err());
}

#[test]
fn fingerprint_index_is_bounded_before_reading() {
    struct TooLarge;
    impl Read for TooLarge {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("unbounded input read")
        }
    }
    impl Seek for TooLarge {
        fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
            Ok(129 << 30)
        }
    }
    let error = CheckedSource::new(&mut TooLarge, |_, _| true)
        .err()
        .unwrap();
    assert!(error.to_string().contains("fingerprint index exceeds"));
}
