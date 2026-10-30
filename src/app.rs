use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Document, Element, Event, HtmlElement, HtmlOptionElement, HtmlSelectElement, HtmlVideoElement,
    KeyboardEvent, MediaDeviceInfo, MediaDeviceKind, MediaStream, MediaStreamConstraints,
    MediaStreamTrack, PermissionState, PermissionStatus, Window,
};

use crate::util::{
    by_id, call_method, describe_error, exit_fullscreen, is_fullscreen, listen, listen_owned, num,
    obj, parse_res, request_fullscreen, set_class, sleep, storage_get, storage_set,
};

const KEY_THEME: &str = "capture-viewer.theme";
const KEY_DEVICE: &str = "capture-viewer.device";
const KEY_RES: &str = "capture-viewer.resolution";

const DEFAULT_THEME: &str = "gruvbox";
const DEFAULT_RES: &str = "1920x1080";

/// (name, browser chrome colour)
const THEMES: [(&str, &str); 2] = [("gruvbox", "#1d2021"), ("charcoal", "#111111")];

#[derive(Clone, Copy)]
enum Dot {
    Live,
    Warn,
    Error,
}

impl Dot {
    fn class(self) -> &'static str {
        match self {
            Dot::Live => "status-dot on",
            Dot::Warn => "status-dot warn",
            Dot::Error => "status-dot err",
        }
    }
}

#[derive(Default)]
struct State {
    stream: Option<MediaStream>,
    running: bool,
    /// Bumped on every start so a slow, superseded getUserMedia can be discarded.
    generation: u32,
    install_prompt: Option<JsValue>,
    /// Listeners on the current video track; dropped together with the track.
    track_handlers: Vec<Closure<dyn FnMut(Event)>>,
}

pub struct App {
    win: Window,
    doc: Document,
    shell: HtmlElement,
    vid: HtmlVideoElement,
    start_overlay: HtmlElement,
    no_signal: HtmlElement,
    error_toast: HtmlElement,
    status_dot: HtmlElement,
    status_label: HtmlElement,
    status_detail: HtmlElement,
    video_sel: HtmlSelectElement,
    res_sel: HtmlSelectElement,
    fs_btn: HtmlElement,
    theme_meta: Element,
    pwa_toast: HtmlElement,
    state: RefCell<State>,
}

impl App {
    pub fn mount() {
        let win = web_sys::window().expect("no window");
        let doc = win.document().expect("no document");

        let app = Rc::new(App {
            shell: by_id(&doc, "app"),
            vid: by_id(&doc, "vid"),
            start_overlay: by_id(&doc, "startOverlay"),
            no_signal: by_id(&doc, "noSignal"),
            error_toast: by_id(&doc, "errorToast"),
            status_dot: by_id(&doc, "statusDot"),
            status_label: by_id(&doc, "statusLabel"),
            status_detail: by_id(&doc, "statusDetail"),
            video_sel: by_id(&doc, "videoInSel"),
            res_sel: by_id(&doc, "resSel"),
            fs_btn: by_id(&doc, "fsBtn"),
            theme_meta: by_id(&doc, "themeColorMeta"),
            pwa_toast: by_id(&doc, "pwaToast"),
            state: RefCell::default(),
            win,
            doc,
        });

        app.restore_prefs();
        app.wire();
        app.register_service_worker();
        spawn_local(app.clone().auto_init());
    }

    // ── Preferences ─────────────────────────────────────────────────────────

    fn restore_prefs(&self) {
        let theme = storage_get(&self.win, KEY_THEME).unwrap_or_else(|| DEFAULT_THEME.into());
        self.apply_theme(&theme);

        if let Some(res) = storage_get(&self.win, KEY_RES) {
            self.res_sel.set_value(&res);
            // set_value() with an unknown option clears the selection.
            if self.res_sel.value().is_empty() {
                self.res_sel.set_value(DEFAULT_RES);
            }
        }
    }

    fn apply_theme(&self, name: &str) {
        let (name, chrome) = THEMES
            .iter()
            .find(|(n, _)| *n == name)
            .copied()
            .unwrap_or(THEMES[0]);

        if let Some(body) = self.doc.body() {
            let _ = body.set_attribute("data-theme", name);
        }
        let _ = self.theme_meta.set_attribute("content", chrome);

        if let Ok(chips) = self.doc.query_selector_all(".theme-chip") {
            for i in 0..chips.length() {
                if let Some(chip) = chips.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
                    let selected = chip.get_attribute("data-t").as_deref() == Some(name);
                    set_class(&chip, "selected", selected);
                }
            }
        }
    }

    // ── Event wiring ────────────────────────────────────────────────────────

    fn wire(self: &Rc<Self>) {
        // Theme chips
        if let Ok(chips) = self.doc.query_selector_all(".theme-chip") {
            for i in 0..chips.length() {
                let Some(chip) = chips.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else {
                    continue;
                };
                let this = self.clone();
                let c = chip.clone();
                listen(&chip, "click", move |_: Event| {
                    if let Some(t) = c.get_attribute("data-t") {
                        this.apply_theme(&t);
                        storage_set(&this.win, KEY_THEME, &t);
                    }
                });
            }
        }

        // Start overlay (the button inside it bubbles up here)
        let this = self.clone();
        listen(&self.start_overlay, "click", move |_: Event| this.spawn_start());

        // Restart / fullscreen buttons
        let this = self.clone();
        let restart: HtmlElement = by_id(&self.doc, "restartBtn");
        listen(&restart, "click", move |_: Event| this.spawn_start());

        let this = self.clone();
        listen(&self.fs_btn, "click", move |_: Event| this.toggle_fullscreen());

        // Selection changes persist and restart a live capture
        let this = self.clone();
        listen(&self.res_sel, "change", move |_: Event| {
            storage_set(&this.win, KEY_RES, &this.res_sel.value());
            if this.is_running() {
                this.spawn_start();
            }
        });

        let this = self.clone();
        listen(&self.video_sel, "change", move |_: Event| {
            storage_set(&this.win, KEY_DEVICE, &this.video_sel.value());
            if this.is_running() {
                this.spawn_start();
            }
        });

        // Intrinsic video size changed (e.g. the console switched output mode)
        let this = self.clone();
        for ev in ["loadedmetadata", "resize"] {
            let this = this.clone();
            listen(&self.vid, ev, move |_: Event| this.refresh_detail());
        }

        // Keyboard shortcuts: F fullscreen, Space pause, Enter start
        let this = self.clone();
        listen(&self.doc, "keydown", move |e: KeyboardEvent| {
            if e.ctrl_key() || e.meta_key() || e.alt_key() {
                return;
            }
            let tag = e
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .map(|el| el.tag_name())
                .unwrap_or_default();
            if matches!(tag.as_str(), "SELECT" | "INPUT" | "TEXTAREA") {
                return;
            }
            let on_button = tag == "BUTTON";
            match e.key().as_str() {
                "f" | "F" => this.toggle_fullscreen(),
                "Enter" if !on_button && !this.is_running() => this.spawn_start(),
                " " if !on_button && this.is_running() => {
                    e.prevent_default();
                    this.toggle_pause();
                }
                _ => {}
            }
        });

        // Fullscreen state
        for ev in ["fullscreenchange", "webkitfullscreenchange"] {
            let this = self.clone();
            listen(&self.doc, ev, move |_: Event| {
                let fs = is_fullscreen(&this.doc);
                this.fs_btn
                    .set_title(if fs { "Exit fullscreen (F)" } else { "Fullscreen (F)" });
            });
        }

        // Pause while the tab is hidden
        let this = self.clone();
        listen(&self.doc, "visibilitychange", move |_: Event| {
            if this.state.borrow().stream.is_none() {
                return;
            }
            if this.doc.hidden() {
                let _ = this.vid.pause();
            } else {
                let _ = this.vid.play();
            }
        });

        // Hot-plug: refresh the device list
        if let Ok(md) = self.win.navigator().media_devices() {
            let this = self.clone();
            listen(&md, "devicechange", move |_: Event| {
                let this = this.clone();
                spawn_local(async move {
                    let count = this.populate_devices().await;
                    if !this.is_running() {
                        if count == 0 {
                            this.show_error("No video inputs found. Connect your capture card.");
                        } else {
                            this.clear_error();
                        }
                    }
                });
            });
        }

        self.wire_install_prompt();
    }

    fn wire_install_prompt(self: &Rc<Self>) {
        let this = self.clone();
        listen(&self.win, "beforeinstallprompt", move |e: Event| {
            e.prevent_default();
            this.state.borrow_mut().install_prompt = Some(e.into());
            let this = this.clone();
            spawn_local(async move {
                sleep(1500).await;
                if this.state.borrow().install_prompt.is_some() {
                    set_class(&this.pwa_toast, "show", true);
                }
            });
        });

        let this = self.clone();
        listen(&self.win, "appinstalled", move |_: Event| {
            this.state.borrow_mut().install_prompt = None;
            set_class(&this.pwa_toast, "show", false);
        });

        let this = self.clone();
        let confirm: HtmlElement = by_id(&self.doc, "pwaInstallConfirm");
        listen(&confirm, "click", move |_: Event| {
            set_class(&this.pwa_toast, "show", false);
            let prompt = this.state.borrow_mut().install_prompt.take();
            if let Some(p) = prompt {
                let _ = call_method(&p, "prompt");
            }
        });

        let this = self.clone();
        let dismiss: HtmlElement = by_id(&self.doc, "pwaInstallDismiss");
        listen(&dismiss, "click", move |_: Event| {
            set_class(&this.pwa_toast, "show", false);
        });
    }

    fn register_service_worker(&self) {
        let nav = self.win.navigator();
        let nav_ref: &JsValue = nav.as_ref();
        let supported =
            js_sys::Reflect::has(nav_ref, &JsValue::from_str("serviceWorker")).unwrap_or(false);
        if !supported {
            return;
        }
        let container = nav.service_worker();
        spawn_local(async move {
            let _ = JsFuture::from(container.register("sw.js")).await;
        });
    }

    // ── Start-up ────────────────────────────────────────────────────────────

    async fn auto_init(self: Rc<Self>) {
        let granted = self.watch_camera_permission().await;
        let count = self.populate_devices().await;

        if granted {
            sleep(300).await;
            self.spawn_start();
        } else {
            self.overlay(true);
            self.set_status(Dot::Warn, "Ready");
        }

        if count == 0 {
            self.show_error("No video inputs found. Connect your capture card and click Start.");
        }
    }

    /// Returns whether camera permission is already granted, and starts
    /// capturing automatically if the person grants it later.
    async fn watch_camera_permission(self: &Rc<Self>) -> bool {
        let Ok(perms) = self.win.navigator().permissions() else {
            return false;
        };
        let query = obj(&[("name", "camera".into())]);
        let Ok(promise) = perms.query(&query) else {
            return false;
        };
        // Firefox rejects the "camera" permission name; treat that as "unknown".
        let Ok(status) = JsFuture::from(promise).await else {
            return false;
        };
        let status: PermissionStatus = status.unchecked_into();

        let this = self.clone();
        let watched = status.clone();
        listen(&status, "change", move |_: Event| {
            if watched.state() == PermissionState::Granted && !this.is_running() {
                this.spawn_start();
            }
        });

        status.state() == PermissionState::Granted
    }

    // ── Devices ─────────────────────────────────────────────────────────────

    /// Rebuild the device list, keeping the current (or last stored) choice.
    /// Returns how many video inputs the browser reports.
    async fn populate_devices(&self) -> usize {
        let Ok(md) = self.win.navigator().media_devices() else {
            return 0;
        };
        let Ok(promise) = md.enumerate_devices() else {
            return 0;
        };
        let Ok(list) = JsFuture::from(promise).await else {
            return 0;
        };

        let current = self.video_sel.value();
        let wanted = if current.is_empty() {
            storage_get(&self.win, KEY_DEVICE).unwrap_or_default()
        } else {
            current
        };

        self.video_sel
            .set_inner_html(r#"<option value="">Auto</option>"#);

        let mut count = 0;
        for item in Array::from(&list).iter() {
            let info: MediaDeviceInfo = item.unchecked_into();
            if info.kind() != MediaDeviceKind::Videoinput {
                continue;
            }
            count += 1;

            // Before permission is granted the browser hides ids and labels.
            let id = info.device_id();
            if id.is_empty() {
                continue;
            }
            let label = match info.label() {
                l if l.is_empty() => format!("Video {count}"),
                l => l,
            };
            if let Ok(opt) = HtmlOptionElement::new_with_text_and_value(&label, &id) {
                let _ = self.video_sel.append_child(&opt);
            }
        }

        self.video_sel.set_value(&wanted);
        if self.video_sel.value() != wanted {
            self.video_sel.set_value("");
        }
        count
    }

    fn constraints(&self) -> MediaStreamConstraints {
        let mut video: Vec<(&str, JsValue)> = vec![
            (
                "frameRate",
                obj(&[("ideal", 60.into()), ("max", 60.into())]).into(),
            ),
            ("resizeMode", "none".into()),
        ];
        if let Some((w, h)) = parse_res(&self.res_sel.value()) {
            video.push(("width", obj(&[("ideal", w.into())]).into()));
            video.push(("height", obj(&[("ideal", h.into())]).into()));
        }
        let id = self.video_sel.value();
        if !id.is_empty() {
            video.push(("deviceId", obj(&[("exact", id.into())]).into()));
        }
        obj(&[("video", obj(&video).into())]).unchecked_into()
    }

    // ── Capture lifecycle ───────────────────────────────────────────────────

    fn spawn_start(self: &Rc<Self>) {
        spawn_local(self.clone().start());
    }

    async fn start(self: Rc<Self>) {
        self.clear_error();
        let generation = {
            let mut s = self.state.borrow_mut();
            s.generation = s.generation.wrapping_add(1);
            s.generation
        };
        self.stop_stream();
        self.set_status(Dot::Warn, "Starting");

        if !self.win.is_secure_context() {
            self.fail(&JsValue::from_str(
                "Camera access needs HTTPS (or localhost)",
            ));
            return;
        }
        let md = match self.win.navigator().media_devices() {
            Ok(md) => md,
            Err(e) => return self.fail(&e),
        };
        let promise = match md.get_user_media_with_constraints(&self.constraints()) {
            Ok(p) => p,
            Err(e) => return self.fail(&e),
        };

        match JsFuture::from(promise).await {
            Ok(v) => {
                let stream: MediaStream = v.unchecked_into();
                if self.state.borrow().generation != generation {
                    stop_tracks(&stream); // a newer start() won the race
                    return;
                }
                self.attach(stream);
                // Labels only become available once permission is granted.
                self.populate_devices().await;
            }
            Err(e) => {
                if self.state.borrow().generation == generation {
                    self.fail(&e);
                }
            }
        }
    }

    fn fail(&self, e: &JsValue) {
        self.set_running(false);
        self.show_error(&describe_error(e));
        self.overlay(true);
    }

    fn attach(self: &Rc<Self>, stream: MediaStream) {
        self.vid.set_src_object(Some(&stream));
        self.vid.set_muted(true);
        let _ = self.vid.play();

        let mut handlers = Vec::new();
        if let Ok(track) = stream.get_video_tracks().get(0).dyn_into::<MediaStreamTrack>() {
            handlers.push(self.on_track(&track, "mute", Self::signal_lost));
            handlers.push(self.on_track(&track, "unmute", Self::signal_back));
            handlers.push(self.on_track(&track, "ended", Self::track_ended));
        }

        {
            let mut s = self.state.borrow_mut();
            s.stream = Some(stream);
            s.track_handlers = handlers;
        }

        set_class(&self.no_signal, "visible", false);
        self.overlay(false);
        self.set_running(true);
        self.set_status(Dot::Live, "Live");
        self.refresh_detail();
        self.try_fullscreen();
    }

    fn on_track(
        self: &Rc<Self>,
        track: &MediaStreamTrack,
        event: &str,
        f: fn(&Rc<Self>),
    ) -> Closure<dyn FnMut(Event)> {
        let this = self.clone();
        listen_owned(track, event, move |_: Event| f(&this))
    }

    fn stop_stream(&self) {
        let mut s = self.state.borrow_mut();
        s.track_handlers.clear();
        if let Some(stream) = s.stream.take() {
            stop_tracks(&stream);
        }
        s.running = false;
        drop(s);
        self.vid.set_src_object(None);
        self.status_detail.set_text_content(None);
    }

    fn signal_lost(self: &Rc<Self>) {
        set_class(&self.no_signal, "visible", true);
        self.set_status(Dot::Warn, "No signal");
    }

    fn signal_back(self: &Rc<Self>) {
        set_class(&self.no_signal, "visible", false);
        self.set_status(Dot::Live, "Live");
    }

    fn track_ended(self: &Rc<Self>) {
        self.set_running(false);
        set_class(&self.no_signal, "visible", true);
        self.show_error("Capture device disconnected");
        self.overlay(true);
    }

    // ── Controls ────────────────────────────────────────────────────────────

    fn toggle_pause(&self) {
        if self.vid.paused() {
            let _ = self.vid.play();
            self.set_status(Dot::Live, "Live");
        } else {
            let _ = self.vid.pause();
            self.set_status(Dot::Warn, "Paused");
        }
    }

    fn toggle_fullscreen(&self) {
        if is_fullscreen(&self.doc) {
            exit_fullscreen(&self.doc);
        } else {
            request_fullscreen(&self.shell);
        }
    }

    fn try_fullscreen(&self) {
        if !is_fullscreen(&self.doc) {
            request_fullscreen(&self.shell);
        }
    }

    // ── View state ──────────────────────────────────────────────────────────

    fn is_running(&self) -> bool {
        self.state.borrow().running
    }

    fn set_running(&self, running: bool) {
        self.state.borrow_mut().running = running;
        if !running {
            self.status_detail.set_text_content(None);
        }
    }

    fn overlay(&self, visible: bool) {
        set_class(&self.start_overlay, "hidden", !visible);
    }

    fn set_status(&self, dot: Dot, label: &str) {
        self.status_dot.set_class_name(dot.class());
        self.status_label.set_text_content(Some(label));
    }

    fn show_error(&self, msg: &str) {
        self.error_toast.set_text_content(Some(&format!("⚠ {msg}")));
        set_class(&self.error_toast, "visible", true);
        self.set_status(Dot::Error, "Error");
    }

    fn clear_error(&self) {
        set_class(&self.error_toast, "visible", false);
        self.error_toast.set_text_content(None);
        if self.is_running() {
            self.set_status(Dot::Live, "Live");
        } else {
            self.set_status(Dot::Warn, "Ready");
        }
    }

    /// "1920×1080 · 60 fps" next to the status label.
    fn refresh_detail(&self) {
        if !self.is_running() {
            return;
        }
        let (w, h) = (self.vid.video_width(), self.vid.video_height());
        if w == 0 || h == 0 {
            return;
        }
        let fps = self
            .state
            .borrow()
            .stream
            .as_ref()
            .and_then(|s| s.get_video_tracks().get(0).dyn_into::<MediaStreamTrack>().ok())
            .and_then(|t| num(&JsValue::from(t.get_settings()), "frameRate"));

        let text = match fps {
            Some(f) => format!("{w}×{h} · {f:.0} fps"),
            None => format!("{w}×{h}"),
        };
        self.status_detail.set_text_content(Some(&text));
    }
}

fn stop_tracks(stream: &MediaStream) {
    for t in stream.get_tracks().iter() {
        t.unchecked_into::<MediaStreamTrack>().stop();
    }
}
