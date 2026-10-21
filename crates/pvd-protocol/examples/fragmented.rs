use pvd_protocol::{Decoder, EventGroup};
use std::{env, fs, hint::black_box, time::Instant};

fn decode(bytes: &[u8], chunk_size: usize) -> Result<Vec<EventGroup>, pvd_protocol::Error> {
    let mut decoder = Decoder::default();
    let mut groups = Vec::new();
    for chunk in bytes.chunks(chunk_size) {
        groups.extend(decoder.feed(chunk)?);
    }
    decoder.finish()?;
    Ok(groups)
}

fn verify(bytes: &[u8], chunks: &[usize]) {
    let expected = decode(bytes, 64 * 1024).unwrap();
    let largest = expected
        .iter()
        .enumerate()
        .max_by_key(|(index, group)| {
            expected
                .get(index + 1)
                .map_or(bytes.len(), |next| next.offset as usize)
                - group.offset as usize
        })
        .unwrap();
    let start = largest.1.offset as usize;
    let end = expected
        .get(largest.0 + 1)
        .map_or(bytes.len(), |next| next.offset as usize);
    let truncated = [
        0,
        1,
        27,
        start + 1,
        start + 23,
        start + 24,
        (start + end) / 2,
        end - 1,
        bytes.len() - 1,
    ];
    for &chunk_size in chunks {
        let mut decoder = Decoder::default();
        let mut group_index = 0;
        for chunk in bytes.chunks(chunk_size) {
            for group in decoder.feed(chunk).unwrap() {
                assert_eq!(
                    &group, &expected[group_index],
                    "chunk {chunk_size}, group {group_index}"
                );
                group_index += 1;
            }
        }
        decoder.finish().unwrap();
        assert_eq!(group_index, expected.len());
        for end in truncated {
            assert!(
                decode(&bytes[..end], chunk_size).is_err(),
                "truncation {end}, chunk {chunk_size}"
            );
        }
        for (position, replacement) in [
            (start, u32::MAX.to_le_bytes().to_vec()),
            (start + 4, u32::MAX.to_le_bytes().to_vec()),
            (start + 24, vec![255]),
        ] {
            let mut corrupted = bytes.to_vec();
            corrupted[position..position + replacement.len()].copy_from_slice(&replacement);
            let baseline_error = decode(&corrupted, 64 * 1024).unwrap_err();
            assert_eq!(decode(&corrupted, chunk_size).unwrap_err(), baseline_error);
        }
        let mut abandoned = Decoder::default();
        for chunk in bytes[..(start + end) / 2].chunks(chunk_size) {
            abandoned.feed(chunk).unwrap();
        }
        assert!(abandoned.finish().is_err());
        drop(abandoned);
    }
    println!(
        "verification,chunk_sizes={},groups={},events={},largest_group_offset={start},largest_group_bytes={},truncations_per_chunk={},corruptions_per_chunk=3,abandoned_partial_groups_per_chunk=1",
        chunks.len(),
        expected.len(),
        expected.iter().map(|g| g.events.len()).sum::<usize>(),
        end - start,
        truncated.len()
    );
}

fn main() {
    let args: Vec<_> = env::args().collect();
    assert!(
        args.len() >= 2,
        "fragmented <capture> [repetitions] [chunk_sizes] [verify]"
    );
    let bytes = fs::read(&args[1]).unwrap();
    let repeats: usize = args.get(2).map_or(3, |v| v.parse().unwrap());
    let chunks: Vec<usize> = args
        .get(3)
        .map_or("64,1024,8191,65536,1048576", String::as_str)
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect();
    assert!(chunks.iter().all(|size| *size > 0));
    println!(
        "repeat,chunk_bytes,input_bytes,feed_calls,empty_feeds,groups,events,feed_ms,empty_feed_ms,group_feed_ms,max_feed_ms,max_buffered_bytes"
    );
    for repetition in 0..repeats {
        for &chunk_size in &chunks {
            let mut decoder = Decoder::default();
            let mut feed_calls = 0;
            let mut empty_feeds = 0;
            let mut group_count = 0;
            let mut event_count = 0;
            let mut feed_ns = 0;
            let mut empty_feed_ns = 0;
            let mut max_feed_ns = 0;
            let mut max_buffered = 0;
            for chunk in bytes.chunks(chunk_size) {
                let start = Instant::now();
                let groups = black_box(decoder.feed(black_box(chunk)).unwrap());
                let elapsed = start.elapsed().as_nanos();
                feed_ns += elapsed;
                max_feed_ns = max_feed_ns.max(elapsed);
                feed_calls += 1;
                max_buffered = max_buffered.max(decoder.buffered_bytes());
                if groups.is_empty() {
                    empty_feeds += 1;
                    empty_feed_ns += elapsed;
                }
                group_count += groups.len();
                event_count += groups.iter().map(|group| group.events.len()).sum::<usize>();
            }
            decoder.finish().unwrap();
            println!(
                "{repetition},{chunk_size},{},{feed_calls},{empty_feeds},{group_count},{event_count},{:.3},{:.3},{:.3},{:.3},{max_buffered}",
                bytes.len(),
                feed_ns as f64 / 1e6,
                empty_feed_ns as f64 / 1e6,
                (feed_ns - empty_feed_ns) as f64 / 1e6,
                max_feed_ns as f64 / 1e6
            );
        }
    }
    if args.get(4).is_some_and(|value| value == "verify") {
        verify(&bytes, &chunks);
    }
}
