//! Small helpers shared across the app: DOM lookup, event wiring, JS interop.

use js_sys::{Function, Object, Promise, Reflect};
use wasm_bindgen::convert::FromWasmAbi;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Document, Element, EventTarget, Window};

/// Look up an element by id and cast it, panicking early if the markup drifts.
pub fn by_id<T: JsCast>(doc: &Document, id: &str) -> T {
    doc.get_element_by_id(id)
        .unwrap_or_else(|| panic!("missing element #{id}"))
        .dyn_into::<T>()
        .unwrap_or_else(|_| panic!("element #{id} has an unexpected type"))
}

/// Add an event listener and hand back the closure so the caller controls its lifetime.
pub fn listen_owned<E>(
    target: &EventTarget,
    event: &str,
    handler: impl FnMut(E) + 'static,
) -> Closure<dyn FnMut(E)>
where
    E: FromWasmAbi + 'static,
{
    let closure = Closure::<dyn FnMut(E)>::new(handler);
    target
        .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
        .expect("add_event_listener failed");
    closure
}

/// Add a listener that lives for the lifetime of the page.
pub fn listen<E>(target: &EventTarget, event: &str, handler: impl FnMut(E) + 'static)
where
    E: FromWasmAbi + 'static,
{
    listen_owned(target, event, handler).forget();
}

pub fn set_class(el: &Element, class: &str, on: bool) {
    let list = el.class_list();
    let _ = if on { list.add_1(class) } else { list.remove_1(class) };
}

/// Build a plain JS object from key/value pairs.
pub fn obj(entries: &[(&str, JsValue)]) -> Object {
    let o = Object::new();
    for (k, v) in entries {
        let _ = Reflect::set(&o, &JsValue::from_str(k), v);
    }
    o
}

/// Read a numeric property off any JS value.
pub fn num(target: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(target, &JsValue::from_str(key)).ok()?.as_f64()
}

/// Call a zero-arg method by name. Rejected promises are swallowed so they
/// don't surface as unhandled rejections (fullscreen, install prompt, …).
pub fn call_method(target: &JsValue, name: &str) -> Option<JsValue> {
    let f = Reflect::get(target, &JsValue::from_str(name))
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    let out = f.call0(target).ok()?;
    if let Ok(p) = out.clone().dyn_into::<Promise>() {
        spawn_local(async move {
            let _ = JsFuture::from(p).await;
        });
    }
    Some(out)
}

pub async fn sleep(ms: i32) {
    let promise = Promise::new(&mut |resolve, _| {
        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

// ── Fullscreen (with the webkit-prefixed fallbacks Safari still needs) ──────

pub fn is_fullscreen(doc: &Document) -> bool {
    let v: &JsValue = doc.as_ref();
    ["fullscreenElement", "webkitFullscreenElement"].iter().any(|k| {
        Reflect::get(v, &JsValue::from_str(k))
            .map(|x| !x.is_null() && !x.is_undefined())
            .unwrap_or(false)
    })
}

pub fn request_fullscreen(el: &Element) {
    let t: &JsValue = el.as_ref();
    let _ = call_method(t, "requestFullscreen").or_else(|| call_method(t, "webkitRequestFullscreen"));
}

pub fn exit_fullscreen(doc: &Document) {
    let t: &JsValue = doc.as_ref();
    let _ = call_method(t, "exitFullscreen").or_else(|| call_method(t, "webkitExitFullscreen"));
}

// ── Storage ─────────────────────────────────────────────────────────────────

pub fn storage_get(win: &Window, key: &str) -> Option<String> {
    win.local_storage().ok().flatten()?.get_item(key).ok().flatten()
}

pub fn storage_set(win: &Window, key: &str, value: &str) {
    if let Ok(Some(s)) = win.local_storage() {
        let _ = s.set_item(key, value);
    }
}

// ── Misc ────────────────────────────────────────────────────────────────────

pub fn parse_res(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Turn a getUserMedia / DOMException-ish error into something a person can act on.
pub fn describe_error(e: &JsValue) -> String {
    let get = |k: &str| {
        Reflect::get(e, &JsValue::from_str(k))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default()
    };
    let (name, message) = (get("name"), get("message"));
    match name.as_str() {
        "NotAllowedError" | "SecurityError" => {
            "Camera access is blocked — allow it in the browser's site settings".into()
        }
        "NotFoundError" => "No video input found. Connect your capture card.".into(),
        "NotReadableError" | "AbortError" => {
            "The device is busy or in use by another app".into()
        }
        "OverconstrainedError" => "The selected device is no longer available".into(),
        _ if !name.is_empty() => format!("{name}: {message}"),
        _ => e.as_string().unwrap_or_else(|| "Unknown error".into()),
    }
}
