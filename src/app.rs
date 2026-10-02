//! Browser front end: wires the DOM to the reader state machine.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Document, Element, Event, HtmlElement, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement,
    KeyboardEvent, PointerEvent, Storage, UrlSearchParams, Window,
};

use crate::fetch;
use crate::input::{classify, Input};
use crate::resume::{self, PauseKind};
use crate::settings::Settings;
use crate::text::{self, Boundary, Document as Doc, Style};
use crate::timing::{self, Pacing};

const SETTINGS_KEY: &str = "sicr.settings";
const LAST_KEY: &str = "sicr.last";
const SAMPLE: &str = include_str!("../web/sample.md");
/// Pause before the first word so the eye can settle on the pivot.
const START_DELAY_MS: f64 = 400.0;
/// Replays requested this close together step back one more sentence each, up to a limit.
const ECHO_STACK_MS: f64 = 1500.0;
const ECHO_STACK_MAX: u8 = 3;
/// Words after a resume shown a little longer while the eye settles.
const WARMUP: [f64; 3] = [1.5, 1.25, 1.1];
/// A horizontal drag at least this long, and mostly horizontal, is a swipe.
const SWIPE_MIN_PX: f64 = 40.0;
const SWIPE_MAX_MS: f64 = 600.0;
/// How long to wait for the word font before the first flash.
const FONT_WAIT_MS: i32 = 1000;
/// Largest document we keep in localStorage for "Continue last".
const MAX_SAVED_BYTES: usize = 400_000;

type Shared = Rc<RefCell<App>>;

struct Els {
    home: HtmlElement,
    reader: HtmlElement,
    input: HtmlTextAreaElement,
    read_btn: HtmlElement,
    paste_btn: HtmlElement,
    continue_btn: HtmlElement,
    sample_btn: HtmlElement,
    clear_btn: HtmlElement,
    status: HtmlElement,
    wpm_home: HtmlInputElement,
    wpm_home_val: HtmlElement,
    pause_scale: HtmlInputElement,
    pause_val: HtmlElement,
    font_size: HtmlInputElement,
    font_val: HtmlElement,
    theme: HtmlSelectElement,
    word_font: HtmlSelectElement,
    guides: HtmlInputElement,
    orp: HtmlInputElement,
    study: HtmlInputElement,
    proxy: HtmlInputElement,
    back_btn: HtmlElement,
    title: HtmlElement,
    progress_text: HtmlElement,
    time_left: HtmlElement,
    stage: HtmlElement,
    word: HtmlElement,
    word_before: HtmlElement,
    word_pivot: HtmlElement,
    word_after: HtmlElement,
    context: HtmlElement,
    hint: HtmlElement,
    scrub: HtmlInputElement,
    restart_btn: HtmlElement,
    prev_sent_btn: HtmlElement,
    prev_word_btn: HtmlElement,
    play_btn: HtmlElement,
    next_word_btn: HtmlElement,
    next_sent_btn: HtmlElement,
    wpm_minus: HtmlElement,
    wpm: HtmlInputElement,
    wpm_plus: HtmlElement,
    wpm_val: HtmlElement,
}

#[derive(Serialize, Deserialize)]
struct SavedRead {
    text: String,
    idx: usize,
    title: Option<String>,
    url: Option<String>,
}

struct App {
    window: Window,
    document: Document,
    els: Els,
    settings: Settings,
    doc: Doc,
    idx: usize,
    playing: bool,
    finished: bool,
    timer: Option<i32>,
    tick: Option<Closure<dyn FnMut()>>,
    hint_timer: Option<i32>,
    hide_hint: Option<Closure<dyn FnMut()>>,
    source_text: String,
    source_url: Option<String>,
    loading: bool,
    /// Milliseconds of words shown while playing, for the finish stats.
    elapsed_ms: f64,
    /// Words shown while playing (skipping ahead does not count).
    words_read: usize,
    /// Title from a `?title=` link, applied to the next document.
    title_override: Option<String>,
    /// When and why playback last paused, for sizing the rewind on resume.
    paused_at: Option<f64>,
    pause_kind: PauseKind,
    /// The reader moved the position while paused, so resume starts exactly there.
    moved_while_paused: bool,
    /// Words left in the post-resume warm-up.
    warmup: usize,
    /// Furthest word reached before a replay; the replayed text shows until it is passed.
    replay_mark: Option<usize>,
    echo_stack: u8,
    last_echo_at: f64,
    last_echo_target: usize,
    /// Study mode is holding at a sentence end, waiting for a tap.
    gated: bool,
    /// Pointer-down position and time, for telling a swipe from a tap.
    swipe_start: Option<(f64, f64, f64)>,
    /// A swipe just ended; ignore the click the browser may send after it.
    swallow_click_until: f64,
}

pub fn run() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let els = Els::lookup(&document)?;
    let settings = load_settings(&window);

    let app: Shared = Rc::new(RefCell::new(App {
        window,
        document,
        els,
        settings,
        doc: Doc::default(),
        idx: 0,
        playing: false,
        finished: false,
        timer: None,
        tick: None,
        hint_timer: None,
        hide_hint: None,
        source_text: String::new(),
        source_url: None,
        loading: false,
        elapsed_ms: 0.0,
        words_read: 0,
        title_override: None,
        paused_at: None,
        pause_kind: PauseKind::User,
        moved_while_paused: false,
        warmup: 0,
        replay_mark: None,
        echo_stack: 0,
        last_echo_at: 0.0,
        last_echo_target: 0,
        gated: false,
        swipe_start: None,
        swallow_click_until: 0.0,
    }));

    install_timers(&app);
    {
        let a = app.borrow();
        a.apply_settings_to_ui();
        a.update_continue_button();
    }
    wire_events(&app);
    handle_query_params(&app);
    Ok(())
}

impl Els {
    fn lookup(document: &Document) -> Result<Els, JsValue> {
        fn get<T: JsCast>(document: &Document, id: &str) -> Result<T, JsValue> {
            document
                .get_element_by_id(id)
                .ok_or_else(|| JsValue::from_str(&format!("missing element #{id}")))?
                .dyn_into::<T>()
                .map_err(|_| JsValue::from_str(&format!("element #{id} has the wrong type")))
        }
        Ok(Els {
            home: get(document, "home")?,
            reader: get(document, "reader")?,
            input: get(document, "input")?,
            read_btn: get(document, "read-btn")?,
            paste_btn: get(document, "paste-btn")?,
            continue_btn: get(document, "continue-btn")?,
            sample_btn: get(document, "sample-btn")?,
            clear_btn: get(document, "clear-btn")?,
            status: get(document, "status")?,
            wpm_home: get(document, "wpm-home")?,
            wpm_home_val: get(document, "wpm-home-val")?,
            pause_scale: get(document, "pause-scale")?,
            pause_val: get(document, "pause-val")?,
            font_size: get(document, "font-size")?,
            font_val: get(document, "font-val")?,
            theme: get(document, "theme")?,
            word_font: get(document, "word-font")?,
            guides: get(document, "guides")?,
            orp: get(document, "orp")?,
            study: get(document, "study")?,
            proxy: get(document, "proxy")?,
            back_btn: get(document, "back-btn")?,
            title: get(document, "title")?,
            progress_text: get(document, "progress-text")?,
            time_left: get(document, "time-left")?,
            stage: get(document, "stage")?,
            word: get(document, "word")?,
            word_before: get(document, "word-before")?,
            word_pivot: get(document, "word-pivot")?,
            word_after: get(document, "word-after")?,
            context: get(document, "context")?,
            hint: get(document, "hint")?,
            scrub: get(document, "scrub")?,
            restart_btn: get(document, "restart-btn")?,
            prev_sent_btn: get(document, "prev-sent-btn")?,
            prev_word_btn: get(document, "prev-word-btn")?,
            play_btn: get(document, "play-btn")?,
            next_word_btn: get(document, "next-word-btn")?,
            next_sent_btn: get(document, "next-sent-btn")?,
            wpm_minus: get(document, "wpm-minus")?,
            wpm: get(document, "wpm")?,
            wpm_plus: get(document, "wpm-plus")?,
            wpm_val: get(document, "wpm-val")?,
        })
    }
}

// ---------------------------------------------------------------------------
// Persistence

fn storage(window: &Window) -> Option<Storage> {
    window.local_storage().ok().flatten()
}

fn load_settings(window: &Window) -> Settings {
    storage(window)
        .and_then(|s| s.get_item(SETTINGS_KEY).ok().flatten())
        .map(|json| Settings::from_json(&json))
        .unwrap_or_default()
}

impl App {
    fn save_settings(&self) {
        if let Some(s) = storage(&self.window) {
            let _ = s.set_item(SETTINGS_KEY, &self.settings.to_json());
        }
    }

    fn save_progress(&self) {
        if self.doc.tokens.is_empty() || self.source_text.len() > MAX_SAVED_BYTES {
            return;
        }
        let saved = SavedRead {
            text: self.source_text.clone(),
            idx: if self.finished { 0 } else { self.idx },
            title: self.doc.title.clone(),
            url: self.source_url.clone(),
        };
        if let (Some(s), Ok(json)) = (storage(&self.window), serde_json::to_string(&saved)) {
            let _ = s.set_item(LAST_KEY, &json);
        }
    }

    fn load_progress(&self) -> Option<SavedRead> {
        let json = storage(&self.window)?.get_item(LAST_KEY).ok()??;
        serde_json::from_str(&json).ok()
    }

    fn update_continue_button(&self) {
        let has_last = self.load_progress().is_some_and(|s| !s.text.trim().is_empty());
        self.els.continue_btn.set_hidden(!has_last);
    }
}

// ---------------------------------------------------------------------------
// Settings → UI

impl App {
    fn pacing(&self) -> Pacing {
        Pacing {
            wpm: self.settings.wpm,
            pause_scale: self.settings.pause_scale,
            length_scale: 1.0,
        }
    }

    fn apply_settings_to_ui(&self) {
        let s = &self.settings;
        self.els.wpm_home.set_value(&s.wpm.to_string());
        self.els.wpm.set_value(&s.wpm.to_string());
        self.els.wpm_home_val.set_text_content(Some(&s.wpm.to_string()));
        self.els.wpm_val.set_text_content(Some(&s.wpm.to_string()));
        self.els.pause_scale.set_value(&format!("{}", s.pause_scale));
        self.els
            .pause_val
            .set_text_content(Some(&format!("{:.1}", s.pause_scale)));
        self.els.font_size.set_value(&format!("{}", s.font_size));
        self.els
            .font_val
            .set_text_content(Some(&format!("{:.2}", s.font_size)));
        self.els.theme.set_value(&s.theme);
        self.els.word_font.set_value(&s.word_font);
        self.els.guides.set_checked(s.show_guides);
        self.els.orp.set_checked(s.highlight_orp);
        self.els.study.set_checked(s.study_mode);
        self.els.proxy.set_value(&s.proxy);
        for r in [
            &self.els.wpm_home,
            &self.els.wpm,
            &self.els.pause_scale,
            &self.els.font_size,
        ] {
            set_range_fill(r);
        }

        if let Some(root) = self.document.document_element() {
            if s.theme == "auto" {
                let _ = root.remove_attribute("data-theme");
            } else {
                let _ = root.set_attribute("data-theme", &s.theme);
            }
            if let Some(root) = root.dyn_ref::<HtmlElement>() {
                let _ = root
                    .style()
                    .set_property("--word-size", &format!("{}rem", s.font_size));
                let _ = root.style().set_property("--word-font", s.word_font_stack());
            }
        }
        toggle_class(&self.els.stage, "guides", s.show_guides);
        toggle_class(&self.els.word, "no-orp", !s.highlight_orp);
    }

    fn set_wpm(&mut self, wpm: u32) {
        let wpm = wpm.clamp(Settings::MIN_WPM, Settings::MAX_WPM);
        if wpm == self.settings.wpm {
            return;
        }
        self.settings.wpm = wpm;
        self.els.wpm_home.set_value(&wpm.to_string());
        self.els.wpm.set_value(&wpm.to_string());
        self.els.wpm_home_val.set_text_content(Some(&wpm.to_string()));
        self.els.wpm_val.set_text_content(Some(&wpm.to_string()));
        set_range_fill(&self.els.wpm_home);
        set_range_fill(&self.els.wpm);
        self.save_settings();
        self.render_meta();
    }

    fn read_settings_from_ui(&mut self) {
        let s = &mut self.settings;
        s.wpm = self.els.wpm_home.value().parse().unwrap_or(s.wpm);
        s.pause_scale = self.els.pause_scale.value().parse().unwrap_or(s.pause_scale);
        s.font_size = self.els.font_size.value().parse().unwrap_or(s.font_size);
        s.theme = self.els.theme.value();
        s.word_font = self.els.word_font.value();
        s.show_guides = self.els.guides.checked();
        s.highlight_orp = self.els.orp.checked();
        s.study_mode = self.els.study.checked();
        s.proxy = self.els.proxy.value();
        self.settings = self.settings.clone().clamped();
        self.apply_settings_to_ui();
        self.save_settings();
        self.render_meta();
    }
}

// ---------------------------------------------------------------------------
// Reader state machine

impl App {
    fn start_doc(&mut self, doc: Doc, source_text: String, source_url: Option<String>, start_idx: usize) {
        self.clear_timer();
        self.loading = false;
        self.doc = doc;
        if let Some(title) = self.title_override.take() {
            self.doc.title = Some(title);
        }
        self.source_text = source_text;
        self.source_url = source_url;
        self.idx = start_idx.min(self.doc.tokens.len().saturating_sub(1));
        self.playing = false;
        self.finished = false;
        self.elapsed_ms = 0.0;
        self.words_read = 0;
        self.paused_at = None;
        self.moved_while_paused = false;
        self.replay_mark = None;
        self.gated = false;
        self.set_status("", false);

        let title = self.doc.title.clone().unwrap_or_default();
        self.els.title.set_text_content(Some(&title));
        self.els
            .scrub
            .set_max(&self.doc.tokens.len().saturating_sub(1).to_string());
        self.els.home.set_hidden(true);
        self.els.reader.set_hidden(false);
        let _ = self.els.stage.focus();
        self.render();
    }

    fn go_home(&mut self) {
        self.pause();
        self.save_progress();
        self.update_continue_button();
        self.els.reader.set_hidden(true);
        self.els.home.set_hidden(false);
    }

    fn play(&mut self) {
        if self.doc.tokens.is_empty() || self.playing {
            return;
        }
        if self.gated {
            self.continue_gate();
            return;
        }
        let mut rewound = 0;
        if self.finished {
            self.finished = false;
            self.idx = 0;
            self.elapsed_ms = 0.0;
            self.words_read = 0;
        } else if let Some(at) = self.paused_at {
            let away = js_sys::Date::now() - at;
            let target = resume::resume_target(
                &self.doc,
                self.idx,
                away,
                self.pause_kind,
                self.moved_while_paused,
                &|i| self.word_ms_at(i),
            );
            rewound = self.idx.saturating_sub(target);
            self.idx = target;
        }
        self.paused_at = None;
        self.moved_while_paused = false;
        self.warmup = WARMUP.len();
        self.playing = true;
        self.words_read += 1;
        self.render();
        if rewound > 0 {
            self.show_hint(&format!("\u{21BA} {rewound} words"));
        } else {
            self.show_hint("tap to pause");
        }
        let ms = START_DELAY_MS + self.current_duration();
        self.schedule(ms);
    }

    fn pause(&mut self) {
        self.pause_with(PauseKind::User);
    }

    fn pause_with(&mut self, kind: PauseKind) {
        self.clear_timer();
        let _ = self.els.stage.class_list().remove_1("show-hint");
        if self.playing {
            self.playing = false;
            self.paused_at = Some(js_sys::Date::now());
            self.pause_kind = kind;
            self.moved_while_paused = false;
            self.replay_mark = None;
            self.render();
            self.save_progress();
        }
    }

    fn toggle(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    fn tick(&mut self) {
        self.timer = None;
        if !self.playing {
            return;
        }
        if self.idx + 1 >= self.doc.tokens.len() {
            self.playing = false;
            self.finished = true;
            self.replay_mark = None;
            self.render();
            self.save_progress();
            return;
        }
        if self.settings.study_mode && self.doc.tokens[self.idx].boundary >= Boundary::Sentence {
            self.enter_gate();
            return;
        }
        self.idx += 1;
        self.words_read += 1;
        self.warmup = self.warmup.saturating_sub(1);
        if self.replay_mark.is_some_and(|m| self.idx > m) {
            self.replay_mark = None;
        }
        self.render();
        let ms = self.current_duration();
        self.schedule(ms);
    }

    /// Study mode: stop at a sentence end with the sentence shown until the reader taps.
    fn enter_gate(&mut self) {
        self.playing = false;
        self.gated = true;
        self.replay_mark = None;
        self.render();
        self.show_hint("tap to continue");
    }

    fn continue_gate(&mut self) {
        self.gated = false;
        self.playing = true;
        if self.idx + 1 < self.doc.tokens.len() {
            self.idx += 1;
            self.words_read += 1;
        }
        self.render();
        let ms = START_DELAY_MS / 2.0 + self.current_duration();
        self.schedule(ms);
    }

    fn seek(&mut self, idx: usize) {
        if self.doc.tokens.is_empty() {
            return;
        }
        self.clear_timer();
        self.idx = idx.min(self.doc.tokens.len() - 1);
        self.finished = false;
        if self.gated {
            // Moving off a study-mode hold turns it into an ordinary pause at the new spot.
            self.gated = false;
            self.paused_at = Some(js_sys::Date::now());
        }
        if !self.playing {
            self.moved_while_paused = true;
            self.replay_mark = None;
        }
        self.render();
        if self.playing {
            let ms = START_DELAY_MS / 2.0 + self.current_duration();
            self.schedule(ms);
        }
    }

    /// Replay the sentence the reader most likely meant, without stopping. Repeated
    /// requests in quick succession step back one more sentence each.
    fn echo(&mut self) {
        if self.doc.tokens.is_empty() {
            return;
        }
        let now = js_sys::Date::now();
        let stacking = self.echo_stack > 0
            && now - self.last_echo_at < ECHO_STACK_MS
            && self.echo_stack < ECHO_STACK_MAX;
        let target = if stacking {
            self.echo_stack += 1;
            self.doc.prev_sentence(self.last_echo_target)
        } else {
            self.echo_stack = 1;
            resume::reaction_sentence(&self.doc, self.idx, &|i| self.word_ms_at(i))
        };
        self.last_echo_at = now;
        self.last_echo_target = target;
        let mark = self.replay_mark.unwrap_or(self.idx).max(self.idx);
        self.gated = false;
        if self.playing {
            self.seek(target);
        } else {
            self.finished = false;
            self.idx = target;
            self.paused_at = None;
            self.play();
        }
        self.replay_mark = Some(mark);
        self.render();
        self.show_hint(&format!("\u{21BA} {}", self.echo_stack));
    }

    /// During a replay, jump straight back to where the reader had got to.
    fn return_to_mark(&mut self) {
        if let Some(mark) = self.replay_mark.take() {
            self.seek(mark);
            self.render();
        }
    }

    fn word_ms_at(&self, i: usize) -> f64 {
        self.doc
            .tokens
            .get(i)
            .map(|t| timing::word_ms(t, &self.pacing()))
            .unwrap_or(0.0)
    }

    fn current_duration(&self) -> f64 {
        let Some(t) = self.doc.tokens.get(self.idx) else {
            return 0.0;
        };
        let pacing = self.pacing();
        // The first words after a resume stay up a little longer while the eye settles.
        let warm = match self.warmup {
            0 => 1.0,
            n => WARMUP[WARMUP.len() - n],
        };
        timing::word_ms(t, &pacing) * warm + timing::hold_ms(t, &pacing)
    }

    fn schedule(&mut self, ms: f64) {
        self.clear_timer();
        self.elapsed_ms += ms;
        if let Some(tick) = &self.tick {
            self.timer = self
                .window
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    tick.as_ref().unchecked_ref(),
                    ms.round() as i32,
                )
                .ok();
        }
    }

    fn clear_timer(&mut self) {
        if let Some(handle) = self.timer.take() {
            self.window.clear_timeout_with_handle(handle);
        }
    }

    fn show_hint(&mut self, text: &str) {
        self.els.hint.set_text_content(Some(text));
        let _ = self.els.stage.class_list().add_1("show-hint");
        if let Some(h) = self.hint_timer.take() {
            self.window.clear_timeout_with_handle(h);
        }
        if let Some(cb) = &self.hide_hint {
            self.hint_timer = self
                .window
                .set_timeout_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 1800)
                .ok();
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering

impl App {
    fn render(&self) {
        let Some(token) = self.doc.tokens.get(self.idx) else {
            self.els.word_before.set_text_content(None);
            self.els.word_pivot.set_text_content(None);
            self.els.word_after.set_text_content(None);
            return;
        };
        let split = timing::split_orp(&token.text);
        self.els.word_before.set_text_content(Some(&split.before));
        self.els.word_pivot.set_text_content(Some(&split.pivot));
        self.els.word_after.set_text_content(Some(&split.after));

        // Glyphs on each side of the focus letter, counting half the pivot for each side.
        let left = split.before.chars().count() + 1;
        let right = split.after.chars().count() + 1;
        let style = self.els.word.style();
        let _ = style.set_property("--left", &left.to_string());
        let _ = style.set_property("--right", &right.to_string());

        let cls = self.els.word.class_list();
        for c in ["style-heading", "style-strong", "style-emphasis", "style-code"] {
            let _ = cls.remove_1(c);
        }
        let style_class = match token.style {
            Style::Heading => Some("style-heading"),
            Style::Strong => Some("style-strong"),
            Style::Emphasis => Some("style-emphasis"),
            Style::Code => Some("style-code"),
            Style::Normal => None,
        };
        if let Some(c) = style_class {
            let _ = cls.add_1(c);
        }

        self.els
            .play_btn
            // U+FE0E keeps the glyphs as text so iOS does not swap in emoji.
            .set_text_content(Some(if self.playing {
                "\u{23F8}\u{FE0E}"
            } else {
                "\u{25B6}\u{FE0E}"
            }));
        self.els
            .play_btn
            .set_attribute("aria-label", if self.playing { "Pause" } else { "Play" })
            .ok();
        toggle_class(&self.els.stage, "paused", !self.playing && !self.gated);
        toggle_class(&self.els.stage, "gated", self.gated);
        toggle_class(
            &self.els.stage,
            "replay",
            self.playing && self.replay_mark.is_some(),
        );
        self.els.scrub.set_value(&self.idx.to_string());
        set_range_fill(&self.els.scrub);
        let progress = if self.doc.tokens.len() > 1 {
            self.idx as f64 / (self.doc.tokens.len() - 1) as f64
        } else {
            1.0
        };
        let _ = self
            .els
            .reader
            .style()
            .set_property("--progress", &format!("{progress:.4}"));
        self.render_meta();
        if self.playing && self.replay_mark.is_none() {
            self.els.context.set_text_content(None);
        } else {
            self.render_context();
        }
    }

    fn render_meta(&self) {
        let total = self.doc.tokens.len();
        if total == 0 {
            return;
        }
        self.els.progress_text.set_text_content(Some(&format!(
            "{:0width$} / {total}",
            self.idx + 1,
            width = total.to_string().len()
        )));
        // Time for the words after this one, so the last word reads 00:00.
        let left = timing::total_ms(&self.doc.tokens[self.idx + 1..], &self.pacing());
        self.els
            .time_left
            .set_text_content(Some(&format!("{} left", timing::format_clock(left))));
    }

    /// When paused, show the current sentence with the current word marked.
    fn render_context(&self) {
        let ctx = &self.els.context;
        ctx.set_text_content(None);
        if self.doc.tokens.is_empty() {
            return;
        }
        if self.finished {
            let words = self.words_read;
            let minutes = self.elapsed_ms / 60_000.0;
            let wpm = if minutes > 0.0 {
                (words as f64 / minutes).round() as u32
            } else {
                0
            };
            if let (Ok(done), Ok(headline), Ok(readout), Ok(again)) = (
                self.document.create_element("span"),
                self.document.create_element("strong"),
                self.document.create_element("span"),
                self.document.create_element("span"),
            ) {
                done.set_class_name("done");
                headline.set_text_content(Some("Done"));
                let _ = done.append_child(&headline);
                if words > 0 {
                    readout.set_class_name("readout");
                    readout.set_text_content(Some(&format!(
                        "{words:03} words \u{00B7} {} \u{00B7} {wpm} wpm",
                        timing::format_clock(self.elapsed_ms)
                    )));
                    let _ = done.append_child(&readout);
                }
                again.set_class_name("again");
                again.set_text_content(Some("tap to read again"));
                let _ = done.append_child(&again);
                let _ = ctx.append_child(&done);
            }
            return;
        }
        // A late tap shows the sentence that resume will replay, then the new one's first words.
        let current = self.doc.sentence_start(self.idx);
        let late = !self.playing && !self.gated && !self.moved_while_paused && {
            let meant = resume::reaction_sentence(&self.doc, self.idx, &|i| self.word_ms_at(i));
            meant < current
        };
        let (start, end) = if late {
            (self.doc.sentence_start(current - 1), self.idx + 1)
        } else {
            (current, self.doc.sentence_end(self.idx))
        };
        let mut marked: Option<Element> = None;
        for i in start..end {
            let t = &self.doc.tokens[i];
            // Pieces of a split word are shown joined, without the split hyphen.
            let piece = if t.glue {
                t.text.trim_end_matches('-')
            } else {
                t.text.as_str()
            };
            let text = if i + 1 < end && !t.glue {
                format!("{piece} ")
            } else {
                piece.to_string()
            };
            if i == self.idx {
                if let Ok(mark) = self.document.create_element("mark") {
                    mark.set_text_content(Some(&text));
                    if late {
                        mark.set_class_name("after");
                    }
                    let _ = ctx.append_child(&mark);
                    marked = Some(mark);
                }
            } else if late && i >= current {
                if let Ok(span) = self.document.create_element("span") {
                    span.set_class_name("after");
                    span.set_text_content(Some(&text));
                    let _ = ctx.append_child(&span);
                }
            } else {
                let node = self.document.create_text_node(&text);
                let _ = ctx.append_child(&node);
            }
        }
        // Long sentences scroll; keep the marked word about a third of the way down.
        if let Some(mark) = marked.and_then(|m| m.dyn_into::<HtmlElement>().ok()) {
            let top = mark.offset_top() - ctx.client_height() / 3;
            ctx.set_scroll_top(top.max(0));
        }
    }

    fn set_status(&self, msg: &str, is_error: bool) {
        self.els.status.set_text_content(Some(msg));
        toggle_class(&self.els.status, "error", is_error);
    }
}

/// Expose a range input's position as `--pct` so CSS can paint the filled track.
fn set_range_fill(input: &HtmlInputElement) {
    let value: f64 = input.value().parse().unwrap_or(0.0);
    let min: f64 = input.min().parse().unwrap_or(0.0);
    let max: f64 = input.max().parse().unwrap_or(1.0);
    let pct = if max > min {
        ((value - min) / (max - min) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let _ = input.style().set_property("--pct", &format!("{pct:.2}%"));
}

fn toggle_class(el: &Element, class: &str, on: bool) {
    let list = el.class_list();
    if on {
        let _ = list.add_1(class);
    } else {
        let _ = list.remove_1(class);
    }
}

// ---------------------------------------------------------------------------
// Loading input

fn read_input(app: &Shared, raw: String, autoplay: bool) {
    let input = classify(&raw);
    match input {
        Input::Empty => app.borrow().set_status("Paste some text or a link first.", true),
        Input::Text(text) => {
            let doc = text::parse(&text);
            if doc.tokens.is_empty() {
                app.borrow().set_status("Nothing readable in that text.", true);
                return;
            }
            start_doc_gated(app, doc, text, None, 0, autoplay);
        }
        Input::Url(url) => load_url(app, url, autoplay),
    }
}

fn load_url(app: &Shared, url: String, autoplay: bool) {
    let proxy = {
        let mut a = app.borrow_mut();
        if a.loading {
            return;
        }
        a.loading = true;
        a.set_status("Loading the page…", false);
        a.els.read_btn.set_attribute("disabled", "").ok();
        a.settings.proxy.clone()
    };
    let app = app.clone();
    spawn_local(async move {
        let result = fetch::load(&url, &proxy).await;
        let mut a = app.borrow_mut();
        a.loading = false;
        let _ = a.els.read_btn.remove_attribute("disabled");
        match result {
            Ok(fetched) => {
                let mut doc = text::parse(&fetched.text);
                if doc.title.is_none() {
                    doc.title = fetched.title.clone();
                }
                if doc.tokens.is_empty() {
                    a.set_status("That page had no readable text.", true);
                    return;
                }
                let text = match &fetched.title {
                    // Keep the title with the text so "Continue last" shows it.
                    Some(t) if !fetched.text.trim_start().starts_with('#') => {
                        format!("# {t}\n\n{}", fetched.text)
                    }
                    _ => fetched.text,
                };
                drop(a);
                start_doc_gated(&app, doc, text, Some(url), 0, autoplay);
            }
            Err(e) => a.set_status(&format!("Could not load that link: {e}"), true),
        }
    });
}

/// Show a document and, when asked, start playing once the word font is ready.
/// A short timeout keeps an offline or blocked font from stalling Read.
fn start_doc_gated(
    app: &Shared,
    doc: Doc,
    source_text: String,
    source_url: Option<String>,
    start_idx: usize,
    autoplay: bool,
) {
    let family = {
        let mut a = app.borrow_mut();
        a.start_doc(doc, source_text, source_url, start_idx);
        a.settings.word_font_family().to_string()
    };
    if !autoplay {
        return;
    }
    let app = app.clone();
    spawn_local(async move {
        let (window, document) = {
            let a = app.borrow();
            (a.window.clone(), a.document.clone())
        };
        wait_for_font(&window, &document, &family).await;
        let mut a = app.borrow_mut();
        if !a.els.reader.hidden() {
            a.play();
        }
    });
}

async fn wait_for_font(window: &Window, document: &Document, family: &str) {
    let load = document.fonts().load(&format!("500 48px \"{family}\""));
    let timeout = js_sys::Promise::new(&mut |resolve, _| {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, FONT_WAIT_MS);
    });
    let race = js_sys::Promise::race(&js_sys::Array::of2(&load, &timeout));
    let _ = JsFuture::from(race).await;
}

fn continue_last(app: &Shared) {
    let saved = app.borrow().load_progress();
    let Some(saved) = saved else {
        app.borrow().set_status("Nothing to continue.", true);
        return;
    };
    let mut doc = text::parse(&saved.text);
    if doc.title.is_none() {
        doc.title = saved.title;
    }
    if doc.tokens.is_empty() {
        app.borrow().set_status("Nothing to continue.", true);
        return;
    }
    start_doc_gated(app, doc, saved.text, saved.url, saved.idx, false);
    // Coming back to a saved read rewinds to the paragraph start on play.
    app.borrow_mut().paused_at = Some(0.0);
}

fn paste_and_read(app: &Shared) {
    let navigator = app.borrow().window.navigator();
    let app = app.clone();
    spawn_local(async move {
        let promise = navigator.clipboard().read_text();
        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(v) => {
                let text = v.as_string().unwrap_or_default();
                if text.trim().is_empty() {
                    app.borrow().set_status("The clipboard is empty.", true);
                    return;
                }
                app.borrow().els.input.set_value(&text);
                read_input(&app, text, true);
            }
            Err(_) => app
                .borrow()
                .set_status("Clipboard access was blocked. Paste into the box instead.", true),
        }
    });
}

fn handle_query_params(app: &Shared) {
    let search = app.borrow().window.location().search().unwrap_or_default();
    if search.len() < 2 {
        return;
    }
    let Ok(params) = UrlSearchParams::new_with_str(&search) else {
        return;
    };
    if let Some(wpm) = params.get("wpm").and_then(|w| w.parse::<u32>().ok()) {
        app.borrow_mut().set_wpm(wpm);
    }
    if let Some(title) = params.get("title") {
        let title = title.trim().to_string();
        if !title.is_empty() {
            app.borrow_mut().title_override = Some(title);
        }
    }
    if let Some(url) = params.get("url").or_else(|| params.get("u")) {
        app.borrow().els.input.set_value(&url);
        read_input(app, url, true);
    } else if let Some(text) = params.get("text").or_else(|| params.get("t")) {
        app.borrow().els.input.set_value(&text);
        read_input(app, text, true);
    }
}

// ---------------------------------------------------------------------------
// Events

fn install_timers(app: &Shared) {
    let tick = {
        let app = app.clone();
        Closure::wrap(Box::new(move || {
            app.borrow_mut().tick();
        }) as Box<dyn FnMut()>)
    };
    let hide_hint = {
        let app = app.clone();
        Closure::wrap(Box::new(move || {
            let mut a = app.borrow_mut();
            a.hint_timer = None;
            let _ = a.els.stage.class_list().remove_1("show-hint");
        }) as Box<dyn FnMut()>)
    };
    let mut a = app.borrow_mut();
    a.tick = Some(tick);
    a.hide_hint = Some(hide_hint);
}

fn on<F>(target: &web_sys::EventTarget, event: &str, f: F)
where
    F: FnMut(Event) + 'static,
{
    let closure = Closure::wrap(Box::new(f) as Box<dyn FnMut(Event)>);
    let _ = target.add_event_listener_with_callback(event, closure.as_ref().unchecked_ref());
    // Listeners live as long as the page, so leaking the closure is fine.
    closure.forget();
}

fn wire_events(app: &Shared) {
    let a = app.borrow();
    let els = &a.els;

    // Home
    {
        let app = app.clone();
        on(&els.read_btn, "click", move |_| {
            let raw = app.borrow().els.input.value();
            read_input(&app, raw, true);
        });
    }
    {
        let app = app.clone();
        on(&els.input, "keydown", move |e| {
            let Some(ke) = e.dyn_ref::<KeyboardEvent>() else {
                return;
            };
            if ke.key() == "Enter" && (ke.meta_key() || ke.ctrl_key()) {
                e.prevent_default();
                let raw = app.borrow().els.input.value();
                read_input(&app, raw, true);
            }
        });
    }
    {
        let app = app.clone();
        on(&els.paste_btn, "click", move |_| paste_and_read(&app));
    }
    {
        let app = app.clone();
        on(&els.continue_btn, "click", move |_| continue_last(&app));
    }
    {
        let app = app.clone();
        on(&els.sample_btn, "click", move |_| {
            app.borrow().els.input.set_value(SAMPLE);
            read_input(&app, SAMPLE.to_string(), true);
        });
    }
    {
        let app = app.clone();
        on(&els.clear_btn, "click", move |_| {
            let a = app.borrow();
            a.els.input.set_value("");
            a.set_status("", false);
            let _ = a.els.input.focus();
        });
    }
    let setting_inputs: [&web_sys::EventTarget; 7] = [
        &els.pause_scale,
        &els.font_size,
        &els.theme,
        &els.word_font,
        &els.guides,
        &els.orp,
        &els.study,
    ];
    for el in setting_inputs {
        let app = app.clone();
        on(el, "input", move |_| app.borrow_mut().read_settings_from_ui());
    }
    {
        let app = app.clone();
        on(&els.proxy, "change", move |_| {
            app.borrow_mut().read_settings_from_ui()
        });
    }
    {
        let app = app.clone();
        on(&els.wpm_home, "input", move |_| {
            let v = app.borrow().els.wpm_home.value().parse::<u32>().unwrap_or(300);
            app.borrow_mut().set_wpm(v);
        });
    }

    // Reader
    {
        let app = app.clone();
        on(&els.back_btn, "click", move |_| app.borrow_mut().go_home());
    }
    {
        let app = app.clone();
        on(&els.stage, "click", move |_| {
            let mut a = app.borrow_mut();
            if js_sys::Date::now() < a.swallow_click_until {
                return;
            }
            a.toggle();
        });
    }
    {
        let app = app.clone();
        on(&els.stage, "pointerdown", move |e| {
            let Some(pe) = e.dyn_ref::<PointerEvent>() else {
                return;
            };
            app.borrow_mut().swipe_start =
                Some((pe.client_x() as f64, pe.client_y() as f64, js_sys::Date::now()));
        });
    }
    {
        let app = app.clone();
        on(&els.stage, "pointerup", move |e| {
            let Some(pe) = e.dyn_ref::<PointerEvent>() else {
                return;
            };
            let mut a = app.borrow_mut();
            let Some((x0, y0, t0)) = a.swipe_start.take() else {
                return;
            };
            let (dx, dy) = (pe.client_x() as f64 - x0, pe.client_y() as f64 - y0);
            let now = js_sys::Date::now();
            if dx.abs() < SWIPE_MIN_PX || dx.abs() < 1.5 * dy.abs() || now - t0 > SWIPE_MAX_MS {
                return;
            }
            a.swallow_click_until = now + 400.0;
            // Finger moving right means "back", as in e-readers; left returns from a replay.
            if dx > 0.0 {
                a.echo();
            } else {
                a.return_to_mark();
            }
        });
    }
    {
        let app = app.clone();
        on(&els.stage, "pointercancel", move |_| {
            app.borrow_mut().swipe_start = None
        });
    }
    {
        let app = app.clone();
        on(&els.play_btn, "click", move |_| app.borrow_mut().toggle());
    }
    {
        let app = app.clone();
        on(&els.restart_btn, "click", move |_| {
            let mut a = app.borrow_mut();
            a.finished = false;
            a.seek(0);
        });
    }
    {
        let app = app.clone();
        on(&els.prev_word_btn, "click", move |_| {
            let mut a = app.borrow_mut();
            let i = a.idx.saturating_sub(1);
            a.seek(i);
        });
    }
    {
        let app = app.clone();
        on(&els.next_word_btn, "click", move |_| {
            let mut a = app.borrow_mut();
            let i = a.idx + 1;
            a.seek(i);
        });
    }
    {
        let app = app.clone();
        on(&els.prev_sent_btn, "click", move |_| {
            let mut a = app.borrow_mut();
            if a.playing {
                a.echo();
            } else {
                let i = a.doc.prev_sentence(a.idx);
                a.seek(i);
            }
        });
    }
    {
        let app = app.clone();
        on(&els.next_sent_btn, "click", move |_| {
            let mut a = app.borrow_mut();
            let i = a.doc.next_sentence(a.idx);
            a.seek(i);
        });
    }
    {
        let app = app.clone();
        on(&els.scrub, "input", move |_| {
            let mut a = app.borrow_mut();
            let i = a.els.scrub.value().parse::<usize>().unwrap_or(0);
            a.pause();
            a.seek(i);
        });
    }
    {
        let app = app.clone();
        on(&els.wpm, "input", move |_| {
            let v = app.borrow().els.wpm.value().parse::<u32>().unwrap_or(300);
            app.borrow_mut().set_wpm(v);
        });
    }
    {
        let app = app.clone();
        on(&els.wpm_minus, "click", move |_| {
            let mut a = app.borrow_mut();
            let v = a.settings.wpm.saturating_sub(25);
            a.set_wpm(v);
        });
    }
    {
        let app = app.clone();
        on(&els.wpm_plus, "click", move |_| {
            let mut a = app.borrow_mut();
            let v = a.settings.wpm + 25;
            a.set_wpm(v);
        });
    }

    // Keyboard
    {
        let app = app.clone();
        on(&a.document, "keydown", move |e| {
            let Some(ke) = e.dyn_ref::<KeyboardEvent>() else {
                return;
            };
            handle_key(&app, ke);
        });
    }

    // Pause when the tab is hidden; save when the page goes away.
    {
        let app = app.clone();
        on(&a.document, "visibilitychange", move |_| {
            let mut a = app.borrow_mut();
            if a.document.hidden() {
                a.pause_with(PauseKind::Auto);
                a.save_progress();
            }
        });
    }
    {
        let app = app.clone();
        on(&a.window, "pagehide", move |_| app.borrow().save_progress());
    }
}

fn handle_key(app: &Shared, ke: &KeyboardEvent) {
    let reader_open = !app.borrow().els.reader.hidden();
    let in_field = ke
        .target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .map(|el| matches!(el.tag_name().as_str(), "TEXTAREA" | "INPUT" | "SELECT"))
        .unwrap_or(false);
    let key = ke.key();

    if !reader_open {
        return;
    }
    if in_field && key != "Escape" {
        return;
    }
    if ke.meta_key() || ke.ctrl_key() || ke.alt_key() {
        return;
    }

    let mut a = app.borrow_mut();
    let handled = match key.as_str() {
        " " | "k" | "Enter" => {
            a.toggle();
            true
        }
        "ArrowLeft" | "j" if ke.shift_key() && a.playing => {
            a.echo();
            true
        }
        "ArrowLeft" | "j" => {
            let i = if ke.shift_key() {
                a.doc.prev_sentence(a.idx)
            } else {
                a.idx.saturating_sub(1)
            };
            a.seek(i);
            true
        }
        "Backspace" => {
            a.echo();
            true
        }
        "[" if a.playing => {
            a.echo();
            true
        }
        "ArrowRight" | "l" => {
            let i = if ke.shift_key() {
                a.doc.next_sentence(a.idx)
            } else {
                a.idx + 1
            };
            a.seek(i);
            true
        }
        "[" => {
            let i = a.doc.prev_sentence(a.idx);
            a.seek(i);
            true
        }
        "]" => {
            let i = a.doc.next_sentence(a.idx);
            a.seek(i);
            true
        }
        "ArrowUp" | "+" | "=" => {
            let v = a.settings.wpm + 25;
            a.set_wpm(v);
            true
        }
        "ArrowDown" | "-" | "_" => {
            let v = a.settings.wpm.saturating_sub(25);
            a.set_wpm(v);
            true
        }
        "Home" => {
            a.seek(0);
            true
        }
        "End" => {
            let last = a.doc.tokens.len().saturating_sub(1);
            a.seek(last);
            true
        }
        "r" => {
            a.finished = false;
            a.seek(0);
            true
        }
        "Escape" => {
            a.go_home();
            true
        }
        _ => false,
    };
    if handled {
        ke.prevent_default();
    }
}
