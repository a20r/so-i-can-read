//! Converts an HTML page into markdown-ish text using the browser's own parser,
//! so the wasm bundle stays small.

use wasm_bindgen::JsCast;
use web_sys::{DomParser, Element, Node, SupportedType};

pub struct Extracted {
    pub title: Option<String>,
    pub text: String,
}

const DROP_SELECTOR: &str = "script,style,noscript,template,nav,header,footer,aside,svg,iframe,form,button,\
    [aria-hidden=true],[role=navigation],[role=banner],[role=contentinfo],.sidebar,.nav,.menu,.comments,.advertisement";

pub fn extract(html: &str) -> Result<Extracted, String> {
    let parser = DomParser::new().map_err(|_| "DOMParser unavailable")?;
    let doc = parser
        .parse_from_string(html, SupportedType::TextHtml)
        .map_err(|_| "could not parse page")?;
    let title = doc.title();
    let title = (!title.trim().is_empty()).then(|| title.trim().to_string());

    if let Ok(nodes) = doc.query_selector_all(DROP_SELECTOR) {
        // Collect first: removing while iterating a live list skips nodes.
        let mut to_remove = Vec::new();
        for i in 0..nodes.length() {
            if let Some(n) = nodes.item(i) {
                to_remove.push(n);
            }
        }
        for n in to_remove {
            if let Some(parent) = n.parent_node() {
                let _ = parent.remove_child(&n);
            }
        }
    }

    let root: Option<Element> = [
        "article",
        "main",
        "[role=main]",
        "#content",
        ".post",
        ".entry-content",
        "body",
    ]
    .iter()
    .find_map(|sel| doc.query_selector(sel).ok().flatten());
    let Some(root) = root else {
        return Err("empty page".into());
    };

    let mut out = String::new();
    walk(&root, &mut out, false);
    Ok(Extracted {
        title,
        text: collapse_blank_lines(&out),
    })
}

fn walk(node: &Node, out: &mut String, in_pre: bool) {
    match node.node_type() {
        Node::TEXT_NODE => {
            let text = node.text_content().unwrap_or_default();
            if in_pre {
                out.push_str(&text);
            } else {
                push_collapsed(out, &text);
            }
        }
        Node::ELEMENT_NODE => walk_element(node, out, in_pre),
        _ => {}
    }
}

fn walk_element(node: &Node, out: &mut String, in_pre: bool) {
    let tag = node.node_name().to_ascii_lowercase();
    match tag.as_str() {
        "br" => out.push('\n'),
        "hr" => out.push_str("\n\n---\n\n"),
        "img" => {
            if let Some(el) = node.dyn_ref::<Element>() {
                if let Some(alt) = el.get_attribute("alt") {
                    if !alt.trim().is_empty() {
                        push_collapsed(out, &format!(" {} ", alt.trim()));
                    }
                }
            }
        }
        "pre" => {
            let text = node.text_content().unwrap_or_default();
            out.push_str("\n\n```\n");
            out.push_str(text.trim_end_matches('\n'));
            out.push_str("\n```\n\n");
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = tag[1..].parse::<usize>().unwrap_or(1);
            let mut inner = String::new();
            walk_children(node, &mut inner, in_pre);
            let inner = inner.trim();
            if !inner.is_empty() {
                out.push_str("\n\n");
                out.push_str(&"#".repeat(level));
                out.push(' ');
                out.push_str(&inner.replace('\n', " "));
                out.push_str("\n\n");
            }
        }
        "li" => {
            let mut inner = String::new();
            walk_children(node, &mut inner, in_pre);
            let inner = inner.trim();
            if !inner.is_empty() {
                out.push_str("\n- ");
                out.push_str(&inner.replace("\n\n", "\n  "));
                out.push('\n');
            }
        }
        "blockquote" => {
            let mut inner = String::new();
            walk_children(node, &mut inner, in_pre);
            out.push_str("\n\n");
            for line in inner.trim().lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }
        "td" | "th" => {
            walk_children(node, out, in_pre);
            out.push_str(" | ");
        }
        "tr" => {
            out.push('\n');
            walk_children(node, out, in_pre);
            out.push('\n');
        }
        "strong" | "b" => wrap_inline(node, out, in_pre, "**"),
        "em" | "i" | "cite" => wrap_inline(node, out, in_pre, "*"),
        "code" | "kbd" | "samp" => {
            let text = node.text_content().unwrap_or_default();
            let text = text.trim();
            if !text.is_empty() && !text.contains('`') {
                out.push('`');
                out.push_str(text);
                out.push('`');
            } else {
                push_collapsed(out, text);
            }
        }
        "p" | "div" | "section" | "article" | "main" | "ul" | "ol" | "dl" | "dt" | "dd" | "table"
        | "thead" | "tbody" | "figure" | "figcaption" | "details" | "summary" | "address" | "body" => {
            out.push_str("\n\n");
            walk_children(node, out, in_pre);
            out.push_str("\n\n");
        }
        _ => walk_children(node, out, in_pre),
    }
}

fn wrap_inline(node: &Node, out: &mut String, in_pre: bool, marker: &str) {
    let mut inner = String::new();
    walk_children(node, &mut inner, in_pre);
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return;
    }
    if inner.starts_with(char::is_whitespace) {
        out.push(' ');
    }
    out.push_str(marker);
    out.push_str(trimmed);
    out.push_str(marker);
    if inner.ends_with(char::is_whitespace) {
        out.push(' ');
    }
}

fn walk_children(node: &Node, out: &mut String, in_pre: bool) {
    let children = node.child_nodes();
    for i in 0..children.length() {
        if let Some(child) = children.item(i) {
            walk(&child, out, in_pre);
        }
    }
}

/// Append text with runs of whitespace collapsed to a single space.
fn push_collapsed(out: &mut String, text: &str) {
    let mut last_space = out.ends_with(char::is_whitespace);
    for c in text.chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0;
    for line in s.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}
