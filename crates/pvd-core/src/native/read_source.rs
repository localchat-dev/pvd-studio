use crate::{Error, error};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

const READ_CHUNK: usize = 64 * 1024;

pub(super) struct Source<R> {
    reader: R,
    length: u64,
}

impl<R: Read + Seek> Source<R> {
    pub(super) fn new(mut reader: R) -> Result<Self, Error> {
        let length = reader.seek(SeekFrom::End(0))?;
        reader.rewind()?;
        Ok(Self { reader, length })
    }

    pub(super) fn len(&self) -> u64 {
        self.length
    }

    pub(super) fn check_length(&mut self) -> Result<(), Error> {
        if self.reader.seek(SeekFrom::End(0))? != self.length {
            return Err(error("PVD3 source length changed during import"));
        }
        Ok(())
    }

    pub(super) fn check_range(&self, range: Range<u64>) -> Result<(), Error> {
        if range.start > range.end || range.end > self.length {
            return Err(error("PVD3 file range exceeds capture size"));
        }
        Ok(())
    }

    pub(super) fn read_at(
        &mut self,
        offset: u64,
        output: &mut [u8],
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let end = offset
            .checked_add(u64::try_from(output.len())?)
            .ok_or_else(|| error("PVD3 file range overflow"))?;
        self.check_range(offset..end)?;
        check_cancelled(proceed)?;
        self.reader.seek(SeekFrom::Start(offset))?;
        for chunk in output.chunks_mut(READ_CHUNK) {
            check_cancelled(proceed)?;
            self.reader.read_exact(chunk)?;
        }
        check_cancelled(proceed)
    }

    pub(super) fn read_block(
        &mut self,
        range: Range<u64>,
        limit: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        self.check_range(range.clone())?;
        let length = usize::try_from(range.end - range.start)?;
        if length > limit {
            return Err(error("PVD3 read block exceeds its allocation budget"));
        }
        check_cancelled(proceed)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length)?;
        bytes.resize(length, 0);
        self.read_at(range.start, &mut bytes, proceed)?;
        Ok(bytes)
    }

    pub(super) fn cursor(&mut self, range: Range<u64>) -> Result<Cursor<'_, R>, Error> {
        self.check_range(range.clone())?;
        Ok(Cursor {
            source: self,
            position: range.start,
            end: range.end,
        })
    }
}

pub(super) struct Cursor<'a, R> {
    source: &'a mut Source<R>,
    position: u64,
    end: u64,
}

impl<R: Read + Seek> Cursor<'_, R> {
    pub(super) fn position(&self) -> u64 {
        self.position
    }

    pub(super) fn remaining(&self) -> u64 {
        self.end - self.position
    }

    pub(super) fn skip(&mut self, length: u64) -> Result<(), Error> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| error("PVD3 directory offset overflow"))?;
        if end > self.end {
            return Err(error("PVD3 directory exceeds its containing range"));
        }
        self.position = end;
        Ok(())
    }

    fn fixed<const N: usize>(
        &mut self,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<[u8; N], Error> {
        let begin = self.position;
        self.skip(N as u64)?;
        let mut bytes = [0; N];
        self.source.read_at(begin, &mut bytes, proceed)?;
        Ok(bytes)
    }

    pub(super) fn u32(&mut self, proceed: &mut dyn FnMut() -> bool) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.fixed(proceed)?))
    }

    pub(super) fn u64(&mut self, proceed: &mut dyn FnMut() -> bool) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.fixed(proceed)?))
    }
}

pub(super) struct EventPage {
    payload: Range<u64>,
    offsets: Vec<u32>,
}

impl EventPage {
    pub(super) fn read<R: Read + Seek>(
        source: &mut Source<R>,
        range: Range<u64>,
        expected_count: u32,
        index_limit: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        source.check_range(range.clone())?;
        let mut cursor = source.cursor(range.clone())?;
        let count = cursor.u32(proceed)?;
        if count == 0 || count != expected_count {
            return Err(error("PVD3 page event count mismatch"));
        }
        let table_bytes = usize::try_from(count)?
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(4))
            .ok_or_else(|| error("PVD3 page offset allocation overflow"))?;
        if table_bytes > index_limit {
            return Err(error("PVD3 page offsets exceed their allocation budget"));
        }
        let table_start = cursor.position();
        cursor.skip(u64::from(count) * 4)?;
        let payload = cursor.position()..range.end;
        let payload_length = u32::try_from(payload.end - payload.start)
            .map_err(|_| error("PVD3 page payload exceeds its offset width"))?;
        let mut offsets = Vec::new();
        offsets.try_reserve_exact(usize::try_from(count)? + 1)?;
        let mut bytes = [0; READ_CHUNK];
        let mut done = 0u64;
        let table_length = u64::from(count) * 4;
        while done < table_length {
            let amount = usize::try_from((table_length - done).min(READ_CHUNK as u64))?;
            source.read_at(table_start + done, &mut bytes[..amount], proceed)?;
            for word in bytes[..amount].chunks_exact(4) {
                let offset = u32::from_le_bytes(word.try_into()?);
                if offsets.is_empty() && offset != 0 {
                    return Err(error("PVD3 first event offset must be zero"));
                }
                if offset >= payload_length
                    || offsets.last().is_some_and(|previous| *previous >= offset)
                {
                    return Err(error("invalid PVD3 page event offsets"));
                }
                offsets.push(offset);
            }
            done += amount as u64;
        }
        offsets.push(payload_length);
        Ok(Self { payload, offsets })
    }

    pub(super) fn event_count(&self) -> usize {
        self.offsets.len() - 1
    }

    pub(super) fn event_range(&self, index: usize) -> Result<Range<u64>, Error> {
        let end = index
            .checked_add(1)
            .ok_or_else(|| error("PVD3 event index overflow"))?;
        let offsets = self
            .offsets
            .get(index..=end)
            .ok_or_else(|| error("PVD3 event index exceeds page"))?;
        Ok(self.payload.start + u64::from(offsets[0])..self.payload.start + u64::from(offsets[1]))
    }
}

fn check_cancelled(proceed: &mut dyn FnMut() -> bool) -> Result<(), Error> {
    if !proceed() {
        return Err(
            std::io::Error::new(std::io::ErrorKind::Interrupted, "PVD3 import cancelled").into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor as Bytes, ErrorKind};

    struct Sparse {
        length: u64,
        position: u64,
        reads: Vec<(u64, usize)>,
    }

    impl Read for Sparse {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let length = output.len().min((self.length - self.position) as usize);
            self.reads.push((self.position, length));
            output[..length].fill(0x5a);
            self.position += length as u64;
            Ok(length)
        }
    }

    impl Seek for Sparse {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            let position = match position {
                SeekFrom::Start(position) => i128::from(position),
                SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
                SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            };
            self.position = u64::try_from(position)
                .map_err(|_| std::io::Error::from(ErrorKind::InvalidInput))?;
            Ok(self.position)
        }
    }

    #[test]
    fn reads_a_small_range_above_four_gib_without_reading_the_prefix() {
        let offset = 5u64 << 30;
        let mut source = Source::new(Sparse {
            length: offset + 4096,
            position: 0,
            reads: Vec::new(),
        })
        .unwrap();
        assert_eq!(source.len(), offset + 4096);
        let bytes = source
            .read_block(offset..offset + 17, 17, &mut || true)
            .unwrap();
        assert_eq!(bytes, vec![0x5a; 17]);
        assert_eq!(source.reader.reads, [(offset, 17)]);
        assert!(source.read_block(0..18, 17, &mut || true).is_err());
        assert_eq!(source.reader.reads.len(), 1);
    }

    #[test]
    fn bounds_and_overflow_fail_before_reading() {
        let mut source = Source::new(Bytes::new(vec![0; 10])).unwrap();
        assert!(source.read_at(9, &mut [0; 2], &mut || true).is_err());
        assert!(source.read_at(u64::MAX, &mut [0; 1], &mut || true).is_err());
        let reversed = std::ops::Range { start: 9, end: 8 };
        assert!(source.read_block(reversed, 10, &mut || true).is_err());
        assert!(source.cursor(0..11).is_err());
    }

    #[test]
    fn cancellation_interrupts_a_block_between_fixed_size_reads() {
        let mut source = Source::new(Bytes::new(vec![1; READ_CHUNK * 3])).unwrap();
        let mut checks = 0;
        let error = source
            .read_at(0, &mut vec![0; READ_CHUNK * 3], &mut || {
                checks += 1;
                checks <= 2
            })
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            ErrorKind::Interrupted
        );
        assert_eq!(source.reader.position(), READ_CHUNK as u64);
    }

    #[test]
    fn a_truncated_source_is_not_filled_with_zeroes() {
        let mut source = Source::new(Bytes::new(vec![1; 12])).unwrap();
        source.reader.get_mut().truncate(6);
        let error = source.read_block(0..12, 12, &mut || true).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn source_growth_is_detected_before_import_publication() {
        let mut source = Source::new(Bytes::new(vec![1; 12])).unwrap();
        source.reader.get_mut().push(2);
        assert!(
            source
                .check_length()
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
    }

    #[test]
    fn directory_cursor_cannot_consume_an_adjacent_region() {
        let mut bytes = Vec::new();
        bytes.extend(7u32.to_le_bytes());
        bytes.extend(11u64.to_le_bytes());
        bytes.extend(99u32.to_le_bytes());
        let mut source = Source::new(Bytes::new(bytes)).unwrap();
        let mut cursor = source.cursor(0..12).unwrap();
        assert_eq!(cursor.u32(&mut || true).unwrap(), 7);
        assert_eq!(cursor.u64(&mut || true).unwrap(), 11);
        assert_eq!(cursor.position(), 12);
        assert!(cursor.u32(&mut || true).is_err());
        assert!(cursor.skip(u64::MAX).is_err());
    }

    fn page(offsets: &[u32], payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend((offsets.len() as u32).to_le_bytes());
        for offset in offsets {
            bytes.extend(offset.to_le_bytes());
        }
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn indexes_offsets_without_reading_page_payload() {
        let prefix = 9;
        let mut bytes = vec![0; prefix];
        bytes.extend(page(&[0, 3], b"abcdefgh"));
        let end = bytes.len() as u64;
        let mut source = Source::new(Bytes::new(bytes)).unwrap();
        let page = EventPage::read(&mut source, prefix as u64..end, 2, 12, &mut || true).unwrap();
        assert_eq!(page.event_count(), 2);
        assert_eq!(source.reader.position(), prefix as u64 + 12);
        assert_eq!(
            source
                .read_block(page.event_range(0).unwrap(), 3, &mut || true)
                .unwrap(),
            b"abc"
        );
        assert_eq!(
            source
                .read_block(page.event_range(1).unwrap(), 5, &mut || true)
                .unwrap(),
            b"defgh"
        );
        assert!(page.event_range(2).is_err());
        assert!(page.event_range(usize::MAX).is_err());
    }

    #[test]
    fn rejects_offset_conflicts_and_unbounded_indices() {
        for (offsets, payload, expected_count, limit) in [
            (vec![0, 0], &b"abc"[..], 2, 12),
            (vec![1], &b"abc"[..], 1, 8),
            (vec![0, 3], &b"abc"[..], 2, 12),
            (vec![0, 2, 1], &b"abc"[..], 3, 16),
            (vec![0], &b""[..], 1, 8),
            (vec![0], &b"abc"[..], 2, 8),
            (vec![0, 1], &b"abc"[..], 2, 11),
        ] {
            let bytes = page(&offsets, payload);
            let length = bytes.len() as u64;
            let mut source = Source::new(Bytes::new(bytes)).unwrap();
            assert!(
                EventPage::read(&mut source, 0..length, expected_count, limit, &mut || true)
                    .is_err()
            );
        }
    }
}
