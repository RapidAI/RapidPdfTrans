//! Write a one-page WinAnsi PDF used by the language-binding smoke tests.
//!
//! `cargo run -p rpt-core --example hello_pdf -- testdata/hello.pdf`

use std::env;
use std::fs;
use std::path::PathBuf;

use lopdf::{dictionary, Dictionary, Document, Object, Stream};

fn main() {
    let mut doc = Document::with_version("1.4");
    doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
        "FirstChar" => 32,
        "LastChar" => 122,
        "Widths" => (32..=122).map(|_| Object::Integer(600)).collect::<Vec<_>>(),
    });
    let mut fonts = Dictionary::new();
    fonts.set("F1", font);
    let mut resources = Dictionary::new();
    resources.set("Font", fonts);
    let content = Stream::new(
        Dictionary::new(),
        b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET".to_vec(),
    );
    let content_id = doc.add_object(content);
    let page = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => resources,
    });
    doc.set_object(
        pages_id,
        dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page.into()],
            "Count" => 1,
        },
    );
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog);
    let path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("testdata/hello.pdf"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create testdata");
    }
    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("save");
    fs::write(&path, buf).expect("write");
    eprintln!("wrote {}", path.display());
}
