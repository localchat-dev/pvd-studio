use crate::{Error, error};
use pvd_protocol::Endian;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Pose {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Primitive {
    Point {
        instance: u64,
        position: [f32; 3],
        color: u32,
    },
    Line {
        instance: u64,
        positions: [[f32; 3]; 2],
        colors: [u32; 2],
    },
    Triangle {
        instance: u64,
        positions: [[f32; 3]; 3],
        colors: [u32; 3],
    },
    Text {
        instance: u64,
        position: [f32; 3],
        size: f32,
        color: u32,
        text: String,
    },
    JointFrames {
        instance: u64,
        parent: Pose,
        child: Pose,
    },
    LinearLimit {
        instance: u64,
        parent: Pose,
        child: Pose,
        value: f32,
        active: bool,
    },
    AngularLimit {
        instance: u64,
        pose: Pose,
        lower: f32,
        upper: f32,
        active: bool,
    },
    LimitCone {
        instance: u64,
        pose: Pose,
        y_swing: f32,
        z_swing: f32,
        active: bool,
    },
    DoubleCone {
        instance: u64,
        pose: Pose,
        angle: f32,
        active: bool,
    },
}
#[derive(Default)]
pub(crate) struct RenderDecoder {
    instance: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RenderBufferSummary {
    pub(crate) primitive_count: usize,
    pub(crate) selected_instance: Option<u64>,
    pub(crate) uses_previous_instance: bool,
}

pub(crate) fn scan_context(
    data: &[u8],
    endian: Endian,
    max_retained_bytes: usize,
    progress: &mut dyn FnMut() -> bool,
) -> Result<RenderBufferSummary, Error> {
    Reader::new(data, endian, progress).validate(max_retained_bytes)
}
impl RenderDecoder {
    pub(crate) fn decode(&mut self, data: &[u8], endian: Endian) -> Result<Vec<Primitive>, Error> {
        self.decode_bounded(data, endian, usize::MAX, &mut || true)
    }

    pub(crate) fn decode_bounded(
        &mut self,
        data: &[u8],
        endian: Endian,
        max_retained_bytes: usize,
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<Primitive>, Error> {
        let summary = scan_context(data, endian, max_retained_bytes, progress)?;
        let mut r = Reader::new(data, endian, progress);
        let mut output = Vec::with_capacity(summary.primitive_count);
        let mut next_instance = self.instance;
        while r.pos < data.len() {
            let instance = next_instance;
            match r.u8()? {
                1 => next_instance = r.u64()?,
                2 => r.points(instance, &mut output)?,
                3 => r.lines(instance, &mut output)?,
                4 => r.triangles(instance, &mut output)?,
                5 => output.push(Primitive::JointFrames {
                    instance,
                    parent: r.pose()?,
                    child: r.pose()?,
                }),
                6 => output.push(Primitive::LinearLimit {
                    instance,
                    parent: r.pose()?,
                    child: r.pose()?,
                    value: r.f32()?,
                    active: r.boolean()?,
                }),
                7 => output.push(Primitive::AngularLimit {
                    instance,
                    pose: r.pose()?,
                    lower: r.f32()?,
                    upper: r.f32()?,
                    active: r.boolean()?,
                }),
                8 => output.push(Primitive::LimitCone {
                    instance,
                    pose: r.pose()?,
                    y_swing: r.f32()?,
                    z_swing: r.f32()?,
                    active: r.boolean()?,
                }),
                9 => output.push(Primitive::DoubleCone {
                    instance,
                    pose: r.pose()?,
                    angle: r.f32()?,
                    active: r.boolean()?,
                }),
                10 => {
                    let color = r.u32()?;
                    let position = r.vec3()?;
                    let size = r.f32()?;
                    let length = r.u32()? as usize;
                    if length > 1 << 20 {
                        return Err(error("debug text exceeds one MiB"));
                    }
                    let text = r.take(length)?;
                    if text.last() != Some(&0) {
                        return Err(error("debug text missing NUL terminator"));
                    }
                    output.push(Primitive::Text {
                        instance,
                        position,
                        size,
                        color,
                        // Exact string capacity keeps the retained history charge conservative.
                        text: String::from_utf8_lossy(&text[..text.len() - 1])
                            .into_owned()
                            .into_boxed_str()
                            .into_string(),
                    });
                }
                11 => {
                    r.points(instance, &mut output)?;
                    r.lines(instance, &mut output)?;
                    r.triangles(instance, &mut output)?;
                }
                kind => {
                    return Err(error(format!(
                        "unknown PVD renderer event {kind} at byte {}; no event boundary available",
                        r.pos - 1
                    )));
                }
            }
            if output.len() > 1_000_000 {
                return Err(error("renderer buffer exceeds one million primitives"));
            }
        }
        self.instance = next_instance;
        Ok(output)
    }
}
struct Reader<'a, 'p> {
    bytes: &'a [u8],
    pos: usize,
    endian: Endian,
    progress: &'p mut dyn FnMut() -> bool,
    next_progress: usize,
}
impl<'a, 'p> Reader<'a, 'p> {
    fn new(bytes: &'a [u8], endian: Endian, progress: &'p mut dyn FnMut() -> bool) -> Self {
        Self {
            bytes,
            pos: 0,
            endian,
            progress,
            next_progress: 0,
        }
    }

    fn validate(&mut self, max_retained_bytes: usize) -> Result<RenderBufferSummary, Error> {
        let mut count = 0usize;
        let mut text_bytes = 0usize;
        let mut selected_instance = None;
        let mut uses_previous_instance = false;
        while self.pos < self.bytes.len() {
            let previous_count = count;
            let kind = self.u8()?;
            match kind {
                1 => {
                    selected_instance = Some(self.u64()?);
                }
                2..=4 => count += self.skip_array(16 * (kind as usize - 1))?,
                5 => {
                    self.take(56)?;
                    count += 1;
                }
                6..=9 => {
                    self.take(match kind {
                        6 => 60,
                        7 | 8 => 36,
                        _ => 32,
                    })?;
                    self.boolean()?;
                    count += 1;
                }
                10 => {
                    self.take(20)?;
                    let length = self.u32()? as usize;
                    if length > 1 << 20 {
                        return Err(error("debug text exceeds one MiB"));
                    }
                    if self.take(length)?.last() != Some(&0) {
                        return Err(error("debug text missing NUL terminator"));
                    }
                    // Lossy UTF-8 can expand each source byte to a three-byte replacement.
                    text_bytes = text_bytes
                        .checked_add(length * 6)
                        .ok_or_else(|| error("renderer text memory accounting overflow"))?;
                    count += 1;
                }
                11 => {
                    for stride in [16, 32, 48] {
                        count += self.skip_array(stride)?;
                    }
                }
                _ => {
                    return Err(error(format!(
                        "unknown PVD renderer event {kind} at byte {}; no event boundary available",
                        self.pos - 1
                    )));
                }
            }
            uses_previous_instance |= count != previous_count && selected_instance.is_none();
            if count > 1_000_000 {
                return Err(error("renderer buffer exceeds one million primitives"));
            }
            // A history action, the displayed batch and its growth capacity all retain primitives.
            let retained = count
                .checked_mul(std::mem::size_of::<Primitive>() * 3)
                .and_then(|n| n.checked_add(text_bytes))
                .ok_or_else(|| error("renderer memory accounting overflow"))?;
            if retained > max_retained_bytes {
                return Err(error("renderer decoded primitives exceed memory budget"));
            }
        }
        Ok(RenderBufferSummary {
            primitive_count: count,
            selected_instance,
            uses_previous_instance,
        })
    }

    fn skip_array(&mut self, stride: usize) -> Result<usize, Error> {
        let count = self.count(stride)?;
        self.take(count * stride)?;
        Ok(count)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.pos >= self.next_progress {
            if !(self.progress)() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "renderer decoding cancelled",
                )
                .into());
            }
            self.next_progress = self.pos.saturating_add(4096);
        }
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| error("renderer length overflow"))?;
        let b = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| error(format!("truncated renderer event at byte {}", self.pos)))?;
        self.pos = end;
        Ok(b)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn boolean(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(error("invalid renderer boolean")),
        }
    }
    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?.try_into().unwrap();
        Ok(match self.endian {
            Endian::Little => u32::from_le_bytes(b),
            Endian::Big => u32::from_be_bytes(b),
        })
    }
    fn u64(&mut self) -> Result<u64, Error> {
        let b = self.take(8)?.try_into().unwrap();
        Ok(match self.endian {
            Endian::Little => u64::from_le_bytes(b),
            Endian::Big => u64::from_be_bytes(b),
        })
    }
    fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn vec3(&mut self) -> Result<[f32; 3], Error> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    fn pose(&mut self) -> Result<Pose, Error> {
        Ok(Pose {
            rotation: [self.f32()?, self.f32()?, self.f32()?, self.f32()?],
            position: self.vec3()?,
        })
    }
    fn count(&mut self, stride: usize) -> Result<usize, Error> {
        let n = self.u32()? as usize;
        if n > 1_000_000 || n > (self.bytes.len() - self.pos) / stride {
            return Err(error(
                "renderer array exceeds its payload or primitive limit",
            ));
        }
        Ok(n)
    }
    fn points(&mut self, instance: u64, output: &mut Vec<Primitive>) -> Result<(), Error> {
        for _ in 0..self.count(16)? {
            output.push(Primitive::Point {
                instance,
                position: self.vec3()?,
                color: self.u32()?,
            });
        }
        Ok(())
    }
    fn lines(&mut self, instance: u64, output: &mut Vec<Primitive>) -> Result<(), Error> {
        for _ in 0..self.count(32)? {
            let p0 = self.vec3()?;
            let c0 = self.u32()?;
            let p1 = self.vec3()?;
            let c1 = self.u32()?;
            output.push(Primitive::Line {
                instance,
                positions: [p0, p1],
                colors: [c0, c1],
            });
        }
        Ok(())
    }
    fn triangles(&mut self, instance: u64, output: &mut Vec<Primitive>) -> Result<(), Error> {
        for _ in 0..self.count(48)? {
            let p0 = self.vec3()?;
            let c0 = self.u32()?;
            let p1 = self.vec3()?;
            let c1 = self.u32()?;
            let p2 = self.vec3()?;
            let c2 = self.u32()?;
            output.push(Primitive::Triangle {
                instance,
                positions: [p0, p1, p2],
                colors: [c0, c1, c2],
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIANGLE: &[u8] = &[
        4, 1, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 64, 0, 0, 64, 64, 68, 51, 34, 17, 0, 0, 128, 64, 0,
        0, 160, 64, 0, 0, 192, 64, 136, 119, 102, 85, 0, 0, 224, 64, 0, 0, 0, 65, 0, 0, 16, 65,
        204, 187, 170, 153,
    ];

    #[test]
    fn context_scan_preserves_explicit_zero_and_primitive_order() {
        for bytes in [&[][..], &[11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..]] {
            assert_eq!(
                scan_context(bytes, Endian::Little, 0, &mut || true).unwrap(),
                RenderBufferSummary {
                    primitive_count: 0,
                    selected_instance: None,
                    uses_previous_instance: false,
                }
            );
        }
        for instance in [0u64, 17] {
            let mut bytes = vec![1];
            bytes.extend(instance.to_le_bytes());
            bytes.extend(TRIANGLE);
            let summary = scan_context(&bytes, Endian::Little, usize::MAX, &mut || true).unwrap();
            assert_eq!(summary.primitive_count, 1);
            assert_eq!(summary.selected_instance, Some(instance));
            assert!(!summary.uses_previous_instance);
            let mut decoder = RenderDecoder::default();
            let values = decoder.decode(&bytes, Endian::Little).unwrap();
            assert!(matches!(
                values.as_slice(),
                [Primitive::Triangle { instance: selected, positions, colors }]
                    if *selected == instance
                        && *positions == [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]
                        && *colors == [0x11223344, 0x55667788, 0x99aabbcc]
            ));
        }
        let mut bytes = TRIANGLE.to_vec();
        bytes.extend([1, 18, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            scan_context(&bytes, Endian::Little, usize::MAX, &mut || true).unwrap(),
            RenderBufferSummary {
                primitive_count: 1,
                selected_instance: Some(18),
                uses_previous_instance: true,
            }
        );
    }

    #[test]
    fn context_scan_honors_endian_without_allocating_primitives() {
        let bytes = [1, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(
            scan_context(&bytes, Endian::Big, 0, &mut || true).unwrap(),
            RenderBufferSummary {
                primitive_count: 0,
                selected_instance: Some(0x0102030405060708),
                uses_previous_instance: false,
            }
        );
        assert_eq!(
            scan_context(&bytes, Endian::Little, 0, &mut || true)
                .unwrap()
                .selected_instance,
            Some(0x0807060504030201)
        );
    }

    #[test]
    fn context_scan_checks_budget_and_cancellation_across_captured_commands() {
        let mut bytes = vec![2];
        bytes.extend(100_000u32.to_le_bytes());
        bytes.resize(bytes.len() + 100_000 * 16, 0);
        let commands = TRIANGLE.repeat(10_000);
        let mut calls = 0;
        let error = scan_context(&commands, Endian::Little, usize::MAX, &mut || {
            calls += 1;
            calls < 3
        })
        .unwrap_err();
        assert_eq!(calls, 3);
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(
            scan_context(&bytes, Endian::Little, 1024, &mut || true)
                .unwrap_err()
                .to_string()
                .contains("memory budget")
        );
        assert!(
            scan_context(
                &bytes[..bytes.len() - 1],
                Endian::Little,
                usize::MAX,
                &mut || true
            )
            .is_err()
        );
    }

    #[test]
    fn bulk_primitives_preserve_color_and_instance_across_flushes() {
        let mut d = RenderDecoder::default();
        let mut initial = vec![1];
        initial.extend(99u64.to_le_bytes());
        d.decode(&initial, Endian::Little).unwrap();
        let mut bytes = vec![2];
        bytes.extend(1u32.to_le_bytes());
        for x in [1.0f32, 2.0, 3.0] {
            bytes.extend(x.to_le_bytes());
        }
        bytes.extend(0xffff0000u32.to_le_bytes());
        let decoded = d.decode(&bytes, Endian::Little).unwrap();
        assert_eq!(
            decoded,
            vec![Primitive::Point {
                instance: 99,
                position: [1.0, 2.0, 3.0],
                color: 0xffff0000
            }]
        );
        for n in 1..bytes.len() {
            assert!(
                RenderDecoder::default()
                    .decode(&bytes[..n], Endian::Little)
                    .is_err()
            );
        }
    }
    #[test]
    fn unknown_and_hostile_lengths_fail() {
        assert!(
            RenderDecoder::default()
                .decode(&[255], Endian::Little)
                .is_err()
        );
        assert!(
            RenderDecoder::default()
                .decode(&[2, 255, 255, 255, 255], Endian::Little)
                .is_err()
        );
    }

    #[test]
    fn preflight_bounds_output_and_cancellation_without_changing_instance() {
        let mut decoder = RenderDecoder { instance: 42 };
        let mut points = vec![1];
        points.extend(99u64.to_le_bytes());
        points.push(2);
        points.extend(10_000u32.to_le_bytes());
        points.resize(points.len() + 10_000 * 16, 0);
        assert!(
            decoder
                .decode_bounded(&points, Endian::Little, 1024, &mut || true)
                .unwrap_err()
                .to_string()
                .contains("memory budget")
        );
        assert_eq!(decoder.instance, 42);
        let mut calls = 0;
        let error = decoder
            .decode_bounded(&points, Endian::Little, usize::MAX, &mut || {
                calls += 1;
                calls < 4
            })
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(decoder.instance, 42);
        assert_eq!(calls, 4);

        let mut text = vec![10];
        text.extend([0u8; 20]);
        text.extend(1024u32.to_le_bytes());
        text.extend([0xffu8; 1023]);
        text.push(0);
        assert!(
            decoder
                .decode_bounded(&text, Endian::Little, 2048, &mut || true)
                .is_err()
        );
        let decoded = decoder.decode(&text, Endian::Little).unwrap();
        let Primitive::Text { text, .. } = &decoded[0] else {
            panic!("expected text");
        };
        assert_eq!(text.len(), 1023 * 3);
        assert_eq!(text.capacity(), text.len());
    }

    #[test]
    fn combined_arrays_limits_and_text_match_the_bounded_scan() {
        let mut bytes = vec![1];
        bytes.extend(7u64.to_le_bytes());
        bytes.push(11);
        for vertices in [1usize, 2, 3] {
            bytes.extend(1u32.to_le_bytes());
            for vertex in 0..vertices {
                for v in [vertex as f32, 2.0, 3.0] {
                    bytes.extend(v.to_le_bytes());
                }
                bytes.extend((0xff000001u32 + vertex as u32).to_le_bytes());
            }
        }
        for (kind, length) in [(5, 56), (6, 60), (7, 36), (8, 36), (9, 32)] {
            bytes.push(kind);
            bytes.extend(vec![0; length]);
            if kind > 5 {
                bytes.push(1);
            }
        }
        bytes.push(10);
        bytes.extend([0; 20]);
        bytes.extend(4u32.to_le_bytes());
        bytes.extend(b"abc\0");
        let decoded = RenderDecoder::default()
            .decode(&bytes, Endian::Little)
            .unwrap();
        assert_eq!(decoded.len(), 9);
        assert!(matches!(
            decoded[0],
            Primitive::Point {
                instance: 7,
                color: 0xff000001,
                ..
            }
        ));
        assert!(matches!(
            decoded[1],
            Primitive::Line {
                instance: 7,
                colors: [0xff000001, 0xff000002],
                ..
            }
        ));
        assert!(matches!(
            decoded[2],
            Primitive::Triangle {
                instance: 7,
                colors: [0xff000001, 0xff000002, 0xff000003],
                ..
            }
        ));
        assert!(matches!(&decoded[8], Primitive::Text { instance:7, text, .. } if text == "abc"));
        for end in bytes.len() - 3..bytes.len() {
            assert!(
                RenderDecoder::default()
                    .decode(&bytes[..end], Endian::Little)
                    .is_err()
            );
        }
        let mut bool_event = vec![9];
        bool_event.extend([0; 32]);
        bool_event.push(2);
        assert!(
            RenderDecoder::default()
                .decode(&bool_event, Endian::Little)
                .is_err()
        );
        let mut excess = vec![2];
        excess.extend(600_000u32.to_le_bytes());
        excess.resize(5 + 600_000 * 16, 0);
        excess.extend(excess.clone());
        assert!(
            RenderDecoder::default()
                .decode(&excess, Endian::Little)
                .unwrap_err()
                .to_string()
                .contains("one million primitives")
        );
    }
}
