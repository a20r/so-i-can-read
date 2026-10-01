//! User settings, persisted as JSON in localStorage.

use serde::{Deserialize, Serialize};

pub const DEFAULT_PROXY: &str = "https://r.jina.ai/";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub wpm: u32,
    /// 0.0 to 2.0; scales pauses at punctuation and paragraphs.
    pub pause_scale: f64,
    /// Font size of the word display in rem.
    pub font_size: f64,
    /// "auto", "dark" or "light".
    pub theme: String,
    pub show_guides: bool,
    pub highlight_orp: bool,
    /// Read-through proxy used when a page blocks cross-origin requests.
    pub proxy: String,
    /// Typeface for the flashed word: one of `WORD_FONTS`.
    pub word_font: String,
}

/// (setting key, CSS family stack, family name to wait for before the first flash)
pub const WORD_FONTS: &[(&str, &str, &str)] = &[
    (
        "atkinson",
        "\"Atkinson Hyperlegible Next\", \"Inter\", system-ui, sans-serif",
        "Atkinson Hyperlegible Next",
    ),
    ("inter", "\"Inter\", system-ui, sans-serif", "Inter"),
    (
        "inclusive",
        "\"Inclusive Sans\", \"Inter\", system-ui, sans-serif",
        "Inclusive Sans",
    ),
];

impl Default for Settings {
    fn default() -> Self {
        Self {
            wpm: 300,
            pause_scale: 1.0,
            // Atkinson has a lower x-height than Inter, so the default is a little larger.
            font_size: 3.25,
            theme: "auto".into(),
            show_guides: true,
            highlight_orp: true,
            proxy: DEFAULT_PROXY.into(),
            word_font: WORD_FONTS[0].0.into(),
        }
    }
}

impl Settings {
    pub const MIN_WPM: u32 = 60;
    pub const MAX_WPM: u32 = 1500;
    pub const WPM_STEP: u32 = 10;

    /// CSS `font-family` value for the flashed word.
    pub fn word_font_stack(&self) -> &'static str {
        self.word_font_entry().1
    }

    /// Family name to pass to `document.fonts.load` before the first flash.
    pub fn word_font_family(&self) -> &'static str {
        self.word_font_entry().2
    }

    fn word_font_entry(&self) -> &'static (&'static str, &'static str, &'static str) {
        WORD_FONTS
            .iter()
            .find(|(key, _, _)| *key == self.word_font)
            .unwrap_or(&WORD_FONTS[0])
    }

    pub fn from_json(json: &str) -> Self {
        serde_json::from_str::<Settings>(json)
            .unwrap_or_default()
            .clamped()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn clamped(mut self) -> Self {
        self.wpm = self.wpm.clamp(Self::MIN_WPM, Self::MAX_WPM);
        self.pause_scale = self.pause_scale.clamp(0.0, 2.0);
        self.font_size = self.font_size.clamp(1.5, 6.0);
        if !matches!(self.theme.as_str(), "auto" | "dark" | "light") {
            self.theme = "auto".into();
        }
        if self.proxy.trim().is_empty() {
            self.proxy = DEFAULT_PROXY.into();
        }
        if !WORD_FONTS.iter().any(|(key, _, _)| *key == self.word_font) {
            self.word_font = WORD_FONTS[0].0.into();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = Settings {
            wpm: 420,
            ..Default::default()
        };
        assert_eq!(Settings::from_json(&s.to_json()), s);
    }

    #[test]
    fn bad_json_gives_defaults() {
        assert_eq!(Settings::from_json("not json"), Settings::default());
        assert_eq!(Settings::from_json("{\"wpm\": 5}").wpm, Settings::MIN_WPM);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let s = Settings::from_json("{\"theme\":\"dark\"}");
        assert_eq!(s.theme, "dark");
        assert_eq!(s.wpm, 300);
    }

    #[test]
    fn unknown_word_font_falls_back() {
        let s = Settings::from_json("{\"word_font\":\"comic\"}");
        assert_eq!(s.word_font, "atkinson");
        assert_eq!(s.word_font_family(), "Atkinson Hyperlegible Next");
        let s = Settings::from_json("{\"word_font\":\"inclusive\"}");
        assert!(s.word_font_stack().starts_with("\"Inclusive Sans\""));
    }
}
