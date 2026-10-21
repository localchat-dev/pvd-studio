use bevy::{
    ecs::system::NonSendMarker,
    prelude::*,
    window::{WindowCreated, WindowScaleFactorChanged},
    winit::WINIT_WINDOWS,
};
use winit::{
    dpi::PhysicalSize,
    platform::windows::{IconExtWindows, WindowExtWindows},
    window::Icon,
};

pub fn apply_window_icons(
    mut created: MessageReader<WindowCreated>,
    mut scale_changed: MessageReader<WindowScaleFactorChanged>,
    _main_thread: NonSendMarker,
) {
    for entity in created
        .read()
        .map(|event| event.window)
        .chain(scale_changed.read().map(|event| event.window))
    {
        WINIT_WINDOWS.with_borrow(|windows| {
            let Some(window) = windows.get_window(entity) else {
                return;
            };
            let size = (16.0 * window.scale_factor()).round().clamp(16.0, 256.0) as u32;
            let small = Icon::from_resource(1, Some(PhysicalSize::new(size, size)));
            let large = Icon::from_resource(1, Some(PhysicalSize::new(256, 256)));
            match (small, large) {
                (Ok(small), Ok(large)) => {
                    window.set_window_icon(Some(small));
                    window.set_taskbar_icon(Some(large));
                }
                (Err(error), _) | (_, Err(error)) => {
                    error!("Could not load the embedded pvd-studio icon: {error}");
                }
            }
        });
    }
}
