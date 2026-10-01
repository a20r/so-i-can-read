//! Loads a URL from the browser. Tries a direct request first (works for raw
//! markdown, gists and CORS-friendly sites) and falls back to a read-through
//! proxy that returns markdown.

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, Request, RequestInit, RequestMode, Response};

use crate::html;
use crate::input::strip_reader_preamble;

pub struct Fetched {
    pub title: Option<String>,
    pub text: String,
}

const TIMEOUT_MS: i32 = 20_000;

pub async fn load(url: &str, proxy: &str) -> Result<Fetched, String> {
    match fetch_text(url).await {
        Ok((content_type, body)) => {
            if looks_like_html(&content_type, &body) {
                let extracted = html::extract(&body)?;
                if extracted.text.split_whitespace().count() < 20 {
                    // Probably a JS-rendered shell; the proxy renders pages properly.
                    return load_via_proxy(url, proxy).await.or(Ok(Fetched {
                        title: extracted.title,
                        text: extracted.text,
                    }));
                }
                Ok(Fetched {
                    title: extracted.title,
                    text: extracted.text,
                })
            } else {
                Ok(Fetched {
                    title: None,
                    text: body,
                })
            }
        }
        Err(direct_err) => load_via_proxy(url, proxy)
            .await
            .map_err(|proxy_err| format!("{proxy_err} (direct load failed: {direct_err})")),
    }
}

async fn load_via_proxy(url: &str, proxy: &str) -> Result<Fetched, String> {
    let proxied = format!("{}{}", proxy.trim(), url);
    let (content_type, body) = fetch_text(&proxied).await?;
    if looks_like_html(&content_type, &body) {
        let extracted = html::extract(&body)?;
        return Ok(Fetched {
            title: extracted.title,
            text: extracted.text,
        });
    }
    let (title, text) = strip_reader_preamble(&body);
    Ok(Fetched { title, text })
}

fn looks_like_html(content_type: &str, body: &str) -> bool {
    if content_type.contains("html") {
        return true;
    }
    let head = body.trim_start().get(..64).unwrap_or(body).to_ascii_lowercase();
    head.starts_with("<!doctype") || head.starts_with("<html")
}

/// Returns (content-type, body) or a human-readable error.
async fn fetch_text(url: &str) -> Result<(String, String), String> {
    let window = web_sys::window().ok_or("no window")?;
    let controller = AbortController::new().map_err(|_| "AbortController unavailable")?;
    let opts = RequestInit::new();
    opts.set_mode(RequestMode::Cors);
    opts.set_signal(Some(&controller.signal()));
    let request = Request::new_with_str_and_init(url, &opts).map_err(|e| js_error(&e))?;

    let abort = {
        let controller = controller.clone();
        Closure::once(move || controller.abort())
    };
    let timer = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(abort.as_ref().unchecked_ref(), TIMEOUT_MS)
        .map_err(|e| js_error(&e))?;

    let result = JsFuture::from(window.fetch_with_request(&request)).await;
    window.clear_timeout_with_handle(timer);
    drop(abort);

    let response: Response = result
        .map_err(|e| js_error(&e))?
        .dyn_into()
        .map_err(|_| "bad response")?;
    if !response.ok() {
        return Err(format!("server returned {}", response.status()));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let text = JsFuture::from(response.text().map_err(|e| js_error(&e))?)
        .await
        .map_err(|e| js_error(&e))?;
    Ok((content_type, text.as_string().unwrap_or_default()))
}

fn js_error(e: &JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    if let Some(msg) = js_sys::Reflect::get(e, &JsValue::from_str("message"))
        .ok()
        .and_then(|m| m.as_string())
    {
        return msg;
    }
    "request failed".into()
}
