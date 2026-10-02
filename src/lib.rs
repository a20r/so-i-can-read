//! so-i-can-read: a Rapid Serial Visual Presentation reader.
//!
//! The pure modules (`text`, `slack`, `timing`, `settings`, `input`) have no
//! browser dependency and are unit tested natively. `app` is the wasm front end.

pub mod input;
pub mod resume;
pub mod settings;
pub mod slack;
pub mod text;
pub mod timing;

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod fetch;
#[cfg(target_arch = "wasm32")]
mod html;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    console_error_panic_hook::set_once();
    app::run()
}
