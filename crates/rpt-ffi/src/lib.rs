//! C ABI for RapidPdfTrans.
//!
//! Handles are opaque. Strings returned by `rpt_extract` and `rpt_translate`
//! are freed with `rpt_string_free`. `rpt_last_error` points at thread-local
//! storage and must not be freed. The API key is never read from JSON; set
//! `RPT_LLM_API_KEY` in the environment before calling `rpt_translate`.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use rpt_core::{
    translate_extraction, ExtractOptions, OpenOptions, PdfDocument, RewriteOptions,
    TranslateOptions, TranslatorBackend,
};

/// Opaque document. Do not dereference.
#[repr(C)]
pub struct RptDocument {
    _private: [u8; 0],
}

struct DocumentInner {
    pdf: PdfDocument,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(message: &str) {
    let cleaned = message.replace('\0', " ");
    let value = CString::new(cleaned).unwrap_or_else(|_| CString::new("error").expect("cstring"));
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(value));
}

fn clear_error() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

fn panic_message(err: Box<dyn std::any::Any + Send>) -> String {
    if let Some(msg) = err.downcast_ref::<&str>() {
        (*msg).to_string()
    } else if let Some(msg) = err.downcast_ref::<String>() {
        msg.clone()
    } else {
        "panic at the FFI boundary".into()
    }
}

/// # Safety
/// `ptr` is null or a valid null-terminated UTF-8 C string.
unsafe fn cstr<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        ""
    } else {
        CStr::from_ptr(ptr).to_str().unwrap_or("")
    }
}

fn into_raw(doc: DocumentInner) -> *mut RptDocument {
    Box::into_raw(Box::new(doc)) as *mut RptDocument
}

/// # Safety
/// `doc` is null or a pointer returned by `rpt_open` that has not been freed.
unsafe fn from_raw<'a>(doc: *mut RptDocument) -> Option<&'a DocumentInner> {
    if doc.is_null() {
        None
    } else {
        Some(&*(doc as *const DocumentInner))
    }
}

/// # Safety
/// `doc` is null or a pointer returned by `rpt_open` that has not been freed.
unsafe fn from_raw_mut<'a>(doc: *mut RptDocument) -> Option<&'a mut DocumentInner> {
    if doc.is_null() {
        None
    } else {
        Some(&mut *(doc as *mut DocumentInner))
    }
}

fn json_string(value: &serde_json::Value) -> Result<CString, String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    CString::new(text).map_err(|e| e.to_string())
}

fn guard<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(panic) => Err(panic_message(panic)),
    }
}

/// # Safety
/// `path` and `options_json` are null-terminated UTF-8 strings, or null.
/// The returned handle is freed with `rpt_free`.
#[no_mangle]
pub unsafe extern "C" fn rpt_open(
    path: *const c_char,
    options_json: *const c_char,
) -> *mut RptDocument {
    let opened = guard(|| {
        if path.is_null() {
            return Err("path is null".into());
        }
        let path = unsafe { CStr::from_ptr(path) }
            .to_str()
            .map_err(|_| "path is not utf-8".to_string())?;
        let options = unsafe { cstr(options_json) };
        let open_opts = OpenOptions::from_json(options).map_err(|e| e.to_string())?;
        let warning = api_key_warning(options);
        let pdf = PdfDocument::open_with(path, &open_opts).map_err(|e| e.to_string())?;
        Ok((pdf, warning))
    });
    match opened {
        Ok((pdf, warning)) => {
            if let Some(warning) = warning {
                set_error(&warning);
            } else {
                clear_error();
            }
            into_raw(DocumentInner { pdf })
        }
        Err(message) => {
            set_error(&message);
            ptr::null_mut()
        }
    }
}

/// # Safety
/// `doc` was returned by `rpt_open` and has not been freed.
/// `options_json` is a null-terminated UTF-8 string, or null.
/// The returned string is freed with `rpt_string_free`.
#[no_mangle]
pub unsafe extern "C" fn rpt_extract(
    doc: *mut RptDocument,
    options_json: *const c_char,
) -> *mut c_char {
    let extracted = guard(|| {
        let inner = unsafe { from_raw(doc) }.ok_or_else(|| "document is null".to_string())?;
        let options = unsafe { cstr(options_json) };
        let extract_opts = ExtractOptions::from_json(options).map_err(|e| e.to_string())?;
        let extraction = inner.pdf.extract_with(&extract_opts);
        let value = extraction.to_json_value().map_err(|e| e.to_string())?;
        json_string(&value)
    });
    finish_string(extracted)
}

/// # Safety
/// `doc` was returned by `rpt_open` and has not been freed.
/// `options_json` is a null-terminated UTF-8 string, or null.
/// `RPT_LLM_API_KEY` must be set. An `api_key` field in JSON is ignored.
/// The returned string is freed with `rpt_string_free`.
#[no_mangle]
pub unsafe extern "C" fn rpt_translate(
    doc: *mut RptDocument,
    options_json: *const c_char,
) -> *mut c_char {
    let translated = guard(|| {
        let inner = unsafe { from_raw(doc) }.ok_or_else(|| "document is null".to_string())?;
        let options = unsafe { cstr(options_json) };
        let (translate_opts, mut warnings) =
            TranslateOptions::from_json(options).map_err(|e| e.to_string())?;
        let extract_opts = ExtractOptions::from_json(options).map_err(|e| e.to_string())?;
        let client = TranslatorBackend::from_env(&translate_opts).map_err(|e| e.to_string())?;
        let mut extraction = inner.pdf.extract_with(&extract_opts);
        let report = translate_extraction(&mut extraction, &translate_opts, &client)
            .map_err(|e| e.to_string())?;
        let mut value = extraction.to_json_value().map_err(|e| e.to_string())?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "translations".into(),
                serde_json::to_value(&report.segments).map_err(|e| e.to_string())?,
            );
            obj.insert(
                "translator".into(),
                serde_json::json!({
                    "model": client.model(),
                    "backend": client.label(),
                    "calls": report.calls,
                    "cache_hits": report.cache_hits,
                }),
            );
            if !warnings.is_empty() {
                obj.insert(
                    "warnings".into(),
                    serde_json::json!(std::mem::take(&mut warnings)),
                );
            }
        }
        json_string(&value)
    });
    finish_string(translated)
}

/// # Safety
/// `doc` was returned by `rpt_open` and has not been freed.
/// `path` and `options_json` are null-terminated UTF-8 strings, or null.
/// `RPT_LLM_API_KEY` must be set. An `api_key` field in JSON is ignored.
/// Returns 0 after the translated PDF is written.
#[no_mangle]
pub unsafe extern "C" fn rpt_save(
    doc: *mut RptDocument,
    path: *const c_char,
    options_json: *const c_char,
) -> c_int {
    let saved = guard(|| {
        let inner = unsafe { from_raw_mut(doc) }.ok_or_else(|| "document is null".to_string())?;
        if path.is_null() {
            return Err("path is null".into());
        }
        let path = unsafe { CStr::from_ptr(path) }
            .to_str()
            .map_err(|_| "path is not utf-8".to_string())?;
        if path.is_empty() {
            return Err("path is empty".into());
        }
        let options = unsafe { cstr(options_json) };
        let (translate_opts, _) =
            TranslateOptions::from_json(options).map_err(|e| e.to_string())?;
        let extract_opts = ExtractOptions::from_json(options).map_err(|e| e.to_string())?;
        let client =
            TranslatorBackend::from_env(&translate_opts).map_err(|e| scrub(&e.to_string()))?;
        let mut extraction = inner.pdf.extract_with(&extract_opts);
        let report = translate_extraction(&mut extraction, &translate_opts, &client)
            .map_err(|e| scrub(&e.to_string()))?;
        inner
            .pdf
            .rewrite(
                &mut extraction,
                &report,
                &RewriteOptions {
                    mode: translate_opts.output_mode,
                    font_bytes: None,
                },
            )
            .map_err(|e| e.to_string())?;
        extraction.assert_complete().map_err(|e| e.to_string())?;
        inner.pdf.save_file(path).map_err(|e| e.to_string())?;
        Ok(())
    });
    match saved {
        Ok(()) => {
            clear_error();
            0
        }
        Err(message) => {
            set_error(&message);
            -1
        }
    }
}

fn scrub(message: &str) -> String {
    let mut cleaned = message.to_string();
    for key in [
        "RPT_LLM_API_KEY",
        "RPT_GOOGLE_API_KEY",
        "RPT_GOOGLE_PRIVATE_KEY",
    ] {
        if let Ok(secret) = std::env::var(key) {
            if secret.len() >= 6 {
                cleaned = cleaned.replace(&secret, "[redacted]");
            }
        }
    }
    cleaned
}

/// # Safety
/// `doc` was returned by `rpt_open` and is not used again.
#[no_mangle]
pub unsafe extern "C" fn rpt_free(doc: *mut RptDocument) {
    if doc.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(doc as *mut DocumentInner) });
}

/// # Safety
/// `text` was returned by `rpt_extract` or `rpt_translate`, or is null.
#[no_mangle]
pub unsafe extern "C" fn rpt_string_free(text: *mut c_char) {
    if text.is_null() {
        return;
    }
    drop(unsafe { CString::from_raw(text) });
}

/// Pointer to the last error on this thread. Do not free it.
#[no_mangle]
pub extern "C" fn rpt_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|text| text.as_ptr())
            .unwrap_or(ptr::null())
    })
}

fn finish_string(result: Result<CString, String>) -> *mut c_char {
    match result {
        Ok(text) => {
            clear_error();
            text.into_raw()
        }
        Err(message) => {
            set_error(&message);
            ptr::null_mut()
        }
    }
}

fn api_key_warning(options: &str) -> Option<String> {
    if options.contains("\"api_key\"") || options.contains("\"apiKey\"") {
        Some("api_key in options JSON is ignored; set RPT_LLM_API_KEY in the environment".into())
    } else {
        None
    }
}

#[cfg(test)]
mod abi_tests {
    use super::*;
    use std::ffi::{CStr, CString};

    #[test]
    fn open_extract_and_save_stub() {
        let pdf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/hello.pdf");
        let path = CString::new(pdf.to_string_lossy().as_bytes()).unwrap();
        let doc = unsafe { rpt_open(path.as_ptr(), ptr::null()) };
        assert!(!doc.is_null(), "{}", unsafe {
            CStr::from_ptr(rpt_last_error()).to_string_lossy()
        });
        let json = unsafe { rpt_extract(doc, ptr::null()) };
        assert!(!json.is_null());
        let text = unsafe { CStr::from_ptr(json) }
            .to_string_lossy()
            .into_owned();
        assert!(text.contains("Hello"), "{text}");
        assert!(text.contains("pending"), "{text}");
        unsafe { rpt_string_free(json) };
        if std::env::var("RPT_LLM_API_KEY")
            .ok()
            .is_some_and(|key| !key.trim().is_empty())
        {
            unsafe { rpt_free(doc) };
            return;
        }
        let out = CString::new("/tmp/rpt-ffi-should-not-write.pdf").unwrap();
        let rc = unsafe { rpt_save(doc, out.as_ptr(), ptr::null()) };
        assert_eq!(rc, -1);
        let err = unsafe { CStr::from_ptr(rpt_last_error()) }
            .to_string_lossy()
            .into_owned();
        assert!(err.contains("RPT_LLM_API_KEY"), "{err}");
        assert!(!std::path::Path::new("/tmp/rpt-ffi-should-not-write.pdf").exists());
        unsafe { rpt_free(doc) };
        unsafe { rpt_free(ptr::null_mut()) };
        unsafe { rpt_string_free(ptr::null_mut()) };
    }

    #[test]
    fn garbage_path_sets_last_error() {
        let path = CString::new("testdata/does-not-exist.pdf").unwrap();
        let doc = unsafe { rpt_open(path.as_ptr(), ptr::null()) };
        assert!(doc.is_null());
        let err = unsafe { CStr::from_ptr(rpt_last_error()) }
            .to_string_lossy()
            .into_owned();
        assert!(!err.is_empty());
    }
}
