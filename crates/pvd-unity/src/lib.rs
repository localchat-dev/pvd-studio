mod export;
mod geometry;
mod sessions;

use geometry::FocusRequest;
use serde::Deserialize;
use serde_json::{Value, json};
use sessions::{FrameFormat, Sessions};
use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;
use std::sync::{LazyLock, Mutex};

#[derive(Default)]
struct Bridge {
    export: export::SessionExport,
    sessions: Sessions,
    frame_error: String,
}

static BRIDGE: LazyLock<Mutex<Bridge>> = LazyLock::new(Mutex::default);

#[derive(Deserialize)]
struct Request {
    op: String,
    root: Option<String>,
    address: Option<String>,
    #[serde(rename = "recordHistory")]
    record_history: Option<bool>,
    session: Option<u64>,
    path: Option<String>,
}

fn dispatch(request: Request) -> Result<Value, String> {
    let mut bridge = BRIDGE.lock().map_err(|_| "bridge lock poisoned")?;
    match request.op.as_str() {
        "start" => {
            let root = Path::new(request.root.as_deref().ok_or("missing cache path")?);
            if !root.is_absolute() || root.file_name().and_then(|s| s.to_str()) != Some("PvdStudio")
            {
                return Err("expected an absolute PvdStudio cache directory".into());
            }
            bridge.sessions.start(
                root,
                request.address.as_deref().ok_or("missing address")?,
                request.record_history.unwrap_or(true),
            )?;
            bridge.frame_error.clear();
        }
        "stop" => bridge.sessions.stop()?,
        "clear" => return Ok(json!({"ok":true, "released":bridge.sessions.clear()?})),
        "export" => {
            let id = request.session.ok_or("missing session ID")?;
            let path = request.path.ok_or("missing capture file path")?;
            let snapshot = bridge.sessions.snapshot(id)?;
            bridge.export.start(id, path.into(), snapshot)?;
        }
        "drainExport" => bridge.export.cancel_and_join(),
        "release" => {
            bridge.sessions.release_view();
            bridge.frame_error.clear();
        }
        "status" => {
            let mut status = bridge.sessions.status();
            status["frameError"] = json!(bridge.frame_error);
            status["export"] = bridge.export.status();
            return Ok(json!({"ok":true,"status":status}));
        }
        "shutdown" => *bridge = Bridge::default(),
        _ => return Err("unknown bridge operation".into()),
    }
    Ok(json!({"ok":true}))
}

/// Dispatches a UTF-8 command. The caller owns the returned string.
///
/// # Safety
/// A non-null input must point to readable, NUL-terminated bytes for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_request(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        if input.is_null() {
            return Err("missing command".to_string());
        }
        let bytes = unsafe { CStr::from_ptr(input) }.to_bytes();
        let request = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        dispatch(request)
    })
    .unwrap_or_else(|_| Err("native bridge panicked".into()));
    let value = result.unwrap_or_else(|error| json!({"ok":false,"error":error}));
    CString::new(value.to_string())
        .expect("JSON escapes NUL")
        .into_raw()
}

/// # Safety
/// Pass null or a live string returned by `pvd_request`, exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}

/// A frame buffer belongs to the caller until pvd_frame_free receives the same pointer and length.
/// Pass `u64::MAX` as the index to request the latest completed frame.
///
/// # Safety
/// A non-null length pointer must be aligned and writable for one i32.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_frame(id: u64, index: u64, room: u64, length: *mut i32) -> *mut u8 {
    unsafe { frame_buffer(id, index, room, FrameFormat::Inline, length) }
}

/// Returns the published frame count, or -1 when the catalog is unavailable.
#[unsafe(no_mangle)]
pub extern "C" fn pvd_frame_count(id: u64) -> i64 {
    BRIDGE
        .try_lock()
        .ok()
        .and_then(|b| b.sessions.frame_count(id))
        .unwrap_or(-1)
}

/// Encodes a PVS2 frame relative to the last acknowledged frame when available.
///
/// # Safety
/// A non-null length pointer must be aligned and writable for one i32.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_frame_cached(
    id: u64,
    index: u64,
    room: u64,
    base: u64,
    length: *mut i32,
) -> *mut u8 {
    unsafe { frame_buffer(id, index, room, FrameFormat::Cached(base), length) }
}

/// Encodes a PVS3 frame with one definition per newly referenced mesh resource.
///
/// # Safety
/// A non-null length pointer must be aligned and writable for one i32.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_frame_resources(
    id: u64,
    index: u64,
    room: u64,
    base: u64,
    length: *mut i32,
) -> *mut u8 {
    unsafe { frame_buffer(id, index, room, FrameFormat::Resources(base, None), length) }
}

/// Encodes a PVS3 frame using a spherical display region in capture space.
/// Mode 1 uses the supplied center. Mode 2 uses the specified actor's position in this frame.
///
/// # Safety
/// A non-null length pointer must be aligned and writable for one i32.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_frame_resources_near(
    id: u64,
    index: u64,
    room: u64,
    base: u64,
    mode: u32,
    actor: u64,
    generation: u32,
    x: f32,
    y: f32,
    z: f32,
    radius: f32,
    length: *mut i32,
) -> *mut u8 {
    let focus = FocusRequest {
        mode,
        actor,
        generation,
        center: [x, y, z],
        radius,
    };
    unsafe {
        frame_buffer(
            id,
            index,
            room,
            FrameFormat::Resources(base, Some(focus)),
            length,
        )
    }
}

/// Encodes shapes and rays in a PVS4 frame. Shape packets without room for ray metadata use PVS3.
/// Mode 0 uses the full scene. Modes 1 and 2 match `pvd_frame_resources_near`.
///
/// # Safety
/// A non-null length pointer must be aligned and writable for one i32.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_frame_view(
    id: u64,
    index: u64,
    room: u64,
    base: u64,
    mode: u32,
    actor: u64,
    generation: u32,
    x: f32,
    y: f32,
    z: f32,
    radius: f32,
    length: *mut i32,
) -> *mut u8 {
    let focus = (mode != 0).then_some(FocusRequest {
        mode,
        actor,
        generation,
        center: [x, y, z],
        radius,
    });
    unsafe { frame_buffer(id, index, room, FrameFormat::View(base, focus), length) }
}

/// Encodes collision shapes, optional rays, and shape queries in a PVS5 frame.
/// Mode 0 uses the full scene. Modes 1 and 2 match `pvd_frame_resources_near`.
/// Frames without room for the two overlay headers use PVS3.
///
/// # Safety
/// A non-null length pointer must be aligned and writable for one i32.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_frame_queries(
    id: u64,
    index: u64,
    room: u64,
    base: u64,
    mode: u32,
    actor: u64,
    generation: u32,
    x: f32,
    y: f32,
    z: f32,
    radius: f32,
    include_rays: u32,
    length: *mut i32,
) -> *mut u8 {
    let focus = (mode != 0).then_some(FocusRequest {
        mode,
        actor,
        generation,
        center: [x, y, z],
        radius,
    });
    unsafe {
        frame_buffer(
            id,
            index,
            room,
            FrameFormat::Queries(base, focus, include_rays != 0),
            length,
        )
    }
}

unsafe fn frame_buffer(
    id: u64,
    index: u64,
    room: u64,
    format: FrameFormat,
    length: *mut i32,
) -> *mut u8 {
    if length.is_null() {
        return ptr::null_mut();
    }
    unsafe { *length = 0 };
    std::panic::catch_unwind(|| {
        let mut job = {
            let mut bridge = BRIDGE.lock().ok()?;
            match bridge.sessions.prepare_frame(id) {
                Ok(job) => job,
                Err(error) => {
                    bridge.frame_error = error;
                    return None;
                }
            }
        };
        let result = job.read(index, room, format);
        let mut bridge = BRIDGE.lock().ok()?;
        if !bridge.sessions.finish_frame(&mut job, result.is_ok()) {
            return None;
        }
        match result {
            Ok(data) => {
                bridge.frame_error.clear();
                drop(bridge);
                let data = data.into_boxed_slice();
                unsafe { *length = i32::try_from(data.len()).ok()? };
                Some(Box::into_raw(data).cast::<u8>())
            }
            Err(error) => {
                bridge.frame_error = error;
                None
            }
        }
    })
    .ok()
    .flatten()
    .unwrap_or(ptr::null_mut())
}

/// # Safety
/// Pass null or the unchanged pointer and length from a successful frame request, exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvd_frame_free(data: *mut u8, length: i32) {
    if !data.is_null() && length >= 0 {
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(data, length as usize)) });
    }
}
