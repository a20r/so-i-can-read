//! Turns arbitrary text (markdown, plain text, Slack pastes) into a flat list of
//! display tokens with style and pause information.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation;

use crate::slack;

/// Visual style of a token, carried over from the markdown structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Normal,
    Heading,
    Strong,
    Emphasis,
    Code,
}

/// How much of a pause should follow a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Boundary {
    None,
    /// Comma, semicolon, colon, dash, closing bracket, soft line break.
    Clause,
    /// End of a sentence.
    Sentence,
    /// End of a list item, table row or similar short block.
    Line,
    /// End of a paragraph, heading, list or block.
    Paragraph,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub style: Style,
    pub boundary: Boundary,
    /// True when this is a piece of a long word that continues in the next token.
    pub glue: bool,
}

impl Token {
    /// Number of grapheme clusters, which is what the eye sees as characters.
    pub fn len(&self) -> usize {
        self.text.graphemes(true).count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// A heading and the index of its first token; the shape of the document for scrubbing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub level: u8,
    pub start: usize,
}

/// Kinds of heavy block worth seeing on a map of the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    List,
    Table,
    Code,
    Quote,
}

/// A top-level block spanning tokens `start..end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub title: Option<String>,
    pub tokens: Vec<Token>,
    /// Headings in document order, with ascending `start`.
    pub sections: Vec<Section>,
    /// Top-level lists, tables, code blocks and quotes in document order.
    pub blocks: Vec<Block>,
}

impl Document {
    /// Index into `sections` of the section containing `idx`, if any heading precedes it.
    pub fn section_at(&self, idx: usize) -> Option<usize> {
        self.sections.partition_point(|s| s.start <= idx).checked_sub(1)
    }

    /// The section whose start lies within `radius` tokens of `idx`; ties go to the earlier one.
    pub fn nearest_section(&self, idx: usize, radius: usize) -> Option<usize> {
        let after = self.sections.partition_point(|s| s.start <= idx);
        let mut best: Option<(usize, usize)> = None;
        for k in after.checked_sub(1).into_iter().chain(Some(after)) {
            let Some(s) = self.sections.get(k) else {
                continue;
            };
            let d = s.start.abs_diff(idx);
            if d <= radius && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((k, d));
            }
        }
        best.map(|(k, _)| k)
    }

    /// Index of the first token of the sentence containing `idx`.
    pub fn sentence_start(&self, idx: usize) -> usize {
        let idx = idx.min(self.tokens.len().saturating_sub(1));
        let mut i = idx;
        while i > 0 && self.tokens[i - 1].boundary < Boundary::Sentence {
            i -= 1;
        }
        i
    }

    /// Index one past the last token of the sentence containing `idx`.
    pub fn sentence_end(&self, idx: usize) -> usize {
        let mut i = idx;
        while i < self.tokens.len() && self.tokens[i].boundary < Boundary::Sentence {
            i += 1;
        }
        (i + 1).min(self.tokens.len())
    }

    /// Index of the first token of the sentence after the one containing `idx`.
    pub fn next_sentence(&self, idx: usize) -> usize {
        self.sentence_end(idx).min(self.tokens.len().saturating_sub(1))
    }

    /// Index of the first token of the sentence before the one containing `idx`.
    pub fn prev_sentence(&self, idx: usize) -> usize {
        let start = self.sentence_start(idx);
        if start == 0 {
            0
        } else {
            self.sentence_start(start - 1)
        }
    }

    /// Index of the first token of the clause containing `idx`, never before its sentence.
    pub fn clause_start(&self, idx: usize) -> usize {
        self.block_start(idx, Boundary::Clause)
    }

    /// Index of the first token of the paragraph containing `idx`.
    pub fn paragraph_start(&self, idx: usize) -> usize {
        self.block_start(idx, Boundary::Paragraph)
    }

    fn block_start(&self, idx: usize, level: Boundary) -> usize {
        let idx = idx.min(self.tokens.len().saturating_sub(1));
        let mut i = idx;
        while i > 0 && self.tokens[i - 1].boundary < level {
            i -= 1;
        }
        i
    }

    pub fn word_count(&self) -> usize {
        self.tokens.len()
    }
}

/// Tokens longer than this are split so they fit on a phone screen.
const MAX_TOKEN_LEN: usize = 14;
/// Target length for the pieces of a split token.
const SPLIT_LEN: usize = 11;

/// A run of text with a single style; paragraph breaks are separate segments.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Text {
        text: String,
        style: Style,
    },
    Break(Boundary),
    /// A heading starts here; the title is filled in once its text has been seen.
    Section {
        level: u8,
        title: String,
    },
    BlockStart(BlockKind),
    BlockEnd,
}

/// Parse `input` (markdown or plain text) into a document of display tokens.
pub fn parse(input: &str) -> Document {
    let cleaned = slack::clean(input);
    let (title, segments) = markdown_segments(&cleaned);
    let (tokens, sections, blocks) = segments_to_tokens(&segments);
    Document {
        title,
        tokens,
        sections,
        blocks,
    }
}

fn markdown_segments(input: &str) -> (Option<String>, Vec<Segment>) {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);

    let mut segments = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::Normal];
    let mut title: Option<String> = None;
    let mut heading_buf: Option<String> = None;
    let mut section_idx: Option<usize> = None;
    let mut skip_depth = 0usize;
    // Only the outermost list, table, code block or quote becomes a block.
    let mut block_depth = 0usize;

    let push_text = |segments: &mut Vec<Segment>, text: &str, style: Style| {
        if text.trim().is_empty() {
            if !text.is_empty() {
                segments.push(Segment::Text {
                    text: " ".into(),
                    style,
                });
            }
            return;
        }
        segments.push(Segment::Text {
            text: text.to_string(),
            style,
        });
    };

    for event in Parser::new_ext(input, options) {
        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                _ => {}
            }
            continue;
        }
        let style = *style_stack.last().unwrap_or(&Style::Normal);
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    style_stack.push(Style::Heading);
                    heading_buf = Some(String::new());
                    section_idx = Some(segments.len());
                    segments.push(Segment::Section {
                        level: level as u8,
                        title: String::new(),
                    });
                }
                Tag::Strong => style_stack.push(Style::Strong),
                Tag::Emphasis => style_stack.push(Style::Emphasis),
                Tag::CodeBlock(_) => {
                    style_stack.push(Style::Code);
                    open_block(&mut segments, &mut block_depth, BlockKind::Code);
                }
                Tag::List(_) => {
                    // A nested list starts a new line inside its parent item.
                    segments.push(Segment::Break(Boundary::Line));
                    open_block(&mut segments, &mut block_depth, BlockKind::List);
                }
                Tag::Table(_) => open_block(&mut segments, &mut block_depth, BlockKind::Table),
                Tag::BlockQuote(_) => open_block(&mut segments, &mut block_depth, BlockKind::Quote),
                Tag::Image { .. } => {
                    // Alt text is the only readable part of an image.
                    style_stack.push(Style::Emphasis);
                }
                Tag::HtmlBlock | Tag::MetadataBlock(_) => skip_depth = 1,
                Tag::TableCell => {}
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    style_stack.pop();
                    if let Some(h) = heading_buf.take() {
                        let h = h.trim().to_string();
                        if let Some(Segment::Section { title: t, .. }) =
                            section_idx.take().and_then(|i| segments.get_mut(i))
                        {
                            t.clone_from(&h);
                        }
                        if title.is_none() && !h.is_empty() {
                            title = Some(h);
                        }
                    }
                    segments.push(Segment::Break(Boundary::Paragraph));
                }
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Image => {
                    style_stack.pop();
                }
                TagEnd::CodeBlock => {
                    style_stack.pop();
                    segments.push(Segment::Break(Boundary::Paragraph));
                    close_block(&mut segments, &mut block_depth);
                }
                TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Table => {
                    segments.push(Segment::Break(Boundary::Paragraph));
                    close_block(&mut segments, &mut block_depth);
                }
                TagEnd::Paragraph | TagEnd::FootnoteDefinition => {
                    segments.push(Segment::Break(Boundary::Paragraph));
                }
                TagEnd::Item
                | TagEnd::TableRow
                | TagEnd::TableHead
                | TagEnd::DefinitionListDefinition
                | TagEnd::DefinitionListTitle => {
                    segments.push(Segment::Break(Boundary::Line));
                }
                TagEnd::TableCell => segments.push(Segment::Break(Boundary::Clause)),
                _ => {}
            },
            Event::Text(t) => {
                if let Some(h) = heading_buf.as_mut() {
                    h.push_str(&t);
                }
                if style == Style::Code {
                    // Code blocks: each line is its own line of thought.
                    for (i, line) in t.lines().enumerate() {
                        if i > 0 {
                            segments.push(Segment::Break(Boundary::Clause));
                        }
                        push_text(&mut segments, line, style);
                    }
                    if t.ends_with('\n') {
                        segments.push(Segment::Break(Boundary::Clause));
                    }
                } else {
                    push_text(&mut segments, &t, style);
                }
            }
            Event::Code(c) => {
                if let Some(h) = heading_buf.as_mut() {
                    h.push_str(&c);
                }
                push_text(&mut segments, &c, Style::Code);
            }
            Event::SoftBreak => segments.push(Segment::Break(Boundary::Clause)),
            Event::HardBreak => segments.push(Segment::Break(Boundary::Clause)),
            Event::Rule => segments.push(Segment::Break(Boundary::Paragraph)),
            Event::TaskListMarker(_) | Event::FootnoteReference(_) => {}
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::InlineMath(m) | Event::DisplayMath(m) => push_text(&mut segments, &m, Style::Code),
        }
    }
    (title, segments)
}

fn open_block(segments: &mut Vec<Segment>, depth: &mut usize, kind: BlockKind) {
    *depth += 1;
    if *depth == 1 {
        segments.push(Segment::BlockStart(kind));
    }
}

fn close_block(segments: &mut Vec<Segment>, depth: &mut usize) {
    *depth = depth.saturating_sub(1);
    if *depth == 0 {
        segments.push(Segment::BlockEnd);
    }
}

fn segments_to_tokens(segments: &[Segment]) -> (Vec<Token>, Vec<Section>, Vec<Block>) {
    let mut tokens: Vec<Token> = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut open: Option<(BlockKind, usize)> = None;
    // Text runs with different styles can touch without whitespace ("**bold**text"),
    // so we join adjacent runs and split on whitespace afterwards.
    let mut pending = String::new();
    let mut pending_style = Style::Normal;

    let flush = |tokens: &mut Vec<Token>, pending: &mut String, style: Style| {
        for word in pending.split_whitespace() {
            push_word(tokens, word, style);
        }
        pending.clear();
    };

    for seg in segments {
        match seg {
            Segment::Text { text, style } => {
                if !pending.is_empty() && pending_style != *style {
                    if pending.ends_with(char::is_whitespace) || text.starts_with(char::is_whitespace) {
                        flush(&mut tokens, &mut pending, pending_style);
                        pending_style = *style;
                    }
                    // Styled runs glued to plain text keep the more visible style.
                    else if pending_style == Style::Normal {
                        pending_style = *style;
                    }
                } else if pending.is_empty() {
                    pending_style = *style;
                }
                pending.push_str(text);
            }
            Segment::Break(b) => {
                flush(&mut tokens, &mut pending, pending_style);
                if let Some(last) = tokens.last_mut() {
                    if last.boundary < *b {
                        last.boundary = *b;
                    }
                }
            }
            Segment::Section { level, title } => {
                flush(&mut tokens, &mut pending, pending_style);
                if !title.is_empty() {
                    sections.push(Section {
                        title: title.clone(),
                        level: *level,
                        start: tokens.len(),
                    });
                }
            }
            Segment::BlockStart(kind) => {
                flush(&mut tokens, &mut pending, pending_style);
                open = Some((*kind, tokens.len()));
            }
            Segment::BlockEnd => {
                flush(&mut tokens, &mut pending, pending_style);
                if let Some((kind, start)) = open.take() {
                    let end = tokens.len();
                    if end > start {
                        blocks.push(Block { kind, start, end });
                    }
                }
            }
        }
    }
    flush(&mut tokens, &mut pending, pending_style);
    if let Some(last) = tokens.last_mut() {
        last.boundary = Boundary::Paragraph;
    }
    (tokens, sections, blocks)
}

fn push_word(tokens: &mut Vec<Token>, word: &str, style: Style) {
    if slack::is_emoji_code(word) {
        return;
    }
    let boundary = trailing_boundary(word);
    let count = word.graphemes(true).count();
    if count <= MAX_TOKEN_LEN {
        tokens.push(Token {
            text: word.to_string(),
            style,
            boundary,
            glue: false,
        });
        return;
    }
    let pieces = split_long_word(word);
    let n = pieces.len();
    for (i, piece) in pieces.into_iter().enumerate() {
        let last = i + 1 == n;
        tokens.push(Token {
            text: piece,
            style,
            boundary: if last { boundary } else { Boundary::None },
            glue: !last,
        });
    }
}

/// Split a long word into readable pieces, preferring natural break points.
fn split_long_word(word: &str) -> Vec<String> {
    let graphemes: Vec<&str> = word.graphemes(true).collect();
    let mut pieces = Vec::new();
    let mut start = 0;
    while graphemes.len() - start > MAX_TOKEN_LEN {
        let window_end = (start + SPLIT_LEN).min(graphemes.len() - 1);
        // Look for a separator in the back half of the window.
        let min_break = start + SPLIT_LEN / 2;
        let mut cut = None;
        for i in (min_break..window_end).rev() {
            let g = graphemes[i];
            if matches!(g, "-" | "/" | "_" | "." | "," | ":" | "=" | "?" | "&") {
                cut = Some(i + 1);
                break;
            }
        }
        let cut = cut.unwrap_or(window_end);
        let mut piece: String = graphemes[start..cut].concat();
        let last = graphemes[cut - 1];
        let natural = matches!(last, "-" | "/" | "_" | "." | "," | ":" | "=" | "?" | "&");
        if !natural {
            piece.push('-');
        }
        pieces.push(piece);
        start = cut;
    }
    pieces.push(graphemes[start..].concat());
    pieces
}

fn trailing_boundary(word: &str) -> Boundary {
    // Skip closing quotes and brackets to find the real terminator.
    let mut chars = word.chars().rev().peekable();
    while let Some(&c) = chars.peek() {
        if matches!(
            c,
            '"' | '\'' | ')' | ']' | '}' | '\u{201D}' | '\u{2019}' | '*' | '_' | '»'
        ) {
            chars.next();
        } else {
            break;
        }
    }
    match chars.next() {
        Some('.') | Some('!') | Some('?') | Some('\u{2026}') => {
            // "e.g." and "1.5" should not end a sentence.
            if is_abbreviation(word) {
                Boundary::Clause
            } else {
                Boundary::Sentence
            }
        }
        Some(',') | Some(';') | Some(':') | Some('\u{2014}') | Some('\u{2013}') => Boundary::Clause,
        _ => {
            if word.ends_with("--") || word.ends_with(")") || word.ends_with("]") {
                Boundary::Clause
            } else {
                Boundary::None
            }
        }
    }
}

fn is_abbreviation(word: &str) -> bool {
    let w = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '.');
    if !w.ends_with('.') {
        return false;
    }
    let body = &w[..w.len() - 1];
    if body.is_empty() {
        return false;
    }
    // "e.g." / "i.e." / "U.S."
    if body.contains('.') && body.chars().all(|c| c.is_alphanumeric() || c == '.') {
        return true;
    }
    // Single capital initials ("J.") and common abbreviations.
    let lower = body.to_lowercase();
    if body.chars().count() == 1 && body.chars().all(|c| c.is_uppercase()) {
        return true;
    }
    matches!(
        lower.as_str(),
        "mr" | "mrs"
            | "ms"
            | "dr"
            | "prof"
            | "sr"
            | "jr"
            | "st"
            | "vs"
            | "etc"
            | "inc"
            | "ltd"
            | "no"
            | "approx"
            | "dept"
            | "fig"
            | "eg"
            | "ie"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(doc: &Document) -> Vec<&str> {
        doc.tokens.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn plain_sentence_tokens() {
        let doc = parse("Hello there, world. Second one!");
        assert_eq!(words(&doc), vec!["Hello", "there,", "world.", "Second", "one!"]);
        assert_eq!(doc.tokens[1].boundary, Boundary::Clause);
        assert_eq!(doc.tokens[2].boundary, Boundary::Sentence);
        assert_eq!(doc.tokens[4].boundary, Boundary::Paragraph);
    }

    #[test]
    fn markdown_styles_and_title() {
        let doc =
            parse("# My Title\n\nSome **bold** and *soft* words with `code`.\n\n- item one\n- item two\n");
        assert_eq!(doc.title.as_deref(), Some("My Title"));
        assert_eq!(doc.tokens[0].style, Style::Heading);
        assert_eq!(doc.tokens[1].boundary, Boundary::Paragraph);
        let bold = doc.tokens.iter().find(|t| t.text == "bold").unwrap();
        assert_eq!(bold.style, Style::Strong);
        let code = doc.tokens.iter().find(|t| t.text == "code.").unwrap();
        assert_eq!(code.style, Style::Code);
        let one = doc.tokens.iter().find(|t| t.text == "one").unwrap();
        assert_eq!(one.boundary, Boundary::Line);
        let two = doc.tokens.iter().find(|t| t.text == "two").unwrap();
        assert_eq!(two.boundary, Boundary::Paragraph);
    }

    #[test]
    fn links_show_text_only() {
        let doc = parse("See [the docs](https://example.com/very/long/path) now.");
        assert_eq!(words(&doc), vec!["See", "the", "docs", "now."]);
    }

    #[test]
    fn long_words_are_split() {
        let doc = parse("https://example.com/some/really/long/path/segment");
        assert!(doc.tokens.len() > 1);
        for t in &doc.tokens {
            assert!(t.len() <= MAX_TOKEN_LEN + 1, "{} too long", t.text);
        }
        let joined: String = doc.tokens.iter().map(|t| t.text.trim_end_matches('-')).collect();
        assert_eq!(joined, "https://example.com/some/really/long/path/segment");
        assert!(doc.tokens[..doc.tokens.len() - 1].iter().all(|t| t.glue));
        assert!(!doc.tokens.last().unwrap().glue);
    }

    #[test]
    fn abbreviations_do_not_end_sentences() {
        assert_eq!(trailing_boundary("e.g."), Boundary::Clause);
        assert_eq!(trailing_boundary("Dr."), Boundary::Clause);
        assert_eq!(trailing_boundary("done."), Boundary::Sentence);
        assert_eq!(trailing_boundary("done.\""), Boundary::Sentence);
        assert_eq!(trailing_boundary("what?!"), Boundary::Sentence);
    }

    #[test]
    fn sentence_navigation() {
        let doc = parse("One two. Three four five. Six.");
        assert_eq!(doc.sentence_start(3), 2);
        assert_eq!(doc.sentence_end(3), 5);
        assert_eq!(doc.next_sentence(0), 2);
        assert_eq!(doc.next_sentence(2), 5);
        assert_eq!(doc.prev_sentence(3), 0);
        assert_eq!(doc.prev_sentence(5), 2);
        assert_eq!(doc.prev_sentence(0), 0);
    }

    #[test]
    fn clause_and_paragraph_starts() {
        let doc = parse("Alpha beta, gamma delta. Epsilon zeta.\n\nEta theta, iota.");
        assert_eq!(doc.clause_start(3), 2);
        assert_eq!(doc.clause_start(1), 0);
        assert_eq!(doc.clause_start(5), 4);
        assert_eq!(doc.paragraph_start(5), 0);
        assert_eq!(doc.paragraph_start(8), 6);
    }

    #[test]
    fn soft_breaks_pause() {
        let doc = parse("first line\nsecond line");
        assert_eq!(doc.tokens[1].boundary, Boundary::Clause);
    }

    #[test]
    fn glued_styles_merge_into_one_word() {
        let doc = parse("**Note**: careful");
        assert_eq!(words(&doc), vec!["Note:", "careful"]);
        assert_eq!(doc.tokens[0].style, Style::Strong);
    }

    #[test]
    fn code_blocks_split_per_line() {
        let doc = parse("```\nlet x = 1;\nlet y = 2;\n```\n");
        let w = words(&doc);
        assert_eq!(w, vec!["let", "x", "=", "1;", "let", "y", "=", "2;"]);
        assert_eq!(doc.tokens[3].boundary, Boundary::Clause);
        assert!(doc.tokens.iter().all(|t| t.style == Style::Code));
    }

    #[test]
    fn empty_input() {
        let doc = parse("   \n\n  ");
        assert!(doc.tokens.is_empty());
        assert!(doc.sections.is_empty());
        assert!(doc.blocks.is_empty());
    }

    /// A document with every structure the reader maps: headings, a table, a quote,
    /// both kinds of list and a code block.
    const STRUCTURED: &str = r##"# Fleet scheduling notes

A short design note on how the dispatcher assigns robots to delivery routes. It is written to be read quickly, but it has the shape of a real document: sections, lists, a table, and some code.

## Background

The current dispatcher runs every **90 seconds**. Each run looks at the open orders, the idle robots and the traffic model, then produces a plan. Plans are *not* sticky: a later run can reassign a robot that has not yet left the depot.

Two problems came up in the last review. First, the plan changes too often during the morning peak, which wastes battery on reroutes. Second, when slack is low the marginal robot hour is about twice as productive as when slack is high, and the dispatcher does not know this.

## What we measured

| Metric | Low slack | High slack |
| --- | --- | --- |
| Deliveries per robot hour | 4.1 | 2.2 |
| Reroutes per hour | 11 | 3 |
| Idle time | 6% | 31% |

> The threshold for degradation is sharp. Below about 0.8 the fleet is fine; above 1.1 it falls apart within two cycles.

## Proposed changes

1. Make plans sticky for robots that are already loading, so a reroute needs a 15% gain to go through.
2. Price slack explicitly in the objective, using the `p1/p2` ratio from the traffic model.
3. Run the dispatcher every 60 seconds during peak and every 180 seconds off peak.

Things we are not changing:

- The routing engine itself.
- The battery model, which is good enough.
- The depot layout.

## How the pricing term works

The term is a single line in the objective. It penalises assignments that push slack above the threshold:

```rust
fn slack_penalty(slack: f64, threshold: f64) -> f64 {
    if slack <= threshold {
        0.0
    } else {
        (slack - threshold).powi(2) * WEIGHT
    }
}
```

The weight was fitted on three weeks of data. See `dispatch/objective.rs` for the full function and the [original proposal](https://example.com/proposal) for the derivation.

## Open questions

Does the 15% stickiness margin hold at night, when there are fewer orders and a single reroute matters more? And should the threshold be learned per depot rather than fixed for the whole fleet? We will know more after the pilot, which runs for two weeks starting Monday.
"##;

    #[test]
    fn structured_doc_sections() {
        let doc = parse(STRUCTURED);
        let titles: Vec<&str> = doc.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Fleet scheduling notes",
                "Background",
                "What we measured",
                "Proposed changes",
                "How the pricing term works",
                "Open questions",
            ]
        );
        let levels: Vec<u8> = doc.sections.iter().map(|s| s.level).collect();
        assert_eq!(levels, vec![1, 2, 2, 2, 2, 2]);
        assert_eq!(doc.sections[0].start, 0);
        for pair in doc.sections.windows(2) {
            assert!(pair[0].start < pair[1].start);
        }
        // Each section starts on the first word of its heading.
        for s in &doc.sections {
            let t = &doc.tokens[s.start];
            assert_eq!(t.style, Style::Heading);
            assert!(s.title.starts_with(&t.text), "{} vs {}", s.title, t.text);
        }
        assert_eq!(doc.section_at(0), Some(0));
        assert_eq!(doc.section_at(doc.sections[3].start + 5), Some(3));
        assert_eq!(doc.section_at(doc.tokens.len() - 1), Some(5));
    }

    #[test]
    fn structured_doc_blocks() {
        let doc = parse(STRUCTURED);
        let kinds: Vec<BlockKind> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                BlockKind::Table,
                BlockKind::Quote,
                BlockKind::List,
                BlockKind::List,
                BlockKind::Code,
            ]
        );
        let table = &doc.blocks[0];
        assert_eq!(doc.tokens[table.start].text, "Metric");
        assert_eq!(doc.tokens[table.end - 1].text, "31%");
        assert_eq!(doc.tokens[table.end - 1].boundary, Boundary::Paragraph);
        let quote = &doc.blocks[1];
        assert_eq!(doc.tokens[quote.start].text, "The");
        assert_eq!(doc.tokens[quote.end - 1].text, "cycles.");
        let numbered = &doc.blocks[2];
        assert_eq!(doc.tokens[numbered.start].text, "Make");
        assert_eq!(doc.tokens[numbered.end - 1].text, "peak.");
        let code = &doc.blocks[4];
        assert_eq!(doc.tokens[code.start].text, "fn");
        assert_eq!(doc.tokens[code.end - 1].text, "}");
        assert!(doc.tokens[code.start..code.end]
            .iter()
            .all(|t| t.style == Style::Code));
        for pair in doc.blocks.windows(2) {
            assert!(pair[0].end <= pair[1].start);
        }
        // Blocks sit inside the sections they belong to.
        assert_eq!(doc.section_at(table.start), Some(2));
        assert_eq!(doc.section_at(code.start), Some(4));
    }

    #[test]
    fn nested_list_is_one_block() {
        let doc = parse("intro\n\n- a\n  - b\n  - c\n- d\n\nafter");
        assert_eq!(doc.blocks.len(), 1);
        let b = &doc.blocks[0];
        assert_eq!(b.kind, BlockKind::List);
        assert_eq!(&words(&doc)[b.start..b.end], &["a", "b", "c", "d"]);
        assert!(doc.sections.is_empty());
    }

    #[test]
    fn nearest_section_snaps_within_radius() {
        let doc = parse("# T\n\nalpha beta gamma.\n\n## A\n\nx y z.\n\n## B\n\nq.\n");
        let starts: Vec<usize> = doc.sections.iter().map(|s| s.start).collect();
        assert_eq!(starts, vec![0, 4, 8]);
        assert_eq!(doc.nearest_section(4, 0), Some(1));
        assert_eq!(doc.nearest_section(5, 1), Some(1));
        assert_eq!(doc.nearest_section(7, 2), Some(2));
        // Equidistant: the earlier section wins.
        assert_eq!(doc.nearest_section(6, 2), Some(1));
        assert_eq!(doc.nearest_section(6, 1), None);
        assert_eq!(doc.nearest_section(2, 1), None);
        assert_eq!(doc.nearest_section(9, 3), Some(2));
        assert_eq!(doc.section_at(3), Some(0));
        assert_eq!(doc.section_at(6), Some(1));
        let plain = parse("no headings here at all.");
        assert_eq!(plain.section_at(2), None);
        assert_eq!(plain.nearest_section(0, 10), None);
    }
}
