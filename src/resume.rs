//! Where to pick up after a pause or a replay request. Pure functions so the rules are
//! unit tested without a browser.

use crate::text::Document;

/// Why playback stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseKind {
    /// The reader tapped or pressed a key.
    User,
    /// The tab was hidden; the lapse probably started before the event.
    Auto,
}

/// A tap this soon into a new sentence is a reaction to the previous one.
pub const LATE_TAP_MS: f64 = 900.0;
pub const LATE_TAP_WORDS: usize = 3;
/// Away-time thresholds for how far a resume rewinds.
const SHORT_AWAY_MS: f64 = 5_000.0;
const MEDIUM_AWAY_MS: f64 = 60_000.0;
const LONG_AWAY_MS: f64 = 15.0 * 60_000.0;
/// Today's quick-resume rule, kept as the minimum rewind.
const SENTENCE_WINDOW: usize = 8;
const MIN_REWIND: usize = 3;

/// Start of the sentence the reader most likely meant when reacting at `idx`.
/// `word_ms(i)` is the on-screen time of token `i` without its boundary pause.
pub fn reaction_sentence(doc: &Document, idx: usize, word_ms: &dyn Fn(usize) -> f64) -> usize {
    let start = doc.sentence_start(idx);
    if start == 0 {
        return 0;
    }
    let played: f64 = (start..idx).map(word_ms).sum();
    if idx - start < LATE_TAP_WORDS && played < LATE_TAP_MS {
        doc.sentence_start(start - 1)
    } else {
        start
    }
}

/// Index to resume from after a pause of `away_ms`.
pub fn resume_target(
    doc: &Document,
    idx: usize,
    away_ms: f64,
    kind: PauseKind,
    moved_while_paused: bool,
    word_ms: &dyn Fn(usize) -> f64,
) -> usize {
    if moved_while_paused || doc.tokens.is_empty() {
        return idx;
    }
    let mut tier = if away_ms < SHORT_AWAY_MS {
        0
    } else if away_ms < MEDIUM_AWAY_MS {
        1
    } else if away_ms < LONG_AWAY_MS {
        2
    } else {
        3
    };
    if kind == PauseKind::Auto {
        tier = (tier + 1).min(3);
    }
    let meant = reaction_sentence(doc, idx, word_ms);
    let current = doc.sentence_start(idx);
    match tier {
        0 if meant < current => meant,
        0 if idx - current <= SENTENCE_WINDOW => current,
        // Deep into a long sentence: back to the clause start, at least a few words.
        0 => doc
            .clause_start(idx)
            .min(idx.saturating_sub(MIN_REWIND))
            .max(current),
        1 => meant,
        2 if meant == 0 => 0,
        2 => doc.sentence_start(meant - 1),
        _ => doc.paragraph_start(idx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::parse;

    const DOC: &str = "One two three four. Five six seven eight nine ten eleven twelve thirteen fourteen, \
                       fifteen sixteen seventeen. Eighteen nineteen.\n\nTwenty one.";

    fn flat(_: usize) -> f64 {
        200.0
    }

    #[test]
    fn late_tap_means_the_previous_sentence() {
        let doc = parse(DOC);
        // Token 4 is "Five", the first word of sentence two.
        assert_eq!(reaction_sentence(&doc, 4, &flat), 0);
        assert_eq!(reaction_sentence(&doc, 6, &flat), 0);
        assert_eq!(reaction_sentence(&doc, 7, &flat), 4);
        // Slow words push the tap out of the late window.
        assert_eq!(reaction_sentence(&doc, 6, &|_| 600.0), 4);
    }

    // Tokens: 0-3 sentence one; 4-16 sentence two, with a clause break after "fourteen," (13);
    // 17-18 sentence three ending the paragraph; 19-20 the second paragraph.

    #[test]
    fn quick_resume_matches_the_old_rule() {
        let doc = parse(DOC);
        assert_eq!(resume_target(&doc, 9, 1_000.0, PauseKind::User, false, &flat), 4);
        assert_eq!(resume_target(&doc, 13, 1_000.0, PauseKind::User, false, &flat), 4);
        // Twelve words in: back to the clause start ("fifteen") or three words, whichever is further.
        assert_eq!(
            resume_target(&doc, 16, 1_000.0, PauseKind::User, false, &flat),
            13
        );
    }

    #[test]
    fn longer_breaks_rewind_further() {
        let doc = parse(DOC);
        assert_eq!(
            resume_target(&doc, 16, 20_000.0, PauseKind::User, false, &flat),
            4
        );
        assert_eq!(
            resume_target(&doc, 16, 120_000.0, PauseKind::User, false, &flat),
            0
        );
        assert_eq!(
            resume_target(&doc, 18, 3_600_000.0, PauseKind::User, false, &flat),
            0
        );
        assert_eq!(
            resume_target(&doc, 20, 3_600_000.0, PauseKind::User, false, &flat),
            19
        );
    }

    #[test]
    fn auto_pause_steps_one_tier_back() {
        let doc = parse(DOC);
        assert_eq!(resume_target(&doc, 16, 1_000.0, PauseKind::Auto, false, &flat), 4);
        assert_eq!(
            resume_target(&doc, 16, 20_000.0, PauseKind::Auto, false, &flat),
            0
        );
    }

    #[test]
    fn deliberate_moves_are_respected() {
        let doc = parse(DOC);
        assert_eq!(
            resume_target(&doc, 16, 120_000.0, PauseKind::User, true, &flat),
            16
        );
    }

    #[test]
    fn late_tap_replays_the_previous_sentence() {
        let doc = parse(DOC);
        assert_eq!(resume_target(&doc, 5, 1_000.0, PauseKind::User, false, &flat), 0);
    }
}
