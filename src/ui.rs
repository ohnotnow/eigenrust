//! The radio's face: a 1960s car radio drawn with GPUI.

use std::f32::consts::PI;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Animation, AnimationExt as _, App, Application, Background, Bounds, ClipboardItem, Context,
    Div, ExternalPaths, FontWeight, KeyBinding, Menu, MenuItem, PathPromptOptions,
    ScrollWheelEvent, SharedString, Stateful, TitlebarOptions, Window, WindowBounds,
    WindowOptions, actions, canvas, div, fill, linear_color_stop, linear_gradient, point,
    prelude::*, px, relative, rgb, rgba, size,
};

use crate::mixer::Mixer;
use crate::playlist;
use crate::radio::{Command, Event};
use crate::server::{self, Server};
use crate::settings::{self, Settings};

const DIAL_NUMBERS: [&str; 8] = ["54", "6", "7", "8", "10", "12", "14", "16"];
const TICKS: usize = 70;
const RIB_SPACING: f32 = 7.0;
/// The needle keeps this far from each end of the dial.
const DIAL_MARGIN: f32 = 0.03;

/// About as long as the audio takes to fade through static, so the needle lands as the
/// new station comes in.
const SPIN_TIME: Duration = Duration::from_millis(3000);
/// The share of the spin spent swinging about before the needle homes in.
const SWING_SHARE: f32 = 0.6;
const MIN_SWING: f32 = 0.25;

/// The click-stops on the "how long to stay" knob, in seconds.
const STAY_STOPS: [u64; 9] = [15, 30, 60, 120, 300, 600, 900, 1800, 3600];
/// How far the knob's marker turns from straight up at each end of its travel, in degrees.
const KNOB_SWEEP: f32 = 135.0;
/// How much scrolling over the knob moves it one stop.
const SCROLL_PER_STOP: f32 = 40.0;
/// How long the stay knob must be left alone before its new setting takes effect.
const STAY_SETTLE: Duration = Duration::from_millis(1500);
/// How long a passing message stays in the station window.
const NOTICE_TIME: Duration = Duration::from_secs(4);

const KNOB: f32 = 88.0;
const KNOB_FACE: f32 = 68.0;
/// The width of dark rim left showing round the edge of a knob's face.
const KNOB_RIM: f32 = 1.0;
const BAKELITE: (u32, u32) = (0xfaf0d6, 0xcbb688);
const BAKELITE_DIM: (u32, u32) = (0xb5a987, 0x8a7e5f);

const INK: u32 = 0x2a1a0c;
const CREAM: u32 = 0xf3e6c4;
const NEEDLE: u32 = 0xff4a26;
const AMBER: (u32, u32) = (0xffbe55, 0xf08f1d);
const KEY_CREAM: (u32, u32) = (0xf8eed3, 0xd6c397);

/// Everything the face needs to drive the radio.
pub struct Setup {
    pub mixer: Arc<Mixer>,
    pub rate: u32,
    pub muted: Arc<AtomicBool>,
    pub commands: Sender<Command>,
    pub events: UnboundedReceiver<Event>,
    pub server: Option<Server>,
    pub server_address: String,
    /// How long each station plays to begin with.
    pub stay: Duration,
    pub settings: Settings,
    pub settings_path: PathBuf,
}

actions!(eigenradio, [Quit]);

/// Opens the radio's window and runs until it is closed.
pub fn run(setup: Setup) {
    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // Without a menu of its own a Mac app shows no menu bar and ignores Cmd+Q.
        // Windows and Linux have no such bar, but get the usual Ctrl+Q.
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        let quit_key = if cfg!(target_os = "macos") { "cmd-q" } else { "ctrl-q" };
        cx.bind_keys([KeyBinding::new(quit_key, Quit, None)]);
        cx.set_menus(vec![Menu {
            name: "EigenRadio".into(),
            items: vec![MenuItem::action("Quit EigenRadio", Quit)],
        }]);

        let bounds = Bounds::centered(None, size(px(860.), px(300.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("EigenRadio".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| RadioView::new(setup, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}

struct RadioView {
    /// Where the needle travels on the current tune, as fractions of the dial's width.
    /// The first entry is where it started and the last is where it comes to rest.
    path: Vec<f32>,
    tunes: usize,
    text: SharedString,
    /// A passing message shown in place of the station name.
    notice: Option<SharedString>,
    notices: usize,
    paused: bool,
    /// Which of `STAY_STOPS` the knob is on.
    stay: usize,
    stay_turns: usize,
    /// Scrolling over the knob that has not yet added up to a whole stop.
    scroll: f32,
    mixer: Arc<Mixer>,
    rate: u32,
    muted: Arc<AtomicBool>,
    commands: Sender<Command>,
    server: Option<Server>,
    server_address: String,
    settings: Settings,
    settings_path: PathBuf,
}

impl RadioView {
    fn new(setup: Setup, cx: &mut Context<Self>) -> Self {
        let mut events = setup.events;
        cx.spawn(async move |view, cx| {
            while let Some(event) = events.next().await {
                if view.update(cx, |view, cx| view.on_event(event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            path: vec![DIAL_MARGIN],
            tunes: 0,
            text: "".into(),
            notice: None,
            notices: 0,
            paused: false,
            stay: nearest_stop(setup.stay.as_secs()),
            stay_turns: 0,
            scroll: 0.0,
            mixer: setup.mixer,
            rate: setup.rate,
            muted: setup.muted,
            commands: setup.commands,
            server: setup.server,
            server_address: setup.server_address,
            settings: setup.settings,
            settings_path: setup.settings_path,
        }
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Spin(position) => {
                let target = DIAL_MARGIN + (1.0 - 2.0 * DIAL_MARGIN) * position;
                let mut path = vec![*self.path.last().unwrap()];
                for _ in 0..fastrand::usize(1..=2) {
                    path.push(swing_away_from(*path.last().unwrap()));
                }
                path.push(target);
                self.path = path;
                self.tunes += 1;
            }
            Event::Show(text) => self.text = text.into(),
        }
        cx.notify();
    }

    fn send(&self, command: Command) {
        // The radio only stops listening once it has shut down, and then there is nothing to do.
        let _ = self.commands.send(command);
    }

    fn retune(&mut self, cx: &mut Context<Self>) {
        self.paused = false;
        self.send(Command::Retune);
        cx.notify();
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        self.send(Command::SetPaused(self.paused));
        cx.notify();
    }

    /// Runs `then` after `delay`, unless `counter` has moved on by then. Callers bump their
    /// counter first, so of several calls in quick succession only the last one's `then` runs.
    fn later(
        &self,
        delay: Duration,
        counter: fn(&Self) -> usize,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let ticket = counter(self);
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(delay).await;
            view.update(cx, |view, cx| {
                if counter(view) == ticket {
                    then(view, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Shows a message in the station window for a few seconds.
    fn flash(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
        self.notices += 1;
        let clear = |view: &mut Self, cx: &mut Context<Self>| {
            view.notice = None;
            cx.notify();
        };
        self.later(NOTICE_TIME, |view| view.notices, clear, cx);
        cx.notify();
    }

    fn toggle_cast(&mut self, cx: &mut Context<Self>) {
        if self.server.take().is_some() {
            self.flash("Casting off", cx);
            return;
        }
        match server::start(&self.server_address, self.mixer.clone(), self.rate) {
            Ok(server) => {
                self.flash(format!("Click to copy  {}", server.url()), cx);
                self.server = Some(server);
            }
            Err(e) => self.flash(format!("Cannot cast: {e}"), cx),
        }
    }

    /// Copies the cast address, but only when asked: the clipboard is the user's.
    fn copy_cast_address(&mut self, cx: &mut Context<Self>) {
        if let Some(server) = &self.server {
            cx.write_to_clipboard(ClipboardItem::new_string(server.url().to_string()));
            self.flash("Address copied", cx);
        }
    }

    /// Turns the stay knob by `stops` (negative for shorter), stopping at either end.
    ///
    /// The knob and its readout move at once, but the radio is only told once the knob has
    /// been left alone for a moment, so passing through a short time on the way to another
    /// does not drop the station that is playing.
    fn turn_stay(&mut self, stops: isize, cx: &mut Context<Self>) {
        let last = STAY_STOPS.len() as isize - 1;
        let stop = (self.stay as isize + stops).clamp(0, last) as usize;
        if stop != self.stay {
            self.stay = stop;
            self.stay_turns += 1;
            let apply = |view: &mut Self, _: &mut Context<Self>| {
                let seconds = STAY_STOPS[view.stay];
                view.send(Command::SetStay(Duration::from_secs(seconds)));
                view.settings.stay_seconds = Some(seconds);
                view.save_settings();
            };
            self.later(STAY_SETTLE, |view| view.stay_turns, apply, cx);
            cx.notify();
        }
    }

    fn scroll_stay(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        self.scroll += f32::from(event.delta.pixel_delta(px(SCROLL_PER_STOP)).y);
        let stops = (self.scroll / SCROLL_PER_STOP).trunc();
        if stops != 0.0 {
            self.scroll -= stops * SCROLL_PER_STOP;
            self.turn_stay(stops as isize, cx);
        }
    }

    fn save_settings(&self) {
        if let Err(e) = settings::save(&self.settings_path, &self.settings) {
            eprintln!("Could not save settings: {e}");
        }
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted.fetch_xor(true, Ordering::Relaxed);
        cx.notify();
    }

    fn choose_playlist(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |view, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await
                && let Some(path) = paths.into_iter().next()
            {
                view.update(cx, |view, cx| view.load_playlist(path, cx)).ok();
            }
        })
        .detach();
    }

    fn load_playlist(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match playlist::load_playlist(&path) {
            Ok(stations) if stations.is_empty() => self.flash("No stations in that file", cx),
            Ok(stations) => {
                self.flash(format!("{} stations loaded", stations.len()), cx);
                self.paused = false;
                self.send(Command::Playlist(stations));
                self.settings.playlist = Some(path);
                self.save_settings();
            }
            Err(e) => self.flash(e, cx),
        }
    }

    fn dial(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let path = self.path.clone();
        let needle = div()
            .absolute()
            .top(px(4.))
            .bottom(px(4.))
            .w(px(4.))
            .rounded_sm()
            .bg(rgb(NEEDLE))
            .shadow_md()
            // A new id each tune restarts the animation.
            .with_animation(("needle", self.tunes), Animation::new(SPIN_TIME), move |needle, time| {
                needle.left(relative(needle_position(&path, time)))
            });

        let scale = div()
            .relative()
            .h(px(66.))
            .mx(px(26.))
            .child(
                div()
                    .pt(px(6.))
                    .flex()
                    .justify_between()
                    .font_family("DIN Condensed")
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(30.))
                    .text_color(rgb(CREAM))
                    .children(DIAL_NUMBERS.map(|number| div().child(number))),
            )
            .child(tick_marks().absolute().left_0().right_0().bottom(px(6.)).h(px(12.)))
            .child(needle);

        // Passing messages are longer than station names, so they are set a little smaller.
        let (text, text_size) = match &self.notice {
            Some(notice) => (notice.clone(), px(20.)),
            None => (self.text.clone(), px(26.)),
        };
        // How long each station plays, always on show at the end of the station window.
        let stay_readout = div()
            .flex_none()
            .w(px(64.))
            .border_l_2()
            .border_color(rgb(0x9c5a10))
            .bg(vertical((0xf5a93c, 0xdc7e14)))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(20.))
            .child(stay_label(STAY_STOPS[self.stay]));
        let station = div()
            .id("station")
            .h(px(50.))
            .mx(px(10.))
            .mb(px(10.))
            .rounded_sm()
            .overflow_hidden()
            .bg(vertical(AMBER))
            .flex()
            .font_weight(FontWeight::BOLD)
            .text_color(rgb(INK))
            .when(self.server.is_some(), |station| station.cursor_pointer())
            .on_click(cx.listener(|view, _, _, cx| view.copy_cast_address(cx)))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(text_size)
                    .child(div().truncate().child(text)),
            )
            .child(stay_readout);

        // A dark chrome bezel around dark glass.
        div().p(px(5.)).rounded_lg().bg(vertical((0x8a9097, 0x33373c))).shadow_lg().child(
            div()
                .rounded_md()
                .overflow_hidden()
                .bg(vertical((0x0f0d0a, 0x2a2116)))
                .child(scale)
                .child(station),
        )
    }

    fn keys(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let play_label = if self.paused { "PLAY" } else { "PAUSE" };
        div()
            .flex()
            .gap(px(10.))
            .child(key("tune", "TUNE", false).on_click(cx.listener(|view, _, _, cx| view.retune(cx))))
            .child(
                key("play", play_label, false)
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_pause(cx))),
            )
            .child(
                key("cast", "CAST", self.server.is_some())
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_cast(cx))),
            )
            .child(
                key("list", "LIST", false)
                    .on_click(cx.listener(|view, _, _, cx| view.choose_playlist(cx))),
            )
    }
}

impl Render for RadioView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Speakers: straight up when on, turned right down (and dimmed) when off.
        let muted = self.muted.load(Ordering::Relaxed);
        let volume_knob = knob_collar("volume")
            .cursor_pointer()
            .on_click(cx.listener(|view, _, _, cx| view.toggle_mute(cx)))
            .child(
                knob_face()
                    .child(
                        div()
                            .absolute()
                            .top(px(KNOB_RIM))
                            .left(px(KNOB_RIM))
                            .size(px(KNOB_FACE - 2.0 * KNOB_RIM))
                            .rounded_full()
                            .bg(vertical(if muted { BAKELITE_DIM } else { BAKELITE })),
                    )
                    .child(knob_marker(if muted { -KNOB_SWEEP } else { 0.0 })),
            );

        // How long to stay on each station. The left half of the knob turns it down and the
        // right half turns it up, and scrolling over it goes either way. A tick on the collar
        // marks each stop, with larger ones at the two ends of its travel.
        let last_stop = STAY_STOPS.len() - 1;
        let stay_knob = knob_collar("stay")
            .on_scroll_wheel(cx.listener(|view, event, _, cx| view.scroll_stay(event, cx)))
            .children((0..=last_stop).map(|stop| {
                let size = if stop == 0 || stop == last_stop { 7.0 } else { 4.0 };
                orbiting_dot(KNOB, 39.0, stop_angle(stop), size, CREAM)
            }))
            .child(
                knob_face()
                    .child(
                        knob_half("shorter", false)
                            .on_click(cx.listener(|view, _, _, cx| view.turn_stay(-1, cx))),
                    )
                    .child(
                        knob_half("longer", true)
                            .on_click(cx.listener(|view, _, _, cx| view.turn_stay(1, cx))),
                    )
                    .child(knob_marker(stop_angle(self.stay))),
            );

        // A walnut dashboard, with the radio's ribbed dark chrome face set into it.
        div()
            .size_full()
            .p(px(16.))
            .bg(vertical((0x4a2f1b, 0x2a190d)))
            .on_drop(cx.listener(|view, dropped: &ExternalPaths, _, cx| {
                if let Some(path) = dropped.paths().first() {
                    view.load_playlist(path.clone(), cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .size_full()
                    .rounded_lg()
                    .bg(vertical((0x4b4f55, 0x2b2e33)))
                    .shadow_lg()
                    .child(
                        ribs().absolute().top(px(10.)).bottom(px(10.)).left(px(10.)).right(px(10.)),
                    )
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .px(px(22.))
                            .flex()
                            .items_center()
                            .gap(px(22.))
                            .child(volume_knob)
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(px(14.))
                                    .child(self.dial(cx))
                                    .child(self.keys(cx)),
                            )
                            .child(stay_knob),
                    ),
            )
    }
}

/// Where the needle is at `time` (0 to 1) on its way along `path`: a few fast swings across
/// the dial, then homing in on the last point with an overshoot and a wobble.
fn needle_position(path: &[f32], time: f32) -> f32 {
    let swings = path.len().saturating_sub(2);
    let position = if swings > 0 && time < SWING_SHARE {
        let along = time / SWING_SHARE * swings as f32;
        let leg = (along as usize).min(swings - 1);
        lerp(path[leg], path[leg + 1], smoothstep(along - leg as f32))
    } else {
        let homing = if swings > 0 { (time - SWING_SHARE) / (1.0 - SWING_SHARE) } else { time };
        lerp(path[path.len().saturating_sub(2)], path[path.len() - 1], settle(homing))
    };
    position.clamp(0.0, 1.0)
}

/// Rises from 0 to exactly 1, overshooting and ringing on the way like a hand correcting itself.
fn settle(time: f32) -> f32 {
    1.0 - (-6.0 * time).exp() * (3.0 * PI * time).cos() * (1.0 - time)
}

fn smoothstep(time: f32) -> f32 {
    time * time * (3.0 - 2.0 * time)
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

fn swing_away_from(position: f32) -> f32 {
    loop {
        let candidate = DIAL_MARGIN + (1.0 - 2.0 * DIAL_MARGIN) * fastrand::f32();
        if (candidate - position).abs() >= MIN_SWING {
            return candidate;
        }
    }
}

/// The stop closest to a length of stay, for settings that fall between stops.
fn nearest_stop(seconds: u64) -> usize {
    (0..STAY_STOPS.len()).min_by_key(|&stop| STAY_STOPS[stop].abs_diff(seconds)).unwrap()
}

/// A length of stay, short enough for the readout: "15s", "5m", "1h".
fn stay_label(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

/// Where the stay knob's marker points for a stop, in degrees clockwise from straight up.
fn stop_angle(stop: usize) -> f32 {
    let turn = stop as f32 / (STAY_STOPS.len() - 1) as f32;
    KNOB_SWEEP * (2.0 * turn - 1.0)
}

fn vertical((top, bottom): (u32, u32)) -> Background {
    linear_gradient(180., linear_color_stop(rgb(top), 0.), linear_color_stop(rgb(bottom), 1.))
}

/// A chunky cream key. A lit key glows amber to show that its feature is switched on.
fn key(id: &'static str, label: &'static str, lit: bool) -> Stateful<Div> {
    let (top, bottom) = if lit { AMBER } else { KEY_CREAM };
    div()
        .id(id)
        .flex_1()
        .h(px(46.))
        .rounded_md()
        .border_1()
        .border_color(rgb(0x6b5a3a))
        .bg(vertical((top, bottom)))
        .shadow_md()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(16.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(INK))
        .cursor_pointer()
        .active(move |pressed| pressed.bg(vertical((bottom, top))))
        .child(label)
}

/// The scale under the numerals, painted directly rather than built from elements.
fn tick_marks() -> impl IntoElement + Styled {
    canvas(
        |_, _, _| {},
        |bounds, _, window, _| {
            for tick in 0..=TICKS {
                let x = bounds.origin.x + bounds.size.width * (tick as f32 / TICKS as f32);
                let height = if tick % 5 == 0 { bounds.size.height } else { bounds.size.height / 2. };
                let top = bounds.origin.y + bounds.size.height - height;
                window.paint_quad(fill(
                    Bounds { origin: point(x, top), size: size(px(1.5), height) },
                    rgb(CREAM),
                ));
            }
        },
    )
}

/// Horizontal ribs pressed into the chrome face: a dark line with a highlight under it.
fn ribs() -> impl IntoElement + Styled {
    canvas(
        |_, _, _| {},
        |bounds, _, window, _| {
            let mut y = bounds.origin.y;
            while y < bounds.origin.y + bounds.size.height {
                let line = |offset: f32, colour: u32| {
                    fill(
                        Bounds {
                            origin: point(bounds.origin.x, y + px(offset)),
                            size: size(bounds.size.width, px(1.)),
                        },
                        rgba(colour),
                    )
                };
                window.paint_quad(line(0., 0x00000055));
                window.paint_quad(line(1., 0xffffff16));
                y += px(RIB_SPACING);
            }
        },
    )
}

/// The dark chrome collar a knob sits in.
fn knob_collar(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .flex_none()
        .size(px(KNOB))
        .rounded_full()
        .bg(vertical((0x969ca3, 0x363a3f)))
        .shadow_lg()
        .flex()
        .items_center()
        .justify_center()
}

/// The dark base of a knob's face. The bakelite laid on it stops short of the edge,
/// leaving a thin rim, and the groove of a two-part knob shows through as the same dark.
fn knob_face() -> Div {
    div().relative().size(px(KNOB_FACE)).rounded_full().bg(rgb(0x6b5a3a))
}

/// The dot on a knob's face showing where it is turned to.
fn knob_marker(angle: f32) -> Div {
    orbiting_dot(KNOB_FACE, 22.0, angle, 9.0, INK)
}

/// A round mark inside a square `within` pixels across: `distance` out from the centre,
/// at `angle` degrees clockwise from straight up.
fn orbiting_dot(within: f32, distance: f32, angle: f32, size: f32, colour: u32) -> Div {
    let (sin, cos) = angle.to_radians().sin_cos();
    div()
        .absolute()
        .left(px(within / 2.0 + distance * sin - size / 2.0))
        .top(px(within / 2.0 - distance * cos - size / 2.0))
        .size(px(size))
        .rounded_full()
        .bg(rgb(colour))
}

/// One half of a two-part knob. The halves slope down to a groove between them, so it is
/// plain at a glance that each side does something different, and each presses in on its own.
///
/// GPUI can only clip to rectangles, so a half is a rectangular window onto one side of a
/// full disc.
fn knob_half(id: &'static str, right: bool) -> Stateful<Div> {
    const GROOVE: f32 = 2.0;
    let disc = KNOB_FACE - 2.0 * KNOB_RIM;
    let half = (disc - GROOVE) / 2.0;
    let (light, shade) = (rgb(BAKELITE.0), rgb(0xbfa873));

    let (window_left, disc_left, slope) = if right {
        let slope = linear_gradient(90., linear_color_stop(shade, 0.5), linear_color_stop(light, 1.));
        (KNOB_RIM + half + GROOVE, -(half + GROOVE), slope)
    } else {
        let slope = linear_gradient(90., linear_color_stop(light, 0.), linear_color_stop(shade, 0.5));
        (KNOB_RIM, 0.0, slope)
    };

    div()
        .id(id)
        .absolute()
        .top(px(KNOB_RIM))
        .left(px(window_left))
        .w(px(half))
        .h(px(disc))
        .overflow_hidden()
        .cursor_pointer()
        .active(|pressed| pressed.opacity(0.8))
        .child(div().absolute().top_0().left(px(disc_left)).size(px(disc)).rounded_full().bg(slope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_needle_starts_where_it_was_and_ends_on_the_station() {
        for path in [vec![0.2, 0.9, 0.5], vec![0.8, 0.1, 0.7, 0.3]] {
            assert!((needle_position(&path, 0.0) - path[0]).abs() < 1e-6);
            assert!((needle_position(&path, 1.0) - path[path.len() - 1]).abs() < 1e-6);
        }
    }

    #[test]
    fn the_needle_overshoots_the_station_before_settling() {
        let path = [0.2, 0.9, 0.5];
        let lowest = (0..=1000)
            .map(|step| needle_position(&path, step as f32 / 1000.0))
            .fold(f32::INFINITY, f32::min);

        // Homing in from 0.9 down to 0.5, it should dip below 0.5 on the way.
        assert!(lowest < 0.48, "lowest point was {lowest}");
    }

    #[test]
    fn the_needle_never_leaves_the_dial() {
        let path = [0.5, 0.99, 0.01, 0.98];
        for step in 0..=1000 {
            let position = needle_position(&path, step as f32 / 1000.0);
            assert!((0.0..=1.0).contains(&position), "position {position} at step {step}");
        }
    }

    #[test]
    fn stay_times_are_labelled_compactly() {
        let labels: Vec<String> = STAY_STOPS.iter().map(|&seconds| stay_label(seconds)).collect();

        assert_eq!(labels, ["15s", "30s", "1m", "2m", "5m", "10m", "15m", "30m", "1h"]);
    }

    #[test]
    fn the_stay_marker_sweeps_evenly_and_leaves_a_gap_at_the_bottom() {
        assert_eq!(stop_angle(0), -KNOB_SWEEP);
        assert_eq!(stop_angle(STAY_STOPS.len() / 2), 0.0);
        assert_eq!(stop_angle(STAY_STOPS.len() - 1), KNOB_SWEEP);
        // Less than a half turn each way, or the two ends would meet.
        assert!(stop_angle(STAY_STOPS.len() - 1) < 180.0);
    }

    #[test]
    fn an_in_between_stay_time_snaps_to_the_nearest_stop() {
        assert_eq!(STAY_STOPS[nearest_stop(30)], 30);
        assert_eq!(STAY_STOPS[nearest_stop(10)], 15);
        assert_eq!(STAY_STOPS[nearest_stop(100)], 120);
        assert_eq!(STAY_STOPS[nearest_stop(99_999)], 3600);
    }

    #[test]
    fn a_needle_with_nowhere_to_go_stays_put() {
        assert_eq!(needle_position(&[0.3], 0.5), 0.3);
    }
}
