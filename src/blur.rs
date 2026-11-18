//! Blur behind the window, asked of the compositor with `ext-background-effect-v1`.
//!
//! GNOME 51 (Mutter) and Plasma 6.7 blur what lies behind a surface region that the
//! application names. The compositor does the blur on its own GPU pass. Rustty only
//! sends the region, so a translucent frame shows a frosted desktop instead of a sharp one.
//!
//! The protocol objects live on a private event queue over winit's own `wl_display`.
//! The region is double-buffered: it takes effect with the next `wl_surface.commit`,
//! which the Vulkan present of the next frame does.

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_compositor::WlCompositor, wl_region::WlRegion, wl_registry::WlRegistry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, Capability, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::window::Window;

/// What the region was last set to, in surface-local (logical) pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    width: i32,
    height: i32,
    radius: i32,
}

#[derive(Default)]
struct State {
    blur: bool,
}

pub struct Blur {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    compositor: WlCompositor,
    effect: ExtBackgroundEffectSurfaceV1,
    applied: Option<Shape>,
}

impl Blur {
    /// `None` on X11 or when the compositor does not offer the protocol.
    pub fn new(window: &Window) -> Option<Self> {
        let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
            return None;
        };
        let RawWindowHandle::Wayland(surface) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: winit owns the display and the surface and keeps both alive as long as the
        // window, which outlives this object. The foreign backend never closes the display.
        let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) });
        let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
            Ok(pair) => pair,
            Err(err) => {
                tracing::debug!(%err, "no Wayland registry for blur");
                return None;
            }
        };
        let qh = queue.handle();
        let manager: ExtBackgroundEffectManagerV1 = match globals.bind(&qh, 1..=1, ()) {
            Ok(manager) => manager,
            Err(_) => {
                tracing::info!("the compositor has no ext-background-effect-v1, glass stays without blur");
                return None;
            }
        };
        let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).ok()?;
        // SAFETY: the pointer is winit's live wl_surface, a wl_proxy of interface wl_surface.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast()) }.ok()?;
        let surface = WlSurface::from_id(&conn, id).ok()?;
        let effect = manager.get_background_effect(&surface, &qh, ());
        let mut state = State::default();
        // The capabilities arrive right after the bind.
        if let Err(err) = queue.roundtrip(&mut state) {
            tracing::warn!(%err, "blur roundtrip");
            return None;
        }
        tracing::info!(blur = state.blur, "ext-background-effect-v1");
        Some(Self { conn, queue, state, compositor, effect, applied: None })
    }

    /// Blurs the window's rounded rectangle, or removes the blur when `on` is false.
    /// Sizes are physical. Nothing is sent when the shape did not change.
    pub fn sync(&mut self, on: bool, width: u32, height: u32, radius: f32, scale: f64) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        let shape = (on && self.state.blur).then(|| Shape {
            width: (f64::from(width) / scale).round() as i32,
            height: (f64::from(height) / scale).round() as i32,
            radius: (f64::from(radius) / scale).round() as i32,
        });
        if shape == self.applied {
            return;
        }
        match shape {
            Some(shape) if shape.width > 0 && shape.height > 0 => {
                let qh = self.queue.handle();
                let region = self.compositor.create_region(&qh, ());
                for (x, y, w, h) in rounded_rects(shape.width, shape.height, shape.radius) {
                    region.add(x, y, w, h);
                }
                self.effect.set_blur_region(Some(&region));
                region.destroy();
            }
            _ => self.effect.set_blur_region(None),
        }
        if let Err(err) = self.conn.flush() {
            tracing::warn!(%err, "blur region was not sent");
            return;
        }
        self.applied = shape;
    }

    pub fn supported(&self) -> bool {
        self.state.blur
    }
}

impl Drop for Blur {
    fn drop(&mut self) {
        self.effect.destroy();
        let _ = self.conn.flush();
    }
}

/// A rounded rectangle as a union of rectangles: one band per pixel row inside each
/// corner, and one block for the middle. The blur then stops at the visible corners.
fn rounded_rects(width: i32, height: i32, radius: i32) -> Vec<(i32, i32, i32, i32)> {
    let radius = radius.clamp(0, width.min(height) / 2);
    let mut rects = Vec::with_capacity(2 * radius as usize + 1);
    for row in 0..radius {
        let dy = f64::from(radius) - f64::from(row) - 0.5;
        let inset = (f64::from(radius) - (f64::from(radius).powi(2) - dy * dy).max(0.0).sqrt()).ceil() as i32;
        let w = width - 2 * inset;
        if w > 0 {
            rects.push((inset, row, w, 1));
            rects.push((inset, height - 1 - row, w, 1));
        }
    }
    rects.push((0, radius, width, height - 2 * radius));
    rects
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            state.blur = flags.into_result().is_ok_and(|flags| flags.contains(Capability::Blur));
        }
    }
}

macro_rules! no_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for State {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

no_events!(WlCompositor, WlRegion, ExtBackgroundEffectSurfaceV1);

#[cfg(test)]
mod tests {
    use super::rounded_rects;

    #[test]
    fn region_covers_the_middle_and_trims_the_corners() {
        let rects = rounded_rects(200, 100, 10);
        assert!(rects.contains(&(0, 10, 200, 80)));
        let top = rects.iter().find(|rect| rect.1 == 0).expect("top row");
        assert!(top.0 >= 6 && top.0 <= 10, "{top:?}");
        let near = rects.iter().find(|rect| rect.1 == 9).expect("row next to the middle");
        assert!(near.0 <= 1, "{near:?}");
        assert!(rects.iter().any(|rect| rect.1 == 99));
        let area: i32 = rects.iter().map(|rect| rect.2 * rect.3).sum();
        assert!(area < 200 * 100 && area > 200 * 100 - 4 * 100);
    }

    #[test]
    fn square_corners_send_one_rectangle() {
        assert_eq!(rounded_rects(300, 200, 0), vec![(0, 0, 300, 200)]);
    }
}
