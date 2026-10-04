//! Native Python bindings. This module calls the Rust core directly, not the C ABI.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use rpt_core::{
    translate_extraction, ExtractOptions, LlmTranslator, OpenOptions, PdfDocument, TranslateOptions,
};

fn py_err(err: impl ToString) -> PyErr {
    PyRuntimeError::new_err(err.to_string())
}

#[pyfunction]
fn version() -> &'static str {
    "0.1.0"
}

/// Extract glyphs as a JSON string. `options_json` is an optional JSON object.
#[pyfunction]
#[pyo3(signature = (path, options_json=None))]
fn extract(path: &str, options_json: Option<&str>) -> PyResult<String> {
    let options = options_json.unwrap_or("");
    let open_opts = OpenOptions::from_json(options).map_err(py_err)?;
    let extract_opts = ExtractOptions::from_json(options).map_err(py_err)?;
    let doc = PdfDocument::open_with(path, &open_opts).map_err(py_err)?;
    let extraction = doc.extract_with(&extract_opts);
    extraction.to_json_pretty().map_err(py_err)
}

/// Translate extracted text. Requires `RPT_LLM_API_KEY`. Does not rewrite the PDF.
#[pyfunction]
#[pyo3(signature = (path, options_json=None))]
fn translate(path: &str, options_json: Option<&str>) -> PyResult<String> {
    let options = options_json.unwrap_or("");
    let (opts, warnings) = TranslateOptions::from_json(options).map_err(py_err)?;
    let doc = PdfDocument::open(path).map_err(py_err)?;
    let client = LlmTranslator::from_env(&opts).map_err(py_err)?;
    let mut extraction = doc.extract();
    let report = translate_extraction(&mut extraction, &opts, &client).map_err(py_err)?;
    let mut value = extraction.to_json_value().map_err(py_err)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "translations".to_string(),
            serde_json::to_value(&report.segments).map_err(py_err)?,
        );
        obj.insert(
            "translator".to_string(),
            serde_json::json!({
                "model": client.model(),
                "calls": report.calls,
                "cache_hits": report.cache_hits,
            }),
        );
        if !warnings.is_empty() {
            obj.insert("warnings".to_string(), serde_json::json!(warnings));
        }
    }
    serde_json::to_string_pretty(&value).map_err(py_err)
}

#[pymodule]
fn rapidpdftrans(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(extract, m)?)?;
    m.add_function(wrap_pyfunction!(translate, m)?)?;
    Ok(())
}
