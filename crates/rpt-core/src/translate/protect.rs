//! Shield spans the model must copy verbatim, then restore them after translation.
//!
//! Placeholders use ⟦N⟧ (U+27E6 / U+27E7). Glossary hits are replaced locally so
//! the model never has to obey a glossary table: the target term is what gets
//! restored.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shielded {
    pub text: String,
    pub slots: Vec<String>,
}

pub fn shield(input: &str, glossary: &[(String, String)]) -> Shielded {
    let glossary = sorted_glossary(glossary);
    let mut slots = Vec::new();
    let mut out = String::new();
    let mut i = 0;
    while i < input.len() {
        if let Some((end, replacement)) = match_one(input, i, &glossary) {
            push_placeholder(&mut out, slots.len());
            slots.push(replacement);
            i = end;
            continue;
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    Shielded { text: out, slots }
}

pub fn restore(text: &str, slots: &[String]) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    let mut seen = vec![false; slots.len()];
    while let Some(rel) = rest.find('⟦') {
        out.push_str(&rest[..rel]);
        let after = &rest[rel + '⟦'.len_utf8()..];
        let Some(end_rel) = after.find('⟧') else {
            out.push('⟦');
            rest = after;
            continue;
        };
        let token = &after[..end_rel];
        if let Ok(n) = token.parse::<usize>() {
            if n >= slots.len() {
                return Err(format!("unknown placeholder ⟦{n}⟧"));
            }
            seen[n] = true;
            out.push_str(&slots[n]);
            rest = &after[end_rel + '⟧'.len_utf8()..];
        } else {
            out.push('⟦');
            rest = after;
        }
    }
    out.push_str(rest);
    if let Some(i) = seen.iter().position(|used| !used) {
        return Err(format!("missing placeholder ⟦{i}⟧"));
    }
    Ok(out)
}

fn push_placeholder(out: &mut String, n: usize) {
    out.push('⟦');
    out.push_str(&n.to_string());
    out.push('⟧');
}

fn sorted_glossary(glossary: &[(String, String)]) -> Vec<(String, String)> {
    let mut terms: Vec<(String, String)> = glossary
        .iter()
        .filter(|(src, _)| !src.is_empty())
        .cloned()
        .collect();
    terms.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
    terms
}

fn match_one(input: &str, i: usize, glossary: &[(String, String)]) -> Option<(usize, String)> {
    if let Some(end) = match_existing_placeholder(input, i) {
        return Some((end, input[i..end].to_string()));
    }
    if let Some(end) = match_url(input, i) {
        return Some((end, input[i..end].to_string()));
    }
    if let Some(end) = match_email(input, i) {
        return Some((end, input[i..end].to_string()));
    }
    if let Some(end) = match_braces(input, i) {
        return Some((end, input[i..end].to_string()));
    }
    if let Some(end) = match_number(input, i) {
        return Some((end, input[i..end].to_string()));
    }
    match_glossary(input, i, glossary)
}

fn match_existing_placeholder(input: &str, i: usize) -> Option<usize> {
    let rest = input.get(i..)?;
    if !rest.starts_with('⟦') {
        return None;
    }
    let after = &rest['⟦'.len_utf8()..];
    let end_rel = after.find('⟧')?;
    if end_rel == 0 || !after[..end_rel].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(i + '⟦'.len_utf8() + end_rel + '⟧'.len_utf8())
}

fn match_url(input: &str, i: usize) -> Option<usize> {
    let rest = input.get(i..)?;
    let prefix = if rest.starts_with("https://") {
        8
    } else if rest.starts_with("http://") {
        7
    } else {
        return None;
    };
    let bytes = rest.as_bytes();
    let mut end = prefix;
    while end < bytes.len()
        && !bytes[end].is_ascii_whitespace()
        && bytes[end] != b'<'
        && bytes[end] != b'>'
        && bytes[end] != b'"'
    {
        end += 1;
    }
    while end > prefix && matches!(bytes[end - 1], b'.' | b',' | b';' | b':' | b')' | b']') {
        end -= 1;
    }
    (end > prefix).then_some(i + end)
}

fn match_email(input: &str, i: usize) -> Option<usize> {
    let rest = input.get(i..)?;
    let bytes = rest.as_bytes();
    let mut p = 0;
    while p < bytes.len() && is_email_local(bytes[p]) {
        p += 1;
    }
    if p == 0 || p >= bytes.len() || bytes[p] != b'@' {
        return None;
    }
    p += 1;
    let domain_start = p;
    while p < bytes.len() && is_email_domain(bytes[p]) {
        p += 1;
    }
    let domain = &rest[domain_start..p];
    let dot = domain.rfind('.')?;
    let tld = &domain[dot + 1..];
    if tld.len() < 2 || !tld.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    if !left_boundary(input, i) {
        return None;
    }
    Some(i + p)
}

fn match_braces(input: &str, i: usize) -> Option<usize> {
    let rest = input.get(i..)?;
    if !rest.starts_with('{') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut p = 1;
    while p < bytes.len() && bytes[p] != b'}' && bytes[p] != b'{' && bytes[p] != b'\n' {
        p += 1;
    }
    if p < bytes.len() && bytes[p] == b'}' && p > 1 {
        Some(i + p + 1)
    } else {
        None
    }
}

fn match_number(input: &str, i: usize) -> Option<usize> {
    let rest = input.get(i..)?;
    let bytes = rest.as_bytes();
    if bytes.first().is_none_or(|b| !b.is_ascii_digit()) {
        return None;
    }
    if !left_boundary(input, i) {
        return None;
    }
    let mut p = 0;
    while p < bytes.len() && bytes[p].is_ascii_digit() {
        p += 1;
    }
    if p < bytes.len() && bytes[p] == b'.' {
        let mut q = p + 1;
        let frac = q;
        while q < bytes.len() && bytes[q].is_ascii_digit() {
            q += 1;
        }
        if q > frac {
            p = q;
        }
    }
    if !right_boundary(input, i + p) {
        return None;
    }
    Some(i + p)
}

fn match_glossary(input: &str, i: usize, glossary: &[(String, String)]) -> Option<(usize, String)> {
    let rest = input.get(i..)?;
    for (src, target) in glossary {
        if rest.starts_with(src.as_str()) && term_boundary(input, i, src) {
            return Some((i + src.len(), target.clone()));
        }
    }
    None
}

fn term_boundary(input: &str, i: usize, term: &str) -> bool {
    let ascii_left = term
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let ascii_right = term
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    (!ascii_left || left_boundary(input, i))
        && (!ascii_right || right_boundary(input, i + term.len()))
}

fn left_boundary(input: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    input[..i]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn right_boundary(input: &str, i: usize) -> bool {
    if i >= input.len() {
        return true;
    }
    input[i..]
        .chars()
        .next()
        .is_some_and(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn is_email_local(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shields_url_email_braces_number_and_restores() {
        let src = "Rate 0.001, see https://example.com/paper, mail a@b.co, and {eq:1}.";
        let shielded = shield(src, &[]);
        assert!(!shielded.text.contains("0.001"));
        assert!(!shielded.text.contains("https://"));
        assert!(!shielded.text.contains("a@b.co"));
        assert!(!shielded.text.contains("{eq:1}"));
        assert!(shielded.text.contains('⟦'));
        assert_eq!(restore(&shielded.text, &shielded.slots).unwrap(), src);
    }

    #[test]
    fn glossary_longest_match_and_word_boundary() {
        let glossary = vec![
            ("attention".into(), "ATT".into()),
            ("self-attention".into(), "SELF-ATT".into()),
            ("transformer".into(), "Transformer".into()),
        ];
        let shielded = shield(
            "The transformer uses self-attention, not transformers.",
            &glossary,
        );
        assert_eq!(
            restore(&shielded.text, &shielded.slots).unwrap(),
            "The Transformer uses SELF-ATT, not transformers."
        );
        assert!(shielded.text.contains("transformers"));
        assert!(!shielded.text.contains("self-attention"));
        assert!(shielded.slots.contains(&"Transformer".to_string()));
        assert!(shielded.slots.contains(&"SELF-ATT".to_string()));
        assert!(!shielded.slots.iter().any(|s| s == "ATT"));
    }

    #[test]
    fn missing_placeholder_is_an_error() {
        let shielded = shield("see https://example.com/x", &[]);
        let broken = shielded.text.replace('⟦', "");
        assert!(restore(&broken, &shielded.slots).is_err());
    }
}
