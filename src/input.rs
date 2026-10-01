//! Classifies what the user pasted: a URL to fetch, or text to read directly.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Url(String),
    Text(String),
    Empty,
}

pub fn classify(raw: &str) -> Input {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Input::Empty;
    }
    if let Some(url) = as_url(trimmed) {
        return Input::Url(url);
    }
    Input::Text(trimmed.to_string())
}

/// Returns a normalised URL if the whole input is a single link.
pub fn as_url(s: &str) -> Option<String> {
    let s = s.trim().trim_matches(|c| c == '<' || c == '>');
    if s.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return (s.len() > 8).then(|| s.to_string());
    }
    // "example.com/path" without a scheme.
    let host = s.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return None;
    }
    let tld = labels.last().unwrap();
    let tld_ok = tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic());
    let labels_ok = labels
        .iter()
        .all(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if tld_ok && labels_ok {
        Some(format!("https://{s}"))
    } else {
        None
    }
}

/// Strip the preamble that the r.jina.ai reader prepends to its markdown.
/// Returns (title, markdown).
pub fn strip_reader_preamble(body: &str) -> (Option<String>, String) {
    let mut title = None;
    let mut rest = body;
    if let Some(pos) = body.find("Markdown Content:\n") {
        let head = &body[..pos];
        for line in head.lines() {
            if let Some(t) = line.strip_prefix("Title: ") {
                let t = t.trim();
                if !t.is_empty() {
                    title = Some(t.to_string());
                }
            }
        }
        rest = &body[pos + "Markdown Content:\n".len()..];
    }
    (title, rest.trim_start().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_urls() {
        assert_eq!(
            classify(" https://example.com/a?b=c "),
            Input::Url("https://example.com/a?b=c".into())
        );
        assert_eq!(
            classify("example.com/post"),
            Input::Url("https://example.com/post".into())
        );
        assert_eq!(classify("<https://x.io>"), Input::Url("https://x.io".into()));
    }

    #[test]
    fn classifies_text() {
        assert_eq!(classify("hello world"), Input::Text("hello world".into()));
        assert_eq!(classify("hello."), Input::Text("hello.".into()));
        assert_eq!(classify("v1.2"), Input::Text("v1.2".into()));
        assert_eq!(classify("  "), Input::Empty);
    }

    #[test]
    fn reader_preamble() {
        let body = "Title: Hi there\n\nURL Source: https://x\n\nMarkdown Content:\n# Hi\n\nbody";
        let (t, md) = strip_reader_preamble(body);
        assert_eq!(t.as_deref(), Some("Hi there"));
        assert_eq!(md, "# Hi\n\nbody");
        let (t, md) = strip_reader_preamble("plain");
        assert_eq!(t, None);
        assert_eq!(md, "plain");
    }
}
