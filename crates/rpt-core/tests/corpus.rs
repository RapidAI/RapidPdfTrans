//! Generated-PDF corpus. Each test locks a failure mode that drops or shifts text:
//! encodings, CID widths, XObjects, annotations, Type3, clips, and bad operators.

use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use rpt_core::{minimal_ttf, Extraction, PdfDocument, SourceKind};

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 0.08
}

fn assert_close(a: f32, b: f32) {
    assert!(close(a, b), "{a} != {b}");
}

struct Builder {
    doc: Document,
    pages_id: ObjectId,
    kids: Vec<Object>,
}

impl Builder {
    fn new() -> Self {
        let mut doc = Document::with_version("1.4");
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let pages_id = doc.new_object_id();
        Self {
            doc,
            pages_id,
            kids: Vec::new(),
        }
    }

    fn add_page(
        &mut self,
        resources: Dictionary,
        content: &str,
        annots: Option<ObjectId>,
    ) -> ObjectId {
        let bytes = content.as_bytes().to_vec();
        let content_id = self.doc.add_object(Stream::new(Dictionary::new(), bytes));
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => self.pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => resources,
        };
        if let Some(annots_id) = annots {
            page.set("Annots", annots_id);
        }
        let page_id = self.doc.add_object(page);
        self.kids.push(page_id.into());
        page_id
    }

    fn font_dict(&mut self, font: ObjectId) -> Dictionary {
        let mut fonts = Dictionary::new();
        fonts.set("F1", font);
        let mut resources = Dictionary::new();
        resources.set("Font", fonts);
        resources
    }

    fn bytes(&mut self) -> Vec<u8> {
        self.finish(false)
    }

    fn bytes_modern(&mut self) -> Vec<u8> {
        self.finish(true)
    }

    fn finish(&mut self, modern: bool) -> Vec<u8> {
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => self.kids.clone(),
            "Count" => self.kids.len() as i64,
        };
        self.doc.set_object(self.pages_id, pages);
        let catalog_id = self.doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => self.pages_id,
        });
        self.doc.trailer.set("Root", catalog_id);
        let mut buf = Vec::new();
        if modern {
            self.doc.save_modern(&mut buf).expect("save modern");
        } else {
            self.doc.save_to(&mut buf).expect("save");
        }
        buf
    }
}

fn uniform_widths(first: i64, last: i64, width: i64) -> Vec<Object> {
    (first..=last).map(|_| Object::Integer(width)).collect()
}

fn type1(
    doc: &mut Document,
    subtype: &str,
    encoding: Object,
    first: i64,
    last: i64,
    width: i64,
) -> ObjectId {
    doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => subtype,
        "BaseFont" => "Helvetica",
        "Encoding" => encoding,
        "FirstChar" => first,
        "LastChar" => last,
        "Widths" => uniform_widths(first, last, width),
    })
}

fn extract(bytes: &[u8]) -> Extraction {
    let doc = PdfDocument::open_bytes(bytes).unwrap_or_else(|err| panic!("open failed: {err}"));
    doc.extract()
}

fn joined(ex: &Extraction) -> String {
    ex.glyphs.iter().map(|g| g.unicode.as_str()).collect()
}

#[test]
fn winansi_type1_and_truetype_advances() {
    for subtype in ["Type1", "TrueType"] {
        let mut b = Builder::new();
        let font = type1(&mut b.doc, subtype, "WinAnsiEncoding".into(), 32, 122, 600);
        let resources = b.font_dict(font);
        b.add_page(resources, "BT /F1 12 Tf 100 700 Td (Hello) Tj ET", None);
        let ex = extract(&b.bytes());
        assert_eq!(joined(&ex), "Hello", "{subtype} diags {:?}", ex.diagnostics);
        assert!(ex
            .glyphs
            .iter()
            .all(|g| !g.unmapped && !g.invisible && !g.clipped));
        for (i, glyph) in ex.glyphs.iter().enumerate() {
            assert_close(glyph.matrix[4], 100.0 + i as f32 * 7.2);
            assert_close(glyph.matrix[5], 700.0);
            assert_close(glyph.font_size, 12.0);
            assert_eq!(glyph.page_index, 0);
            assert_eq!(glyph.source.kind, SourceKind::PageContent);
            assert!(glyph.source.byte_end > glyph.source.byte_start);
        }
        // Coverage starts unresolved: a skipped glyph would shrink this list.
        let report = ex.coverage_report();
        assert!(!report.complete);
        assert_eq!(report.unresolved_ids.len(), 5);
        assert_eq!(report.pending, 5);
    }
}

#[test]
fn differences_encoding_and_unmapped_glyph() {
    let mut b = Builder::new();
    let mut encoding = Dictionary::new();
    encoding.set("Type", "Encoding");
    encoding.set("BaseEncoding", "WinAnsiEncoding");
    encoding.set(
        "Differences",
        vec![
            Object::Integer(65),
            Object::from("copyright"),
            Object::from("fi"),
            Object::from("TotallyUnknownGlyph"),
        ],
    );
    let font = type1(&mut b.doc, "Type1", encoding.into(), 65, 67, 500);
    let resources = b.font_dict(font);
    b.add_page(resources, "BT /F1 10 Tf 0 0 Td (ABC) Tj ET", None);
    let ex = extract(&b.bytes());
    assert_eq!(ex.glyphs.len(), 3, "{:?}", ex.diagnostics);
    assert_eq!(ex.glyphs[0].unicode, "\u{00A9}");
    assert_eq!(ex.glyphs[1].unicode, "\u{FB01}");
    assert!(ex.glyphs[2].unmapped, "unknown glyph name must be kept");
    assert_eq!(ex.glyphs[2].unicode, "");
    assert_eq!(ex.glyphs[2].char_code, vec![b'C']);
    assert_close(ex.glyphs[1].matrix[4], 5.0);
    assert_close(ex.glyphs[2].matrix[4], 10.0);
}

#[test]
fn cid_identity_tounicode_widths_and_ligature() {
    let mut b = Builder::new();
    let tounicode = b"1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
        3 beginbfchar\n<0001> <0048>\n<0002> <0069>\n<0003> <00660069>\nendbfchar\n\
        1 beginbfrange\n<0004> <0005> [<0043> <0044>]\nendbfrange\n";
    let to_id = b
        .doc
        .add_object(Stream::new(Dictionary::new(), tounicode.to_vec()));
    let cid = b.doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "TestCID",
        "DW" => 1000,
        "W" => vec![
            Object::Integer(1),
            Object::Array(vec![
                Object::Integer(600),
                Object::Integer(400),
                Object::Integer(800),
                Object::Integer(500),
                Object::Integer(500),
            ]),
        ],
        "CIDToGIDMap" => "Identity",
    });
    let font = b.doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "TestCID",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::from(cid)],
        "ToUnicode" => to_id,
    });
    let resources = b.font_dict(font);
    b.add_page(
        resources,
        "BT /F1 10 Tf 20 30 Td <00010002000300040005> Tj ET",
        None,
    );
    let ex = extract(&b.bytes());
    let text: Vec<_> = ex.glyphs.iter().map(|g| g.unicode.as_str()).collect();
    assert_eq!(text, vec!["H", "i", "fi", "C", "D"], "{:?}", ex.diagnostics);
    assert!(ex.glyphs.iter().all(|g| !g.unmapped));
    // width 600, 400, 800, 500 at 10 pt → advances 6, 4, 8, 5
    let xs: Vec<f32> = ex.glyphs.iter().map(|g| g.matrix[4]).collect();
    assert_close(xs[0], 20.0);
    assert_close(xs[1], 26.0);
    assert_close(xs[2], 30.0);
    assert_close(xs[3], 38.0);
    assert_close(xs[4], 43.0);
    assert!(ex.glyphs.iter().all(|g| close(g.matrix[5], 30.0)));
    assert!(ex.glyphs[2].unicode.chars().count() == 2);
}

#[test]
fn cid_without_tounicode_uses_embedded_cmap() {
    let mut b = Builder::new();
    let ttf = minimal_ttf(&[(0x41, 1), (0x42, 2)]);
    let file_id = b
        .doc
        .add_object(Stream::new(Dictionary::new(), ttf).with_compression(false));
    let descriptor = b.doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "EmbeddedCID",
        "Flags" => 32,
        "FontBBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
        "ItalicAngle" => 0,
        "Ascent" => 800,
        "Descent" => -200,
        "CapHeight" => 700,
        "StemV" => 80,
        "FontFile2" => file_id,
    });
    let cid = b.doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "EmbeddedCID",
        "FontDescriptor" => descriptor,
        "DW" => 1000,
        "CIDToGIDMap" => "Identity",
    });
    let font = b.doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "EmbeddedCID",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::from(cid)],
    });
    let resources = b.font_dict(font);
    b.add_page(resources, "BT /F1 10 Tf 0 0 Td <00010002> Tj ET", None);
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "AB", "diags {:?}", ex.diagnostics);
    assert_eq!(ex.glyphs[0].gid, Some(1));
    assert_eq!(ex.glyphs[1].gid, Some(2));
    assert_close(ex.glyphs[1].matrix[4], 10.0);
}

#[test]
fn predefined_cjk_cmaps() {
    let samples = [
        ("GBK-EUC-H", "<D6D0>", "中"),
        ("UniGB-UCS2-H", "<4E2D>", "中"),
        ("90ms-RKSJ-H", "<82A0>", "あ"),
        ("UniJIS-UCS2-V", "<3042>", "あ"),
        ("ETen-B5-H", "<A4A4>", "中"),
        ("KSCms-UHC-H", "<C7D1>", "한"),
    ];
    for (encoding, shown, expected) in samples {
        let mut b = Builder::new();
        let cid = b.doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "CIDFontType2",
            "BaseFont" => "CJK",
            "DW" => 1000,
        });
        let font = b.doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "CJK",
            "Encoding" => encoding,
            "DescendantFonts" => vec![Object::from(cid)],
        });
        let resources = b.font_dict(font);
        let content = format!("BT /F1 10 Tf 5 6 Td {shown} Tj ET");
        b.add_page(resources, &content, None);
        let ex = extract(&b.bytes());
        assert_eq!(
            joined(&ex),
            expected,
            "{encoding} diags {:?}",
            ex.diagnostics
        );
        assert_eq!(ex.glyphs.len(), 1, "{encoding}");
        assert!(!ex.glyphs[0].unmapped);
        let vertical = encoding.ends_with("-V");
        if vertical {
            // Default vx is half the horizontal width (0.5 em) → +5 at 10 pt.
            assert_close(ex.glyphs[0].matrix[4], 10.0);
            assert_close(ex.glyphs[0].advance[0], 0.0);
            assert!(ex.glyphs[0].advance[1] > 0.0, "{}", ex.glyphs[0].advance[1]);
        } else {
            assert_close(ex.glyphs[0].matrix[4], 5.0);
            assert_close(ex.glyphs[0].advance[0], 10.0);
        }
    }
}

#[test]
fn tj_kerning_moves_the_next_glyph() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 65, 66, 500);
    let resources = b.font_dict(font);
    b.add_page(
        resources,
        "BT /F1 10 Tf 0 0 Td [ (A) -250 (B) ] TJ ET",
        None,
    );
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "AB");
    assert_close(ex.glyphs[0].matrix[4], 0.0);
    // Width 500 at 10 pt is 5; TJ -250 adds 2.5, stored on the previous advance.
    assert_close(ex.glyphs[0].advance[0], 7.5);
    assert_close(ex.glyphs[1].matrix[4], 7.5);
    assert_close(ex.glyphs[1].advance[0], 5.0);
}

#[test]
fn q_q_and_cm_then_restored() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 65, 66, 1000);
    let resources = b.font_dict(font);
    let content = "\
q
2 0 0 2 50 50 cm
BT /F1 10 Tf 10 10 Td (A) Tj ET
Q
BT /F1 10 Tf 0 0 Td (B) Tj ET
";
    b.add_page(resources, content, None);
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "AB", "{:?}", ex.diagnostics);
    assert_close(ex.glyphs[0].matrix[4], 70.0);
    assert_close(ex.glyphs[0].matrix[5], 70.0);
    assert_close(ex.glyphs[0].matrix[0], 20.0);
    assert_close(ex.glyphs[1].matrix[4], 0.0);
    assert_close(ex.glyphs[1].matrix[5], 0.0);
    assert_close(ex.glyphs[1].matrix[0], 10.0);
}

#[test]
fn nested_form_xobjects() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 65, 90, 1000);
    let mut x2_fonts = Dictionary::new();
    x2_fonts.set("F1", font);
    let mut x2_res = Dictionary::new();
    x2_res.set("Font", x2_fonts);
    let x2_stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 10.into(), 20.into()],
            "Resources" => x2_res,
        },
        b"BT /F1 10 Tf 5 5 Td (N) Tj ET".to_vec(),
    );
    let x2 = b.doc.add_object(x2_stream);
    let mut x1_xobjects = Dictionary::new();
    x1_xobjects.set("X2", x2);
    let mut x1_fonts = Dictionary::new();
    x1_fonts.set("F1", font);
    let mut x1_res = Dictionary::new();
    x1_res.set("XObject", x1_xobjects);
    x1_res.set("Font", x1_fonts);
    let x1_stream = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 100.into(), 200.into()],
            "Resources" => x1_res,
        },
        b"BT /F1 10 Tf 0 0 Td (O) Tj ET /X2 Do".to_vec(),
    );
    let x1 = b.doc.add_object(x1_stream);
    let mut xobjects = Dictionary::new();
    xobjects.set("X1", x1);
    let mut resources = Dictionary::new();
    resources.set("XObject", xobjects);
    b.add_page(resources, "/X1 Do", None);
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "ON", "{:?}", ex.diagnostics);
    assert_eq!(ex.glyphs[0].source.kind, SourceKind::FormXObject);
    assert_eq!(ex.glyphs[1].source.kind, SourceKind::FormXObject);
    assert_close(ex.glyphs[0].matrix[4], 100.0);
    assert_close(ex.glyphs[0].matrix[5], 200.0);
    assert_close(ex.glyphs[1].matrix[4], 115.0);
    assert_close(ex.glyphs[1].matrix[5], 225.0);
    let report = ex.coverage_report();
    assert_eq!(report.total, 2);
    assert_eq!(report.unresolved_ids.len(), 2);
}

#[test]
fn annotation_appearance_text() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 65, 90, 1000);
    let mut fonts = Dictionary::new();
    fonts.set("F1", font);
    let mut resources = Dictionary::new();
    resources.set("Font", fonts);
    let appearance = b.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 50.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
            "Resources" => resources,
        },
        b"BT /F1 10 Tf 10 10 Td (Z) Tj ET".to_vec(),
    ));
    let mut ap = Dictionary::new();
    ap.set("N", appearance);
    let annot = b.doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Stamp",
        "Rect" => vec![100.into(), 200.into(), 200.into(), 250.into()],
        "AP" => ap,
    });
    let annots = b.doc.add_object(vec![Object::from(annot)]);
    // Page content is intentionally empty of text; the glyph lives in /AP.
    let empty = Dictionary::new();
    b.add_page(empty, "", Some(annots));
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "Z", "{:?}", ex.diagnostics);
    assert_eq!(ex.glyphs[0].source.kind, SourceKind::AnnotationAppearance);
    assert_close(ex.glyphs[0].matrix[4], 110.0);
    assert_close(ex.glyphs[0].matrix[5], 210.0);
}

#[test]
fn type3_charproc_text_is_kept() {
    let mut b = Builder::new();
    let inner = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 65, 122, 600);
    let mut fonts = Dictionary::new();
    fonts.set("F1", inner);
    let mut resources = Dictionary::new();
    resources.set("Font", fonts);
    let proc = b.doc.add_object(Stream::new(
        Dictionary::new(),
        b"600 0 d0 BT /F1 10 Tf 0 0 Td (Hi) Tj ET".to_vec(),
    ));
    let mut procs = Dictionary::new();
    procs.set("A", proc);
    let mut enc = Dictionary::new();
    enc.set("Type", "Encoding");
    enc.set("BaseEncoding", "WinAnsiEncoding");
    enc.set("Differences", vec![Object::Integer(65), Object::from("A")]);
    let type3 = b.doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
        "FontBBox" => vec![0.into(), 0.into(), 1000.into(), 1000.into()],
        "Encoding" => enc,
        "FirstChar" => 65,
        "LastChar" => 65,
        "Widths" => vec![Object::Integer(600)],
        "CharProcs" => procs,
        "Resources" => resources,
    });
    let page_resources = b.font_dict(type3);
    b.add_page(page_resources, "BT /F1 12 Tf 50 50 Td (A) Tj ET", None);
    let ex = extract(&b.bytes());
    assert!(
        ex.glyphs
            .iter()
            .any(|g| g.unicode == "A" && g.source.kind == SourceKind::PageContent),
        "type3 glyph missing: {ex:?}"
    );
    let inner: Vec<_> = ex
        .glyphs
        .iter()
        .filter(|g| g.source.kind == SourceKind::Type3CharProc)
        .map(|g| g.unicode.as_str())
        .collect();
    assert_eq!(
        inner,
        vec!["H", "i"],
        "charproc text dropped: {:?}",
        ex.diagnostics
    );
    let parent = ex.glyphs.iter().find(|g| g.unicode == "A").unwrap();
    assert_close(parent.matrix[4], 50.0);
    assert_close(parent.matrix[5], 50.0);
    assert_close(parent.advance[0], 7.2);
}

#[test]
fn invisible_render_mode_is_recorded() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 32, 122, 500);
    let resources = b.font_dict(font);
    b.add_page(
        resources,
        "BT /F1 12 Tf 10 10 Td (See) Tj 3 Tr (Hide) Tj 0 Tr (Me) Tj ET",
        None,
    );
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "SeeHideMe", "{:?}", ex.diagnostics);
    assert!(!ex.glyphs[0].invisible && !ex.glyphs[1].invisible && !ex.glyphs[2].invisible);
    assert!(
        ex.glyphs[3].invisible
            && ex.glyphs[4].invisible
            && ex.glyphs[5].invisible
            && ex.glyphs[6].invisible
    );
    assert_eq!(ex.glyphs[3].render_mode, 3);
    assert!(!ex.glyphs[7].invisible && !ex.glyphs[8].invisible);
    // Hidden glyphs still advance.
    assert!(ex.glyphs[7].matrix[4] > ex.glyphs[2].matrix[4]);
}

#[test]
fn vertical_writing_advances_on_y() {
    let mut b = Builder::new();
    let tounicode = b"1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
        2 beginbfchar\n<0001> <0041>\n<0002> <0042>\nendbfchar\n";
    let to_id = b
        .doc
        .add_object(Stream::new(Dictionary::new(), tounicode.to_vec()));
    let mut cid_dict = dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "Vert",
        "DW" => 1000,
    };
    cid_dict.set("DW2", vec![Object::Integer(1000), Object::Integer(-500)]);
    let cid = b.doc.add_object(cid_dict);
    let font = b.doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "Vert",
        "Encoding" => "Identity-V",
        "DescendantFonts" => vec![Object::from(cid)],
        "ToUnicode" => to_id,
    });
    let resources = b.font_dict(font);
    b.add_page(resources, "BT /F1 10 Tf 100 500 Td <00010002> Tj ET", None);
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "AB", "{:?}", ex.diagnostics);
    assert!(ex.glyphs.iter().all(|g| g.vertical));
    assert_close(ex.glyphs[0].matrix[5], 495.0);
    assert_close(ex.glyphs[1].matrix[5], 505.0);
    assert_close(ex.glyphs[0].advance[1], 10.0);
    assert_close(ex.glyphs[0].advance[0], 0.0);
}

#[test]
fn rectangular_clip_flags_glyphs_outside() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 65, 122, 600);
    let resources = b.font_dict(font);
    let content = "\
q
100 100 50 50 re W n
BT /F1 12 Tf 10 10 Td (No) Tj ET
BT /F1 12 Tf 110 110 Td (Yes) Tj ET
Q
";
    b.add_page(resources, content, None);
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "NoYes");
    assert!(
        ex.glyphs[0].clipped && ex.glyphs[1].clipped,
        "outside glyphs not flagged"
    );
    assert!(
        ex.glyphs[2..].iter().all(|g| !g.clipped),
        "inside glyphs flagged {:?}",
        ex.glyphs
    );
}

#[test]
fn inline_image_and_bad_operator_do_not_drop_neighbors() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 32, 122, 600);
    let resources = b.font_dict(font);
    let content = "\
BT /F1 12 Tf 72 700 Td (Before) Tj ET
BI /W 2 /H 2 /CS /G /BPC 8 ID wxyz
EI
123 456 789 bogus
BT /F1 12 Tf 72 680 Td (After) Tj ET
";
    b.add_page(resources, content, None);
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "BeforeAfter", "{:?}", ex.diagnostics);
    assert!(ex
        .diagnostics
        .iter()
        .any(|d| d.message.contains("bogus") || d.message.contains("unknown")));
    assert_close(ex.glyphs[0].matrix[5], 700.0);
    assert_close(ex.glyphs[6].matrix[5], 680.0);
}

#[test]
fn object_streams_round_trip() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 32, 122, 600);
    let resources = b.font_dict(font);
    b.add_page(resources, "BT /F1 12 Tf 1 2 Td (Streams) Tj ET", None);
    let bytes = b.bytes_modern();
    assert!(bytes.windows(6).any(|w| w == b"ObjStm") || bytes.windows(7).any(|w| w == b"/ObjStm"));
    let ex = extract(&bytes);
    assert_eq!(joined(&ex), "Streams", "{:?}", ex.diagnostics);
}

#[test]
fn broken_xref_does_not_panic() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 32, 122, 600);
    let resources = b.font_dict(font);
    b.add_page(resources, "BT /F1 12 Tf 0 0 Td (Hello) Tj ET", None);
    let mut bytes = b.bytes();
    if let Some(pos) = bytes.windows(9).position(|w| w == b"startxref") {
        let num_start = pos + 9;
        // Overwrite the xref offset with a value past EOF.
        if let Some(rel_end) = bytes[num_start..]
            .iter()
            .position(|b| *b == b'%' || *b == b'\n')
        {
            for byte in &mut bytes[num_start..num_start + rel_end] {
                if byte.is_ascii_digit() {
                    *byte = b'9';
                }
            }
        }
    }
    match PdfDocument::open_bytes(&bytes) {
        Ok(doc) => {
            let ex = doc.extract();
            assert_eq!(
                joined(&ex),
                "Hello",
                "recovered file dropped text: {:?}",
                ex.diagnostics
            );
        }
        Err(err) => {
            let _ = err.to_string();
        }
    }
    assert!(PdfDocument::open_bytes(&[]).is_err());
}

#[test]
fn coverage_requires_every_extracted_glyph() {
    let mut b = Builder::new();
    let font = type1(&mut b.doc, "Type1", "WinAnsiEncoding".into(), 65, 90, 600);
    let resources = b.font_dict(font);
    b.add_page(resources, "BT /F1 12 Tf 0 0 Td (ABC) Tj ET", None);
    let mut ex = extract(&b.bytes());
    assert!(ex.assert_complete().is_err());
    let ids: Vec<u32> = ex.glyphs.iter().map(|g| g.id).collect();
    assert_eq!(ids.len(), 3);
    ex.mark_rewritten(ids[0], "甲").unwrap();
    ex.mark_kept(ids[1], "proper noun").unwrap();
    assert!(
        ex.assert_complete().is_err(),
        "a glyph was lost from the report"
    );
    ex.mark_non_text(ids[2], "decoration").unwrap();
    assert!(ex.assert_complete().is_ok());
    let report = ex.coverage_report();
    assert!(report.complete);
    assert_eq!(report.rewritten, 1);
    assert_eq!(report.kept_original, 1);
    assert_eq!(report.non_text, 1);
    assert!(ex.mark_rewritten(ids[0], "again").is_err());
}

#[test]
fn macroman_byte_maps_through_base_encoding() {
    let mut b = Builder::new();
    let font = type1(
        &mut b.doc,
        "Type1",
        "MacRomanEncoding".into(),
        128,
        128,
        600,
    );
    let resources = b.font_dict(font);
    // MacRoman 0x80 is Ä.
    b.add_page(resources, "BT /F1 12 Tf 0 0 Td <80> Tj ET", None);
    let ex = extract(&b.bytes());
    assert_eq!(joined(&ex), "\u{00C4}", "{:?}", ex.diagnostics);
}
