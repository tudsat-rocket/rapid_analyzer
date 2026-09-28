//! The browser's side of what the desktop app does with files. A web page
//! never sees a path: files arrive as bytes (see `App::pick_file`) and leave
//! as downloads.

use anyhow::{Context as _, Result, anyhow};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;

/// How long a download's object URL outlives the click that started it. The
/// browser reads the blob asynchronously, so revoking it on the spot can
/// cancel the download; a minute is far longer than that takes.
const REVOKE_AFTER_MS: i32 = 60_000;

fn js_error(e: JsValue) -> anyhow::Error {
    anyhow!("{e:?}")
}

/// Offers `bytes` to the user as a file called `file_name`, the way a link
/// with a `download` attribute does.
pub fn download(file_name: &str, bytes: &[u8], mime: &str) -> Result<()> {
    let window = web_sys::window().context("no browser window")?;
    let document = window.document().context("no document")?;

    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(js_error)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js_error)?;

    let anchor: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(js_error)?
        .dyn_into()
        .map_err(|_| anyhow!("<a> is not an anchor element"))?;
    anchor.set_href(&url);
    anchor.set_download(file_name);
    anchor.click();

    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    window
        .set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), REVOKE_AFTER_MS)
        .map_err(js_error)?;
    Ok(())
}
