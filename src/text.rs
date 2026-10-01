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
    /// End of a paragraph, heading, list item or block.
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub title: Option<String>,
    pub tokens: Vec<Token>,
}

impl Document {
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
    Text { text: String, style: Style },
    Break(Boundary),
}

/// Parse `input` (markdown or plain text) into a document of display tokens.
pub fn parse(input: &str) -> Document {
    let cleaned = slack::clean(input);
    let (title, segments) = markdown_segments(&cleaned);
    let tokens = segments_to_tokens(&segments);
    Document { title, tokens }
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
    let mut skip_depth = 0usize;

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
                Tag::Heading { .. } => {
                    style_stack.push(Style::Heading);
                    heading_buf = Some(String::new());
                }
                Tag::Strong => style_stack.push(Style::Strong),
                Tag::Emphasis => style_stack.push(Style::Emphasis),
                Tag::CodeBlock(_) => style_stack.push(Style::Code),
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
                }
                TagEnd::Paragraph
                | TagEnd::Item
                | TagEnd::BlockQuote(_)
                | TagEnd::TableRow
                | TagEnd::TableHead
                | TagEnd::List(_)
                | TagEnd::FootnoteDefinition
                | TagEnd::DefinitionListDefinition
                | TagEnd::DefinitionListTitle => {
                    segments.push(Segment::Break(Boundary::Paragraph));
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

fn segments_to_tokens(segments: &[Segment]) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
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
        }
    }
    flush(&mut tokens, &mut pending, pending_style);
    if let Some(last) = tokens.last_mut() {
        last.boundary = Boundary::Paragraph;
    }
    tokens
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
        assert_eq!(one.boundary, Boundary::Paragraph);
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
    }
}
