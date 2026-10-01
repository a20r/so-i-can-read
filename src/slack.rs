//! Heuristics that strip the chrome Slack adds when a message is copied:
//! author lines, timestamps, "(edited)" markers and reaction rows.

/// Remove Slack chrome from pasted text. Non-Slack text passes through
/// unchanged apart from trailing-whitespace trimming.
pub fn clean(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        let next = lines.get(i + 1).map(|l| l.trim()).unwrap_or("");

        if is_time_line(trimmed) || is_reaction_row(trimmed) || is_chrome_line(trimmed) {
            i += 1;
            continue;
        }
        if is_author_line(trimmed, next) {
            i += 2;
            continue;
        }
        if ends_with_time(trimmed) {
            i += 1;
            continue;
        }
        let cleaned = strip_edited(line);
        out.push(cleaned.trim_end().to_string());
        i += 1;
    }
    out.join("\n")
}

/// `:smile:`, `:+1:`, `:thumbsup::skin-tone-2:` style emoji codes.
pub fn is_emoji_code(word: &str) -> bool {
    let w = word;
    if w.len() < 3 || !w.starts_with(':') || !w.ends_with(':') {
        return false;
    }
    let inner = &w[1..w.len() - 1];
    !inner.is_empty()
        && inner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | ':'))
        && inner.chars().any(|c| c.is_ascii_alphanumeric())
}

fn strip_edited(line: &str) -> String {
    let mut s = line.to_string();
    for marker in ["(edited)", "（edited）"] {
        while let Some(pos) = s.find(marker) {
            s.replace_range(pos..pos + marker.len(), "");
        }
    }
    s
}

/// "10:32 AM", "10:32", "Today at 10:32 AM", "Yesterday at 9:05 PM".
fn is_time_line(line: &str) -> bool {
    let rest = strip_day_prefix(line);
    is_time(rest)
}

fn strip_day_prefix(line: &str) -> &str {
    for prefix in ["Today at ", "Yesterday at ", "today at ", "yesterday at "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    line
}

fn is_time(s: &str) -> bool {
    let s = s.trim();
    let (digits, suffix) = match s.find([' ', '\u{a0}']) {
        Some(pos) => (&s[..pos], s[pos..].trim()),
        None => (s, ""),
    };
    let suffix_ok = suffix.is_empty() || matches!(suffix.to_ascii_uppercase().as_str(), "AM" | "PM");
    if !suffix_ok {
        return false;
    }
    let Some((h, m)) = digits.split_once(':') else {
        return false;
    };
    let hour_ok = (1..=2).contains(&h.len()) && h.chars().all(|c| c.is_ascii_digit());
    let min_ok = m.len() == 2 && m.chars().all(|c| c.is_ascii_digit());
    hour_ok && min_ok
}

/// "Alex Wallar  10:32 AM" copied as a single line.
fn ends_with_time(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 2 || words.len() > 6 {
        return false;
    }
    let tail2 = format!("{} {}", words[words.len() - 2], words[words.len() - 1]);
    let name_len = if is_time(&tail2) {
        words.len() - 2
    } else if is_time(words[words.len() - 1]) {
        words.len() - 1
    } else {
        return false;
    };
    (1..=4).contains(&name_len) && looks_like_name(&words[..name_len])
}

/// A short line without terminal punctuation, immediately followed by a time line.
fn is_author_line(line: &str, next: &str) -> bool {
    if line.is_empty() || !is_time_line(next) {
        return false;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    (1..=4).contains(&words.len()) && looks_like_name(&words)
}

fn looks_like_name(words: &[&str]) -> bool {
    words.iter().all(|w| {
        !w.ends_with(['.', '!', '?', ',', ';', ':'])
            && w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '(')
    })
}

/// A row that is only emoji codes and reaction counts, e.g. ":+1: 3 :tada: 1".
fn is_reaction_row(line: &str) -> bool {
    if line.is_empty() {
        return false;
    }
    let mut saw_emoji = false;
    for w in line.split_whitespace() {
        if is_emoji_code(w) {
            saw_emoji = true;
        } else if !w.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    saw_emoji
}

/// Lines Slack adds around threads and message groups.
fn is_chrome_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower == "new messages"
        || lower == "new"
        || lower.ends_with(" replies") && lower.split_whitespace().count() == 2
    {
        return true;
    }
    if lower == "1 reply" {
        return true;
    }
    if lower.starts_with("last reply ")
        || lower.starts_with("view thread")
        || lower.starts_with("also send to ")
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_author_and_timestamp_lines() {
        let input = "Alex Wallar\n  10:32 AM\nhey, can you look at this?\nSure thing (edited)\n:+1: 2\n";
        let out = clean(input);
        assert_eq!(out, "hey, can you look at this?\nSure thing");
    }

    #[test]
    fn strips_single_line_author_time() {
        let input = "Alex Wallar  10:32 AM\nhello there\nJane Doe 9:05 PM\nhi";
        assert_eq!(clean(input), "hello there\nhi");
    }

    #[test]
    fn keeps_normal_text() {
        let input = "# Title\n\nSome text at 10:30 was fine.\nAnother line";
        assert_eq!(clean(input), input);
    }

    #[test]
    fn detects_emoji_codes() {
        assert!(is_emoji_code(":smile:"));
        assert!(is_emoji_code(":+1:"));
        assert!(is_emoji_code(":thumbsup::skin-tone-2:"));
        assert!(!is_emoji_code("::"));
        assert!(!is_emoji_code("note:"));
        assert!(!is_emoji_code(":"));
    }

    #[test]
    fn time_detection() {
        assert!(is_time("10:32 AM"));
        assert!(is_time("9:05"));
        assert!(is_time("10:32 pm"));
        assert!(!is_time("10:3"));
        assert!(!is_time("ratio 1:2 is"));
        assert!(is_time_line("Today at 10:32 AM"));
    }
}
