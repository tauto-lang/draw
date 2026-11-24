#![forbid(unsafe_code)]

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::ThreadId,
    time::Duration,
};
mod keys;
use keys::Logical;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    platform::pump_events::{EventLoopExtPumpEvents, PumpStatus},
    window::{Fullscreen, Window, WindowAttributes, WindowId},
};

type R<T> = Result<T, String>;

#[derive(Debug, PartialEq)]
pub enum WinitMessage {
    Idle,
    Close,
    Resize(u32, u32),
    Scale(u32),
    Redraw,
    Focus(bool),
    Key(String, Logical, bool, bool),
    WheelLines(i64, i64),
    WheelPixels(i64, i64),
    Pointer(i64, i64),
    Button(String, bool),
    Left,
}

fn scale_units(factor: f64) -> u32 {
    (factor * 1000.0).round() as u32
}

fn wheel(delta: MouseScrollDelta) -> WinitMessage {
    let milli = |v: f64| (v * 1000.0).round() as i64;
    match delta {
        MouseScrollDelta::LineDelta(x, y) => {
            WinitMessage::WheelLines(milli(x.into()), milli(y.into()))
        }
        MouseScrollDelta::PixelDelta(p) => WinitMessage::WheelPixels(milli(p.x), milli(p.y)),
    }
}

fn translate(event: WindowEvent) -> Option<WinitMessage> {
    use WinitMessage as M;
    Some(match event {
        WindowEvent::CloseRequested | WindowEvent::Destroyed => M::Close,
        WindowEvent::Resized(s) => M::Resize(s.width, s.height),
        WindowEvent::ScaleFactorChanged { scale_factor, .. } => M::Scale(scale_units(scale_factor)),
        WindowEvent::RedrawRequested => M::Redraw,
        WindowEvent::Focused(f) => M::Focus(f),
        WindowEvent::KeyboardInput { event, .. } => M::Key(
            keys::physical(event.physical_key),
            keys::logical(event.logical_key),
            event.state.is_pressed(),
            event.repeat,
        ),
        WindowEvent::MouseWheel { delta, .. } => wheel(delta),
        WindowEvent::CursorMoved { position, .. } => {
            M::Pointer(position.x.round() as i64, position.y.round() as i64)
        }
        WindowEvent::CursorLeft { .. } => M::Left,
        WindowEvent::MouseInput { state, button, .. } => M::Button(
            match button {
                MouseButton::Left => "left".into(),
                MouseButton::Right => "right".into(),
                MouseButton::Middle => "middle".into(),
                MouseButton::Back => "back".into(),
                MouseButton::Forward => "forward".into(),
                MouseButton::Other(n) => format!("other-{n}"),
            },
            state == ElementState::Pressed,
        ),
        _ => return None,
    })
}

#[derive(Default)]
struct App {
    attributes: Option<WindowAttributes>,
    window: Option<Arc<Window>>,
    error: Option<String>,
    events: VecDeque<WinitMessage>,
}
impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if let Some(attrs) = self.attributes.take() {
            match el.create_window(attrs) {
                Ok(w) => self.window = Some(Arc::new(w)),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let Some(event) = translate(event) {
            self.events.push_back(event);
        }
    }
}

/// Main-thread-bound state: the event loop and the window stay in a
/// thread-local of the thread that opened them. Tauto holds only the
/// `WinitWindow` handle, which is `Send + Sync`; calls from any other thread
/// are refused with an `Err`.
struct Owned {
    app: App,
    event_loop: EventLoop<()>,
}
thread_local! {
    static OWNED: RefCell<HashMap<u64, Owned>> = RefCell::new(HashMap::new());
}
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
const WRONG_THREAD: &str = "winit window must be called on the thread that opened the window";

pub struct WinitWindow {
    id: u64,
    owner: ThreadId,
    device: Option<wgpu_bridge::Device>,
}
impl WinitWindow {
    fn with<T>(&self, f: impl FnOnce(&mut Owned) -> R<T>) -> R<T> {
        if self.owner != std::thread::current().id() {
            return Err(WRONG_THREAD.into());
        }
        OWNED.with(|m| match m.borrow_mut().get_mut(&self.id) {
            Some(o) => f(o),
            None => Err("window is closed".into()),
        })
    }
    fn with_window<T>(&self, f: impl FnOnce(&Window) -> T) -> R<T> {
        self.with(|o| {
            o.app
                .window
                .as_deref()
                .map(f)
                .ok_or_else(|| "window is closed".into())
        })
    }
}
/// The attributes of a new window: `Some(size)` is a plain window of that
/// physical size, `None` is borderless fullscreen on the current monitor.
fn attributes(title: &str, size: Option<(u32, u32)>) -> R<WindowAttributes> {
    let attrs = Window::default_attributes().with_title(title);
    match size {
        Some((0, _) | (_, 0)) => Err("initial physical size must be positive".into()),
        Some((width, height)) => Ok(attrs.with_inner_size(PhysicalSize::new(width, height))),
        None => Ok(attrs.with_fullscreen(Some(Fullscreen::Borderless(None)))),
    }
}
pub fn win_open(title: &str, width: u32, height: u32) -> R<WinitWindow> {
    open_with(attributes(title, Some((width, height)))?)
}
pub fn win_open_fullscreen(title: &str) -> R<WinitWindow> {
    open_with(attributes(title, None)?)
}
fn open_with(attrs: WindowAttributes) -> R<WinitWindow> {
    #[cfg(target_os = "macos")]
    if objc2_foundation::MainThreadMarker::new().is_none() {
        return Err("winit requires the macOS process main thread".into());
    }
    let mut builder = EventLoop::builder();
    // On Linux and Windows the Tauto entry thread may own this event loop.
    #[cfg(target_os = "linux")]
    winit::platform::x11::EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
    #[cfg(target_os = "windows")]
    winit::platform::windows::EventLoopBuilderExtWindows::with_any_thread(&mut builder, true);
    let mut el = builder.build().map_err(|e| e.to_string())?;
    let mut app = App {
        attributes: Some(attrs),
        ..Default::default()
    };
    while app.window.is_none() {
        let status = el.pump_app_events(Some(Duration::from_millis(25)), &mut app);
        if let Some(e) = app.error.take() {
            return Err(e);
        }
        if matches!(status, PumpStatus::Exit(_)) {
            return Err("event loop exited before window creation".into());
        }
    }
    let window = app.window.as_ref().ok_or("window was not created")?;
    let size = window.inner_size();
    let (device, _) = wgpu_bridge::open_window(Arc::clone(window), size.width, size.height)?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let owned = Owned {
        app,
        event_loop: el,
    };
    OWNED.with(|m| m.borrow_mut().insert(id, owned));
    Ok(WinitWindow {
        id,
        owner: std::thread::current().id(),
        device: Some(device),
    })
}
pub fn win_device(w: &mut WinitWindow) -> R<wgpu_bridge::Device> {
    w.with(|_| Ok(()))?;
    w.device.take().ok_or_else(|| "device already taken".into())
}
pub fn win_pull(w: &mut WinitWindow, timeout_ms: u32) -> R<WinitMessage> {
    w.with(|o| {
        if o.app.events.is_empty() {
            let status = o
                .event_loop
                .pump_app_events(Some(Duration::from_millis(timeout_ms.into())), &mut o.app);
            if matches!(status, PumpStatus::Exit(_)) {
                return Ok(WinitMessage::Close);
            }
        }
        Ok(o.app.events.pop_front().unwrap_or(WinitMessage::Idle))
    })
}
pub fn win_redraw(w: &mut WinitWindow) -> R<()> {
    w.with_window(|w| w.request_redraw())
}
pub fn win_size(w: &mut WinitWindow) -> R<(u32, u32)> {
    w.with_window(|w| {
        let s = w.inner_size();
        (s.width, s.height)
    })
}
/// Switches between borderless fullscreen on the window's current monitor
/// and a plain window. The new size arrives as a resize event.
pub fn win_set_fullscreen(w: &mut WinitWindow, on: bool) -> R<()> {
    w.with_window(|w| w.set_fullscreen(on.then_some(Fullscreen::Borderless(None))))
}
pub fn win_scale(w: &mut WinitWindow) -> R<u32> {
    w.with_window(|w| scale_units(w.scale_factor()))
}
pub fn win_close(w: WinitWindow) -> R<()> {
    w.with_window(|w| w.set_visible(false))?;
    let owned = OWNED.with(|m| m.borrow_mut().remove(&w.id));
    drop(owned);
    Ok(())
}

pub fn msg_kind(m: &WinitMessage) -> u32 {
    use WinitMessage::*;
    match m {
        Idle => 0,
        Close => 1,
        Resize(..) => 2,
        Scale(_) => 3,
        Redraw => 4,
        Focus(_) => 5,
        Key(..) => 6,
        Pointer(..) => 7,
        Button(..) => 8,
        Left => 9,
        WheelLines(..) => 10,
        WheelPixels(..) => 11,
    }
}
pub fn msg_pair(m: &WinitMessage) -> (u32, u32) {
    match m {
        WinitMessage::Resize(x, y) => (*x, *y),
        WinitMessage::Scale(s) => (*s, 0),
        _ => (0, 0),
    }
}
pub fn msg_position(m: &WinitMessage) -> (i64, i64) {
    match m {
        WinitMessage::Pointer(x, y)
        | WinitMessage::WheelLines(x, y)
        | WinitMessage::WheelPixels(x, y) => (*x, *y),
        _ => (0, 0),
    }
}
pub fn msg_name(m: &WinitMessage) -> String {
    match m {
        WinitMessage::Key(s, ..) | WinitMessage::Button(s, _) => s.clone(),
        _ => String::new(),
    }
}
pub fn msg_logical_kind(m: &WinitMessage) -> u32 {
    match m {
        WinitMessage::Key(_, Logical::Character(_), ..) => 1,
        WinitMessage::Key(_, Logical::Named(_), ..) => 2,
        WinitMessage::Key(_, Logical::Dead(_), ..) => 3,
        _ => 0,
    }
}
pub fn msg_logical_text(m: &WinitMessage) -> String {
    match m {
        WinitMessage::Key(_, Logical::Character(s) | Logical::Named(s) | Logical::Dead(s), ..) => {
            s.clone()
        }
        _ => String::new(),
    }
}
pub fn msg_flag(m: &WinitMessage) -> bool {
    match m {
        WinitMessage::Focus(b) | WinitMessage::Button(_, b) | WinitMessage::Key(_, _, b, _) => *b,
        _ => false,
    }
}
pub fn msg_repeat(m: &WinitMessage) -> bool {
    matches!(m, WinitMessage::Key(_, _, _, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_size_and_scale_are_independent() {
        assert_eq!(
            translate(WindowEvent::Resized(PhysicalSize::new(801, 0))),
            Some(WinitMessage::Resize(801, 0))
        );
        assert_eq!(scale_units(1.25), 1250);
    }
    #[test]
    fn close_and_focus_are_distinct() {
        assert_eq!(
            translate(WindowEvent::CloseRequested),
            Some(WinitMessage::Close)
        );
        assert_eq!(
            translate(WindowEvent::Focused(false)),
            Some(WinitMessage::Focus(false))
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn wrong_thread_is_rejected_before_appkit() {
        std::thread::spawn(|| {
            assert!(
                win_open("test", 32, 32)
                    .err()
                    .is_some_and(|e| e.contains("main thread"))
            )
        })
        .join()
        .unwrap();
    }
    #[test]
    fn fullscreen_is_borderless_on_the_current_monitor() {
        let full = attributes("t", None).unwrap();
        assert_eq!(full.fullscreen, Some(Fullscreen::Borderless(None)));
        let plain = attributes("t", Some((640, 320))).unwrap();
        assert_eq!(plain.fullscreen, None);
        assert_eq!(plain.inner_size, Some(PhysicalSize::new(640, 320).into()));
        assert!(attributes("t", Some((0, 320))).is_err());
    }
    #[test]
    fn handle_and_messages_cross_threads() {
        fn crosses_threads<T: Send + Sync>() {}
        crosses_threads::<WinitWindow>();
        crosses_threads::<WinitMessage>();
    }
    #[test]
    fn foreign_thread_gets_an_error() {
        let mut w = WinitWindow {
            id: 0,
            owner: std::thread::current().id(),
            device: None,
        };
        assert_eq!(win_size(&mut w), Err("window is closed".into()));
        std::thread::scope(|s| {
            s.spawn(|| assert_eq!(win_scale(&mut w), Err(WRONG_THREAD.into())));
        });
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use winit::keyboard::{Key, KeyCode, NamedKey};
    #[test]
    fn physical_position_is_independent_of_layout() {
        assert_eq!(keys::physical(KeyCode::KeyA.into()), "Code(KeyA)");
        assert_eq!(
            keys::logical(Key::Character("ち".into())),
            Logical::Character("ち".into())
        );
        assert_eq!(
            keys::logical(Key::Named(NamedKey::Enter)),
            Logical::Named("Enter".into())
        );
        assert_eq!(keys::logical(Key::Dead(None)), Logical::Dead(String::new()));
    }
    #[test]
    fn scroll_units_preserve_signed_fractional_deltas() {
        assert_eq!(
            wheel(MouseScrollDelta::LineDelta(-0.25, 1.5)),
            WinitMessage::WheelLines(-250, 1500)
        );
        assert_eq!(
            wheel(MouseScrollDelta::PixelDelta(
                winit::dpi::PhysicalPosition::new(0.5, -2.25)
            )),
            WinitMessage::WheelPixels(500, -2250)
        );
    }
}
