//! Optimal recognition point and per-token display durations.

use unicode_segmentation::UnicodeSegmentation;

use crate::text::{Boundary, Style, Token};

/// A token split around its optimal recognition point (ORP), the letter the
/// eye should fixate on. All three parts are grapheme-safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrpSplit {
    pub before: String,
    pub pivot: String,
    pub after: String,
}

/// Index of the ORP grapheme, slightly left of centre as in Spritz-style readers.
pub fn orp_index(len: usize) -> usize {
    match len {
        0 | 1 => 0,
        2..=5 => 1,
        6..=9 => 2,
        10..=13 => 3,
        _ => 4,
    }
}

pub fn split_orp(text: &str) -> OrpSplit {
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    // Leading punctuation (quotes, brackets) should not take the pivot.
    let letters: Vec<usize> = graphemes
        .iter()
        .enumerate()
        .filter(|(_, g)| g.chars().any(|c| c.is_alphanumeric()))
        .map(|(i, _)| i)
        .collect();
    let idx = if letters.is_empty() {
        orp_index(graphemes.len())
    } else {
        letters[orp_index(letters.len())]
    };
    if graphemes.is_empty() {
        return OrpSplit {
            before: String::new(),
            pivot: String::new(),
            after: String::new(),
        };
    }
    OrpSplit {
        before: graphemes[..idx].concat(),
        pivot: graphemes[idx].to_string(),
        after: graphemes[idx + 1..].concat(),
    }
}

/// Knobs that influence how long each token stays on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pacing {
    pub wpm: u32,
    /// Scale pauses at punctuation and paragraph breaks (1.0 = default, 0 = none).
    pub pause_scale: f64,
    /// Slow down long words and numbers.
    pub length_scale: f64,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            wpm: 300,
            pause_scale: 1.0,
            length_scale: 1.0,
        }
    }
}

/// Milliseconds the token should remain on screen.
pub fn duration_ms(token: &Token, pacing: &Pacing) -> f64 {
    let base = 60_000.0 / pacing.wpm.max(1) as f64;
    let len = token.len() as f64;
    let mut mult = 1.0;

    if len > 6.0 {
        mult += 0.035 * (len - 6.0).min(10.0) * pacing.length_scale;
    }
    if token.text.chars().any(|c| c.is_ascii_digit()) {
        mult += 0.35 * pacing.length_scale;
    }
    if token.style == Style::Code {
        mult += 0.25 * pacing.length_scale;
    }
    let pause = match token.boundary {
        Boundary::None => 0.0,
        Boundary::Clause => 0.6,
        Boundary::Sentence => 1.4,
        Boundary::Paragraph => 2.2,
    };
    mult += pause * pacing.pause_scale;
    if token.style == Style::Heading {
        mult += 0.5 * pacing.pause_scale;
    }
    base * mult
}

/// Total reading time for a slice of tokens, in milliseconds.
pub fn total_ms(tokens: &[Token], pacing: &Pacing) -> f64 {
    tokens.iter().map(|t| duration_ms(t, pacing)).sum()
}

/// Format milliseconds as "m:ss" (or "h:mm:ss" for long reads).
pub fn format_clock(ms: f64) -> String {
    let secs = (ms / 1000.0).round().max(0.0) as u64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str, boundary: Boundary) -> Token {
        Token {
            text: text.into(),
            style: Style::Normal,
            boundary,
            glue: false,
        }
    }

    #[test]
    fn orp_positions() {
        assert_eq!(split_orp("a").pivot, "a");
        let s = split_orp("hello");
        assert_eq!(
            (s.before.as_str(), s.pivot.as_str(), s.after.as_str()),
            ("h", "e", "llo")
        );
        let s = split_orp("reading");
        assert_eq!(
            (s.before.as_str(), s.pivot.as_str(), s.after.as_str()),
            ("re", "a", "ding")
        );
        let s = split_orp("\"quoted\"");
        assert_eq!(s.before, "\"qu");
        assert_eq!(s.pivot, "o");
        let s = split_orp("naïve");
        assert_eq!(s.pivot, "a");
        assert_eq!(s.after, "ïve");
    }

    #[test]
    fn durations_scale_with_boundaries() {
        let p = Pacing::default();
        let plain = duration_ms(&tok("word", Boundary::None), &p);
        let clause = duration_ms(&tok("word,", Boundary::Clause), &p);
        let sentence = duration_ms(&tok("word.", Boundary::Sentence), &p);
        let para = duration_ms(&tok("word.", Boundary::Paragraph), &p);
        assert!((plain - 200.0).abs() < 1e-9);
        assert!(plain < clause && clause < sentence && sentence < para);
    }

    #[test]
    fn pause_scale_zero_removes_pauses() {
        let p = Pacing {
            pause_scale: 0.0,
            ..Pacing::default()
        };
        let a = duration_ms(&tok("word", Boundary::None), &p);
        let b = duration_ms(&tok("word", Boundary::Paragraph), &p);
        assert_eq!(a, b);
    }

    #[test]
    fn clock_format() {
        assert_eq!(format_clock(0.0), "00:00");
        assert_eq!(format_clock(61_000.0), "01:01");
        assert_eq!(format_clock(3_600_000.0 + 5_000.0), "1:00:05");
    }
}
