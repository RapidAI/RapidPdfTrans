use std::collections::HashMap;

use lopdf::{Document, Object};

use crate::pdfutil::{as_name, deref, dict_of};

#[derive(Clone, Debug, Default)]
pub struct Resources {
    pub fonts: HashMap<String, Object>,
    pub xobjects: HashMap<String, Object>,
    pub ext_gstates: HashMap<String, Object>,
}

impl Resources {
    /// Child entries replace parent entries with the same name.
    pub fn merge_override(&mut self, child: Resources) {
        self.fonts.extend(child.fonts);
        self.xobjects.extend(child.xobjects);
        self.ext_gstates.extend(child.ext_gstates);
    }

    pub fn from_dict(doc: &Document, dict: &lopdf::Dictionary) -> Self {
        Self {
            fonts: name_map(doc, dict, b"Font"),
            xobjects: name_map(doc, dict, b"XObject"),
            ext_gstates: name_map(doc, dict, b"ExtGState"),
        }
    }
}

fn name_map(doc: &Document, dict: &lopdf::Dictionary, key: &[u8]) -> HashMap<String, Object> {
    let mut out = HashMap::new();
    let Ok(obj) = dict.get(key) else {
        return out;
    };
    let Some(map) = dict_of(doc, obj) else {
        return out;
    };
    for (name, value) in map.iter() {
        let key = String::from_utf8_lossy(name).into_owned();
        // Keep the original object (often a reference) so the loader can see the id.
        let _ = as_name(deref(doc, value));
        out.insert(key, value.clone());
    }
    out
}
