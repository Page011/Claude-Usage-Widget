#![windows_subsystem = "windows"]
//! Corner widget: Claude 5-hour and weekly usage bars; click for pace graphs, right-click for menu.

mod usage;
mod win;

use eframe::egui::{
    self, Align2, Color32, FontId, Painter, PointerButton, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
    ViewportBuilder, ViewportCommand, ViewportId, pos2, vec2,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use usage::{Level, Problem, Shared, Window};
use windows_sys::Win32::Foundation::HWND;

const ROW: f32 = 16.0;
const PAD: f32 = 8.0;
const STATUS_H: f32 = 15.0;
const GRAPH_H: f32 = 78.0;

/// Logical window size. The status line only appears when data is loading, stale or failing.
fn size(expanded: bool, status: bool) -> Vec2 {
    let extra = if status { STATUS_H } else { 0.0 };
    if expanded { vec2(300.0, 276.0 + extra) } else { vec2(190.0, 44.0 + extra) }
}

fn px(v: f32, scale: f32) -> i32 {
    (v * scale).round() as i32
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Config {
    count: usize,
    /// Compact top-left (physical px) per widget, keyed by monitor layout.
    layouts: BTreeMap<String, Vec<(i32, i32)>>,
}

impl Config {
    fn path() -> PathBuf {
        usage::data_dir().join("config.json")
    }
    fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::path()) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            usage::log(&format!("config.json unreadable, using defaults: {e}"));
            Self::default()
        })
    }
    fn save(&self) {
        let tmp = Self::path().with_extension("tmp");
        let ok = serde_json::to_string_pretty(self)
            .map_err(|e| e.to_string())
            .and_then(|s| std::fs::write(&tmp, s).map_err(|e| e.to_string()))
            .and_then(|_| std::fs::rename(&tmp, Self::path()).map_err(|e| e.to_string()));
        if let Err(e) = ok {
            usage::log(&format!("config save failed: {e}"));
        }
    }
}

struct Widget {
    vid: ViewportId,
    title: String,
    hwnd: Option<HWND>,
    expanded: bool,
    size: Vec2,
    /// Programmatic move, re-applied until it sticks (a DPI change between monitors resizes once).
    target: Option<(win::Rect, u8)>,
    last: Option<win::Rect>,
}

impl Widget {
    fn new(id: u64) -> Self {
        Widget {
            vid: if id == 0 { ViewportId::ROOT } else { ViewportId::from_hash_of(("widget", id)) },
            title: format!("usage-widget {} #{id}", std::process::id()),
            hwnd: None,
            expanded: id == 0 && std::env::args().any(|a| a == "--expanded"),
            size: size(false, false),
            target: None,
            last: None,
        }
    }
}

fn builder(title: &str) -> ViewportBuilder {
    ViewportBuilder::default()
        .with_title(title)
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_inner_size(size(false, false))
}

enum Act {
    Toggle,
    Menu,
    Close,
}

struct App {
    shared: Arc<Mutex<Shared>>,
    refresh: mpsc::Sender<()>,
    widgets: Vec<Widget>,
    next: u64,
    cfg: Config,
    monitors: Vec<win::Monitor>,
    layout: String,
    checked: Instant,
    dirty: Option<Instant>,
    tray: Option<win::Tray>,
    quit: mpsc::Receiver<()>,
    quitting: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let (shared, refresh) = usage::spawn(cc.egui_ctx.clone());
        let cfg = Config::load();
        let monitors = win::monitors(); // after winit made the process per-monitor DPI aware
        let layout = win::signature(&monitors);
        usage::log(&format!("start: {} monitor(s) {layout}", monitors.len()));
        let count = cfg.count.max(1);
        let cfg = Config { count, ..cfg };
        let (quit_tx, quit) = mpsc::channel();
        let repaint = cc.egui_ctx.clone();
        let tray = win::Tray::new(move || {
            let _ = quit_tx.send(());
            repaint.request_repaint();
        });
        App {
            shared,
            refresh,
            widgets: (0..count as u64).map(Widget::new).collect(),
            next: count as u64,
            cfg,
            monitors,
            layout,
            checked: Instant::now(),
            dirty: None,
            tray,
            quit,
            quitting: false,
        }
    }

    fn needs_login(&self) -> bool {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        matches!(s.problem, Some(Problem::NoLogin | Problem::Expired | Problem::Auth(_)))
    }

    /// Saved spot for this layout if it is still on a monitor, else the default corner.
    fn home(&self, i: usize, sz: Vec2) -> win::Rect {
        if let Some(&(x, y)) = self.cfg.layouts.get(&self.layout).and_then(|v| v.get(i))
            && let Some(m) = self.monitors.iter().find(|m| m.work.contains(x + 1, y + 1))
        {
            return win::Rect::new(x, y, px(sz.x, m.scale), px(sz.y, m.scale));
        }
        self.default_rect(i, sz)
    }

    /// Outer corner of the first monitor (primary, then left to right) with no other widget on it;
    /// all taken = stack under the primary's widget.
    fn default_rect(&self, i: usize, sz: Vec2) -> win::Rect {
        let ms = &self.monitors;
        let taken: Vec<usize> = (self.widgets.iter().enumerate())
            .filter(|(j, _)| *j != i)
            .filter_map(|(_, w)| w.last.map(|r| win::monitor_of(ms, r)))
            .collect();
        let order = win::order(ms);
        let (m, k) = match order.iter().find(|m| !taken.contains(m)) {
            Some(&m) => (m, 0),
            None => (order[0], taken.iter().filter(|&&t| t == order[0]).count() as i32),
        };
        let sc = ms[m].scale;
        let r = win::corner(ms, m, px(sz.x, sc), px(sz.y, sc));
        win::Rect::new(r.l, r.t + k * (r.h() + px(8.0, sc)), r.w(), r.h())
    }

    fn record(&mut self, i: usize, r: win::Rect) {
        let v = self.cfg.layouts.entry(self.layout.clone()).or_default();
        if v.len() <= i {
            v.resize(i + 1, (r.l, r.t));
        }
        v[i] = (r.l, r.t);
        self.dirty = Some(Instant::now());
    }

    /// Finds the window, applies pending moves/resizes, and records moves made by dragging.
    fn place(&mut self, ctx: &egui::Context, i: usize, status: bool) {
        if self.monitors.is_empty() {
            return;
        }
        let want = size(self.widgets[i].expanded, status);
        let h = match self.widgets[i].hwnd {
            Some(h) => h,
            None => {
                // child windows appear a frame or two after first being shown
                let Some(h) = win::find(&self.widgets[i].title) else {
                    ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                };
                win::hide_from_alt_tab(h);
                // saved spots are compact; anything bigger grows from there below
                let compact = size(false, false);
                let r = self.home(i, compact);
                let w = &mut self.widgets[i];
                (w.hwnd, w.size, w.target) = (Some(h), compact, Some((r, 0)));
                h
            }
        };
        let cur = win::rect(h);
        if self.widgets[i].size != want {
            let base = self.widgets[i].target.map_or(cur, |(t, _)| t);
            let m = &self.monitors[win::monitor_of(&self.monitors, base)];
            let r = win::anchored(base, px(want.x, m.scale), px(want.y, m.scale), m.work);
            let w = &mut self.widgets[i];
            (w.size, w.target) = (want, Some((r, 0)));
        }
        let w = &mut self.widgets[i];
        let record = match w.target {
            Some((t, n)) if cur != t && n < 5 => {
                win::place(h, t);
                w.target = Some((t, n + 1));
                ctx.request_repaint();
                false
            }
            Some((t, _)) => {
                if cur != t {
                    usage::log(&format!("placement did not stick: want {t:?} got {cur:?}"));
                }
                w.target = None;
                !w.expanded
            }
            None => w.last.is_some_and(|l| l != cur) && !w.expanded,
        };
        w.last = Some(cur);
        if record {
            self.record(i, cur);
        }
    }

    fn frame_widget(&mut self, ui: &mut Ui, i: usize) -> Option<Act> {
        let (resp, status) = {
            let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            draw(ui, &s, self.widgets[i].expanded, usage::now())
        };
        self.place(ui.ctx(), i, status);
        if ui.input(|inp| inp.viewport().close_requested()) {
            return Some(Act::Close);
        }
        if resp.drag_started_by(PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if resp.clicked() {
            Some(Act::Toggle)
        } else if resp.secondary_clicked() {
            Some(Act::Menu)
        } else {
            None
        }
    }

    fn act(&mut self, ctx: &egui::Context, i: usize, a: Act) {
        match a {
            Act::Toggle => self.widgets[i].expanded ^= true, // place() resizes next frame
            Act::Close => self.remove(ctx, i),
            Act::Menu => {
                let Some(h) = self.widgets[i].hwnd else { return };
                let last = if self.widgets.len() > 1 { "Remove Widget" } else { "End Task" };
                let mut items = vec![(1, "Refresh now"), (2, "Reset position")];
                if self.needs_login() {
                    items.push((5, "Log in to Claude"));
                }
                items.extend([(0, ""), (3, "+ Widget"), (4, last)]);
                match win::menu(h, &items) {
                    1 => _ = self.refresh.send(()),
                    5 => usage::login(),
                    2 => {
                        if let Some(cur) = self.widgets[i].last {
                            let r = win::corner(&self.monitors, win::monitor_of(&self.monitors, cur), cur.w(), cur.h());
                            self.widgets[i].target = Some((r, 0));
                        }
                    }
                    3 => {
                        self.widgets.push(Widget::new(self.next));
                        self.next += 1;
                        self.cfg.count = self.widgets.len();
                        self.dirty = Some(Instant::now());
                    }
                    4 => self.remove(ctx, i),
                    _ => {}
                }
            }
        }
        ctx.request_repaint();
    }

    fn remove(&mut self, ctx: &egui::Context, i: usize) {
        if self.quitting || self.widgets.len() == 1 {
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Close);
            return;
        }
        let saved = self.cfg.layouts.entry(self.layout.clone()).or_default();
        if i < saved.len() {
            saved.remove(i);
        }
        if i == 0 {
            // The root window cannot close without ending the app: it takes widget 1's place
            // and widget 1's window closes instead. Same result on screen.
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::CancelClose);
            let gone = self.widgets.remove(1);
            let root = &mut self.widgets[0];
            (root.expanded, root.size, root.target) = (gone.expanded, gone.size, gone.last.map(|r| (r, 0)));
        } else {
            self.widgets.remove(i);
        }
        self.cfg.count = self.widgets.len();
        self.dirty = Some(Instant::now());
    }

    /// Dock/undock: switch to that layout's saved spots (or defaults).
    fn check_layout(&mut self) {
        if self.checked.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.checked = Instant::now();
        let ms = win::monitors();
        let sig = win::signature(&ms);
        if ms.is_empty() || sig == self.layout {
            return;
        }
        usage::log(&format!("layout changed: {} monitor(s) {sig}", ms.len()));
        (self.monitors, self.layout) = (ms, sig);
        for i in 0..self.widgets.len() {
            let sz = size(false, false);
            let r = self.home(i, sz);
            let w = &mut self.widgets[i];
            (w.expanded, w.size, w.target) = (false, sz, Some((r, 0)));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_secs(30)); // countdowns, layout check
        self.check_layout();
        if self.quit.try_recv().is_ok() {
            // Quit from the tray: close every widget, not just one (see remove()).
            self.quitting = true;
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Close);
        }
        let mut acts = Vec::new();
        if let Some(a) = self.frame_widget(ui, 0) {
            acts.push((0, a));
        }
        for i in 1..self.widgets.len() {
            let (vid, b) = (self.widgets[i].vid, builder(&self.widgets[i].title));
            if let Some(a) = ctx.show_viewport_immediate(vid, b, |ui, _| self.frame_widget(ui, i)) {
                acts.push((i, a));
            }
        }
        // one action per frame: a removal shifts indexes
        if let Some((i, a)) = acts.into_iter().next() {
            self.act(&ctx, i, a);
        }
        if let Some(t) = self.dirty {
            if t.elapsed() >= Duration::from_secs(1) {
                self.cfg.save();
                self.dirty = None;
            } else {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty.is_some() {
            self.cfg.save();
        }
        drop(self.tray.take()); // removes the tray icon now, not on the next hover
    }
}

// ---------- drawing ----------

fn color(l: Level, dark: bool) -> Color32 {
    match (l, dark) {
        (Level::Ok, true) => Color32::from_rgb(0x6A, 0x9C, 0xC4), // lighter blue for dark panels
        (Level::Ok, false) => Color32::from_rgb(0x36, 0x63, 0x82),
        // amber is too faint on light panels (2.3:1 contrast), so it gets a darker shade
        (Level::Warn, true) => Color32::from_rgb(0xE7, 0x9B, 0x25),
        (Level::Warn, false) => Color32::from_rgb(0x9A, 0x5B, 0x00),
        (Level::Limit, true) => Color32::from_rgb(0xEF, 0x53, 0x50),
        (Level::Limit, false) => Color32::from_rgb(0xC6, 0x28, 0x28),
    }
}

fn dur(s: i64) -> String {
    let s = s.max(0);
    let (d, h, m) = (s / 86400, s % 86400 / 3600, s % 3600 / 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m:02}")
    } else {
        format!("{m}m")
    }
}

fn local(t: i64, fmt: &str) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| d.with_timezone(&chrono::Local).format(fmt).to_string())
        .unwrap_or_default()
}

/// Paints one widget. Returns its click/drag response and whether the status line is shown.
fn draw(ui: &mut Ui, s: &Shared, expanded: bool, now: i64) -> (egui::Response, bool) {
    let rect = ui.max_rect();
    let vis = ui.visuals().clone();
    let (text, weak) = (vis.text_color(), vis.weak_text_color());
    let small = FontId::proportional(11.0);
    let p = ui.painter();
    p.rect(rect, 6.0, vis.panel_fill, vis.widgets.noninteractive.bg_stroke, StrokeKind::Inside);

    let stale = s.problem.is_some();
    let rows = [("5h", s.last.map(|(u, _)| u.five), "%H:%M"), ("Wk", s.last.map(|(u, _)| u.week), "%a %H:%M")];
    let (l, r) = (rect.left() + PAD, rect.right() - PAD);
    let mut y = rect.top() + PAD;
    for (label, w, _) in rows {
        let mid = y + ROW / 2.0 - 1.0;
        p.text(pos2(l, mid), Align2::LEFT_CENTER, label, small.clone(), text);
        let bar = Rect::from_min_max(pos2(l + 20.0, mid - 4.0), pos2(r - 38.0, mid + 4.0));
        p.rect_filled(bar, 3.0, vis.extreme_bg_color);
        if let Some(w) = w {
            let lv = w.level(now);
            let c = if stale { weak } else { color(lv, vis.dark_mode) };
            let fill = Rect::from_min_size(bar.min, vec2(bar.width() * (w.pct as f32 / 100.0).clamp(0.0, 1.0), bar.height()));
            p.rect_filled(fill, 3.0, c);
            // colour is never the only signal: "!" when projected to hit, or at 90%+
            let mark = if lv == Level::Ok { "" } else { "!" };
            p.text(pos2(r, mid), Align2::RIGHT_CENTER, format!("{:.0}%{mark}", w.pct), small.clone(), text);
        }
        y += ROW;
    }

    let status = match (&s.problem, s.last) {
        (Some(pr), Some((_, at))) => Some(format!("{} ({} old)", pr.label(), dur(now - at))),
        (Some(pr), None) => Some(pr.label().to_string()),
        (None, None) => Some("Loading...".to_string()),
        (None, Some(_)) => None,
    };
    if let Some(msg) = &status {
        let c = if s.problem.is_some() { color(Level::Warn, vis.dark_mode) } else { text };
        p.text(pos2(l, y + 6.0), Align2::LEFT_CENTER, msg, FontId::proportional(10.0), c);
        y += STATUS_H;
    }

    if expanded {
        for (label, w, fmt) in rows {
            let Some(w) = w else { continue };
            y += 8.0;
            let head = match w.resets_at {
                Some(reset) => format!("{label}  {:.0}%   resets {} (in {})", w.pct, local(reset, fmt), dur(reset - now)),
                None => format!("{label}  {:.0}%   no active window", w.pct),
            };
            p.text(pos2(l, y + 6.0), Align2::LEFT_CENTER, head, small.clone(), text);
            y += 14.0;
            let g = Rect::from_min_max(pos2(l, y), pos2(r, y + GRAPH_H));
            let c = if stale { weak } else { color(w.level(now), vis.dark_mode) };
            let pick = |smp: &usage::Sample| if label == "5h" { smp.five } else { smp.week };
            graph(p, g, &w, s.samples.iter().map(|x| (x.t, pick(x))), now, c, weak);
            y += GRAPH_H;
            let (verdict, vc) = match (w.resets_at, w.projected(now), w.hits_at(now)) {
                (None, ..) => ("idle - no usage this window".to_string(), text),
                (_, None, _) => ("too early to project".to_string(), text),
                (Some(reset), _, Some(hit)) => {
                    (format!("LIMIT ~{} ({} before reset)", local(hit, fmt), dur(reset - hit)), c)
                }
                (_, Some(pr), None) => (format!("~{pr:.0}% at reset at this pace"), text),
            };
            p.text(pos2(l, y + 7.0), Align2::LEFT_CENTER, verdict, small.clone(), vc);
            y += 14.0;
        }
    }

    let resp = ui.interact(rect, ui.scope_id().with("widget"), Sense::click_and_drag());
    let summary = match s.last {
        Some((u, _)) => format!("Claude usage: 5 hour {:.0} percent, weekly {:.0} percent", u.five.pct, u.week.pct),
        None => "Claude usage: no data".to_string(),
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &summary));
    (resp, status.is_some())
}

/// Window start to reset on x, 0-100% on y. Solid = actual, dashed grey = even pace to 100%,
/// dashed colour = projection at the average burn so far (dot where it hits 100%).
fn graph(p: &Painter, g: Rect, w: &Window, samples: impl Iterator<Item = (i64, f64)>, now: i64, c: Color32, weak: Color32) {
    p.rect_stroke(g, 2.0, Stroke::new(1.0, weak.gamma_multiply(0.5)), StrokeKind::Inside);
    let Some(start) = w.start() else { return };
    let len = w.len as f32;
    let x = |t: i64| g.left() + ((t - start) as f32 / len).clamp(0.0, 1.0) * g.width();
    let y = |v: f64| g.bottom() - (v as f32 / 100.0).clamp(0.0, 1.0) * g.height();
    p.line_segment([pos2(g.left(), y(50.0)), pos2(g.right(), y(50.0))], Stroke::new(1.0, weak.gamma_multiply(0.25)));
    p.extend(Shape::dashed_line(&[pos2(g.left(), g.bottom()), pos2(g.right(), g.top())], Stroke::new(1.0, weak), 3.0, 3.0));
    let mut line = vec![pos2(x(start), y(0.0))];
    line.extend(samples.filter(|&(t, _)| t > start && t < now).map(|(t, v)| pos2(x(t), y(v))));
    line.push(pos2(x(now), y(w.pct)));
    p.add(Shape::line(line, Stroke::new(1.5, c)));
    if let Some(pr) = w.projected(now) {
        let end = match w.hits_at(now) {
            Some(hit) => pos2(x(hit), y(100.0)),
            None => pos2(g.right(), y(pr)),
        };
        p.extend(Shape::dashed_line(&[pos2(x(now), y(w.pct)), end], Stroke::new(1.5, c), 4.0, 3.0));
        if w.hits_at(now).is_some() {
            p.circle_filled(end, 3.0, c);
        }
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions { viewport: builder(&Widget::new(0).title), ..Default::default() };
    eframe::run_native("usage-widget", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
