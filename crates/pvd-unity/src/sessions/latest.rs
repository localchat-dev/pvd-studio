use super::{Catalog, LiveView};
use pvd_core::LatestScene;
use pvd_transport::{CaptureServer, ReceiverEvent};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

fn begin(catalog: &Mutex<Catalog>, abort: &Arc<AtomicBool>) -> Result<LatestScene, String> {
    let mut source = LatestScene::default();
    source.set_cancellation(abort.clone());
    let mut state = catalog.lock().unwrap();
    let id = state.next_id()?;
    state.live = Some(LiveView {
        id,
        reader: Arc::new(source.reader()),
        connected: Arc::new(AtomicBool::new(true)),
        frames: 0,
        rooms: Default::default(),
        cache: Arc::default(),
        resources: Arc::default(),
    });
    state.active = id;
    state.error.clear();
    Ok(source)
}

fn disconnect(catalog: &Mutex<Catalog>) {
    let previous = {
        let mut state = catalog.lock().unwrap();
        state.active = 0;
        state.live.take().inspect(|view| {
            view.connected.store(false, Ordering::Release);
        })
    };
    drop(previous);
}

pub(super) fn ingest(server: CaptureServer, catalog: &Mutex<Catalog>, abort: &Arc<AtomicBool>) {
    let mut source = None;
    let mut published = 0;
    while !abort.load(Ordering::Acquire) {
        match server.receiver.recv() {
            Ok(ReceiverEvent::Connected { .. }) => {
                disconnect(catalog);
                published = 0;
                source = match begin(catalog, abort) {
                    Ok(source) => Some(source),
                    Err(error) => {
                        catalog.lock().unwrap().error = error;
                        None
                    }
                };
            }
            Ok(ReceiverEvent::Data { bytes, .. }) => {
                if let Some(parser) = &mut source {
                    let result = parser.feed(&bytes);
                    if let Some((index, rooms)) = parser.summary()
                        && index >= published
                    {
                        published = index.saturating_add(1);
                        let mut state = catalog.lock().unwrap();
                        if let Some(view) = &mut state.live {
                            view.frames = published;
                            view.rooms = rooms;
                        }
                    }
                    if let Err(error) = result {
                        catalog.lock().unwrap().error = error.to_string();
                        source = None;
                    }
                }
            }
            Ok(ReceiverEvent::Disconnected { .. }) => {
                disconnect(catalog);
                source = None;
            }
            Ok(ReceiverEvent::Failed { message, .. }) => {
                disconnect(catalog);
                catalog.lock().unwrap().error = message;
                source = None;
            }
            Ok(ReceiverEvent::Listening(_) | ReceiverEvent::CaptureStarted { .. }) => (),
            Err(_) => break,
        }
    }
    disconnect(catalog);
}
