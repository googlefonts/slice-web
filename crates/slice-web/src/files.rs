//! Getting a font into the page and the result back out.
//!
//! Everything here stays on the machine. There is no upload: the file is read by the
//! browser, handed to the WebAssembly engine, and the result is handed straight back as
//! a download. Nothing is sent anywhere, which for unreleased typefaces is the whole
//! reason a desktop application was worth having in the first place.

use js_sys::{Array, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, File, HtmlAnchorElement, Url};

/// Read a `File` the user chose or dropped.
pub async fn read_file(file: File) -> Result<(String, Vec<u8>), String> {
    let name = file.name();
    let buffer = JsFuture::from(file.array_buffer())
        .await
        .map_err(|e| format!("could not read {name}: {}", describe(&e)))?;
    let bytes = Uint8Array::new(&buffer).to_vec();
    Ok((name, bytes))
}

/// Hand `bytes` to the browser as a download named `file_name`.
pub fn download(bytes: &[u8], file_name: &str, mime_type: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;

    // Copy into a JS-owned array; the Blob must not borrow from wasm memory, which can
    // be reallocated out from under it.
    let array = Uint8Array::new_with_length(bytes.len() as u32);
    array.copy_from(bytes);
    let parts = Array::new();
    parts.push(&array.buffer());

    let options = BlobPropertyBag::new();
    options.set_type(mime_type);
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options)
        .map_err(|e| format!("could not build the download: {}", describe(&e)))?;

    let url = Url::create_object_url_with_blob(&blob)
        .map_err(|e| format!("could not build the download: {}", describe(&e)))?;

    let anchor: HtmlAnchorElement = document
        .create_element("a")
        .map_err(|e| describe(&e))?
        .dyn_into()
        .map_err(|_| "could not create a download link".to_string())?;
    anchor.set_href(&url);
    anchor.set_download(file_name);
    // Kept out of the layout; it exists only to be clicked.
    anchor.style().set_property("display", "none").ok();
    document
        .body()
        .ok_or("no body")?
        .append_child(&anchor)
        .map_err(|e| describe(&e))?;
    anchor.click();
    anchor.remove();

    // The object URL pins the blob in memory until it is revoked.
    Url::revoke_object_url(&url).ok();
    Ok(())
}

/// A readable message from a thrown JavaScript value.
fn describe(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            value
                .dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

/// The file extensions the open dialog should offer.
pub const ACCEPTED_EXTENSIONS: &str =
    ".ttf,.otf,.woff,.woff2,font/ttf,font/otf,font/woff,font/woff2";

/// The sample font bundled with the page, for visitors who arrive without one.
///
/// The real Recursive, not a subset: 1304 glyphs over five axes, with `avar`, `GDEF`,
/// `GPOS`, `GSUB` feature variations, `HVAR` and `MVAR`. A three-glyph subset made the
/// tool look like a toy and exercised almost none of it; someone trying Slice for the
/// first time should be slicing a font that behaves like the ones they own.
///
/// Shipped as WOFF2, which is 719 kB against the sfnt's 2.4 MB and is fetched only when
/// the link is clicked, so it costs nothing to anyone who arrives with their own font.
/// It also means every use of the sample exercises the WOFF2 decoder on a real font.
///
/// The brackets are percent-encoded in the path but not in the name: the first is a URL,
/// the second is what the file is called.
pub const SAMPLE_PATH: &str = "./fonts/Recursive%5BCASL,CRSV,MONO,slnt,wght%5D.woff2";
pub const SAMPLE_NAME: &str = "Recursive[CASL,CRSV,MONO,slnt,wght].woff2";

/// Fetch a font from the same origin.
///
/// Used only for the bundled sample. Fonts the user opens are never fetched or sent
/// anywhere; they are read straight from the file the browser hands over.
pub async fn fetch_same_origin(path: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let response = JsFuture::from(window.fetch_with_str(path))
        .await
        .map_err(|e| format!("could not fetch {path}: {}", describe(&e)))?;
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| "unexpected response".to_string())?;
    if !response.ok() {
        return Err(format!(
            "could not fetch {path}: {} {}",
            response.status(),
            response.status_text()
        ));
    }
    let buffer = JsFuture::from(response.array_buffer().map_err(|e| describe(&e))?)
        .await
        .map_err(|e| describe(&e))?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

/// True when the page was opened with `?sample` in the URL.
///
/// Loads the bundled font on start, so a link can show the tool already working. It is
/// also how the browser smoke test drives the application.
pub fn wants_sample() -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .map(|search| search.contains("sample"))
        .unwrap_or(false)
}
