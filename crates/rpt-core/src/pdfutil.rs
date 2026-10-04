use lopdf::{Dictionary, Document, Object, ObjectId};

const MAX_DEREF: usize = 16;

pub fn deref<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    let mut current = obj;
    for _ in 0..MAX_DEREF {
        match current {
            Object::Reference(id) => match doc.get_object(*id) {
                Ok(next) => current = next,
                Err(_) => break,
            },
            _ => break,
        }
    }
    current
}

pub fn as_f32(obj: &Object) -> Option<f32> {
    match obj {
        Object::Integer(v) => Some(*v as f32),
        Object::Real(v) => Some(*v),
        _ => None,
    }
}

pub fn as_name(obj: &Object) -> Option<String> {
    obj.as_name()
        .ok()
        .map(|b| String::from_utf8_lossy(b).into_owned())
}

pub fn dict_of<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    deref(doc, obj).as_dict().ok()
}

pub fn stream_bytes(
    doc: &Document,
    obj: &Object,
    limit: usize,
) -> Option<(Option<ObjectId>, Vec<u8>, Dictionary)> {
    let mut id = None;
    let mut current = obj;
    for _ in 0..MAX_DEREF {
        match current {
            Object::Reference(oid) => {
                id = Some(*oid);
                current = doc.get_object(*oid).ok()?;
            }
            Object::Stream(stream) => {
                let bytes = stream
                    .decompressed_content_with_limit(limit)
                    .unwrap_or_else(|_| stream.content.clone());
                return Some((id, bytes, stream.dict.clone()));
            }
            _ => return None,
        }
    }
    None
}

pub fn object_id_string(id: ObjectId) -> String {
    format!("{} {}", id.0, id.1)
}

pub fn array_of<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Vec<Object>> {
    deref(doc, obj).as_array().ok()
}
