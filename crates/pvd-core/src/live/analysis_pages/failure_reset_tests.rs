use super::*;

#[test]
fn exhausted_domain_keeps_scene_decoding_and_publishes_an_empty_analysis_barrier() {
    let mut capture = crate::LiveRecording::with_history(config(32 << 20, 1)).unwrap();
    capture.feed(&stream_header()).unwrap();
    let builder = capture.builder.as_mut().unwrap();
    builder
        .push(pvd_protocol::EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events: vec![pvd_protocol::Event::StringHandle {
                handle: 1,
                value: "frame".into(),
            }],
        })
        .unwrap();
    builder
        .action(Action::Create(Object {
            id: 7,
            generation: 0,
            type_name: "Actor".into(),
            properties: [("value".into(), Value::U64(0))].into(),
        }))
        .unwrap();
    builder
        .apply_analysis_input(AnalysisInput::Zone(3, "Physics".into()))
        .unwrap();
    builder
        .push(pvd_protocol::EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events: vec![pvd_protocol::Event::EndSection {
                id: 0,
                name: 1,
                timestamp: 0,
            }],
        })
        .unwrap();
    let published = builder.snapshot().unwrap();
    let old_wire = postcard::to_stdvec(&published.analysis).unwrap();
    let domain = published.analysis.resident().unwrap().domain().clone();
    builder
        .streaming
        .as_mut()
        .unwrap()
        .history
        .reclaim_analysis()
        .unwrap();
    let usage = domain.statistics();
    let blocked = domain.reserve(usage.limit - usage.logical_used).unwrap();
    let failure = builder.apply_analysis_input(AnalysisInput::Buffer {
        id: 9,
        kind: "physx3::PxProfileMemoryEventBuffer".into(),
        bytes: Arc::from([255]),
        little_endian: true,
        frame: 1,
    });
    assert!(failure.is_ok(), "scene continuation failed: {failure:?}");
    assert!(
        builder
            .streaming
            .as_ref()
            .unwrap()
            .analysis_failure
            .is_some()
    );
    assert!(domain.same(builder.recording.analysis.resident().unwrap().domain()));
    let mut empty = Analysis::default();
    empty.first_retained_frame = 1;
    assert_eq!(
        postcard::to_stdvec(&builder.recording.analysis).unwrap(),
        postcard::to_stdvec(&empty).unwrap()
    );
    builder
        .action(Action::Set(7, "value".into(), Value::U64(1)))
        .unwrap();
    builder
        .push(pvd_protocol::EventGroup {
            stream_id: 1,
            timestamp: 1,
            offset: 0,
            events: vec![pvd_protocol::Event::EndSection {
                id: 0,
                name: 1,
                timestamp: 1,
            }],
        })
        .unwrap();
    assert_eq!(postcard::to_stdvec(&published.analysis).unwrap(), old_wire);
    assert_eq!(builder.streaming.as_ref().unwrap().frames_seen, 2);
    assert!(
        builder
            .streaming
            .as_ref()
            .unwrap()
            .history
            .warning
            .is_none(),
        "{:?}",
        builder.streaming.as_ref().unwrap().history.warning
    );
    drop(blocked);
    let recording = capture.finish().unwrap();
    assert_eq!(recording.frame_count(), 2);
    assert_eq!(
        recording.state_at(1).unwrap().objects[&7].properties["value"],
        Value::U64(1)
    );
    assert_eq!(
        postcard::to_stdvec(&recording.analysis_at(1).unwrap()).unwrap(),
        postcard::to_stdvec(&empty).unwrap()
    );
    let resources = recording
        .disk
        .as_ref()
        .unwrap()
        .segments
        .front()
        .unwrap()
        .resources
        .clone();
    drop((published, recording));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    assert_eq!(domain.statistics().logical_used, 0);
}
