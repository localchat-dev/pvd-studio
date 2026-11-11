use crate::Error;

/// Identifies analysis failures without treating scene or protocol errors as recoverable.
#[derive(Debug)]
pub struct DecodeError {
    source: Error,
}

impl DecodeError {
    pub(crate) fn new(source: Error) -> Self {
        Self { source }
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.source, formatter)
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RecordingBuilder, live::AnalysisInput};
    use pvd_protocol::{Endian, Event, EventGroup, Header, Name};

    fn builder() -> RecordingBuilder {
        RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        })
    }

    #[test]
    fn strict_memory_failure_preserves_its_original_error() {
        let failure = builder()
            .apply_analysis_input(AnalysisInput::Buffer {
                id: 90,
                kind: "physx3::PxProfileMemoryEventBuffer".into(),
                bytes: [2, 0, 56, 13, 40, 22, 99].into(),
                little_endian: true,
                frame: 0,
            })
            .unwrap_err();
        let typed = failure.downcast_ref::<DecodeError>().unwrap();
        let source = std::error::Error::source(typed).unwrap();
        assert_eq!(source.to_string(), "undefined memory type handle 13");
        assert_eq!(failure.to_string(), source.to_string());
        assert_eq!(
            source.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn missing_scene_instance_is_not_an_analysis_error() {
        let failure = builder()
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 28,
                events: vec![Event::SetPropertyValue {
                    id: 0xDEAD,
                    property: 1,
                    datatype: Name {
                        namespace: 1,
                        name: 2,
                    },
                    items: 1,
                    data: 7u32.to_le_bytes().into(),
                }],
            })
            .unwrap_err();
        assert!(failure.downcast_ref::<DecodeError>().is_none());
        assert_eq!(
            failure.to_string(),
            "property update for missing instance 0xDEAD"
        );
    }
}
