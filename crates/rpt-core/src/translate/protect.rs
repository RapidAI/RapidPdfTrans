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
    if let Some(end) = match_citation(input, i) {
        return Some((end, input[i..end].to_string()));
    }
    if let Some(end) = match_number(input, i) {
        return Some((end, input[i..end].to_string()));
    }
    match_glossary(input, i, glossary)
}

/// End byte of an in-text citation starting at `i`, if any.
/// Numeric `[12]` / `[1-3]` and author-year `(Smith et al., 2020)` / `Smith (2020)`.
pub fn citation_end(text: &str, i: usize) -> Option<usize> {
    match_citation(text, i)
}

fn match_citation(input: &str, i: usize) -> Option<usize> {
    let rest = input.get(i..)?;
    if let Some(end) = match_numeric_brackets(rest) {
        return Some(i + end);
    }
    if let Some(end) = match_wrapped_citation(rest) {
        return Some(i + end);
    }
    match_narrative_citation(input, i)
}

fn match_numeric_brackets(rest: &str) -> Option<usize> {
    let (open_len, close) = if rest.starts_with('[') {
        (1, "]")
    } else if rest.starts_with('［') {
        ('［'.len_utf8(), "］")
    } else {
        return None;
    };
    let bytes = rest.as_bytes();
    let mut p = open_len;
    if !consume_citation_number(bytes, &mut p) {
        return None;
    }
    loop {
        let saved = p;
        p = skip_ascii_space(bytes, p);
        if rest[p..].starts_with(close) {
            break;
        }
        let sep = if p < bytes.len() && matches!(bytes[p], b',' | b';' | b'-') {
            1
        } else if rest[p..].starts_with('–') || rest[p..].starts_with('—') {
            '–'.len_utf8()
        } else {
            p = saved;
            break;
        };
        p += sep;
        p = skip_ascii_space(bytes, p);
        if !consume_citation_number(bytes, &mut p) {
            p = saved;
            break;
        }
    }
    p = skip_ascii_space(bytes, p);
    rest[p..].starts_with(close).then_some(p + close.len())
}

fn consume_citation_number(bytes: &[u8], p: &mut usize) -> bool {
    let start = *p;
    while *p < bytes.len() && bytes[*p].is_ascii_digit() {
        *p += 1;
    }
    let n = *p - start;
    (1..=4).contains(&n)
}

fn skip_ascii_space(bytes: &[u8], mut p: usize) -> usize {
    while p < bytes.len() && bytes[p] == b' ' {
        p += 1;
    }
    p
}

fn match_wrapped_citation(rest: &str) -> Option<usize> {
    let (open, close) = if rest.starts_with('(') {
        ('(', ')')
    } else if rest.starts_with('[') {
        ('[', ']')
    } else if rest.starts_with('［') {
        ('［', '］')
    } else {
        return None;
    };
    if open == '[' || open == '［' {
        let after = &rest[open.len_utf8()..];
        if after.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
            return None;
        }
    }
    let mut depth = 1usize;
    let mut i = open.len_utf8();
    while i < rest.len() && i < 220 {
        let ch = rest[i..].chars().next()?;
        if ch == '\n' {
            return None;
        }
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                let body = &rest[open.len_utf8()..i];
                if is_citation_body(body) {
                    return Some(i + ch.len_utf8());
                }
                return None;
            }
        }
        i += ch.len_utf8();
    }
    None
}

fn match_narrative_citation(input: &str, i: usize) -> Option<usize> {
    if !left_boundary(input, i) {
        return None;
    }
    let rest = input.get(i..)?;
    let (name, mut p) = take_name(rest)?;
    if is_stop_surname(name) || name.eq_ignore_ascii_case("the") {
        return None;
    }
    let saved = p;
    p = skip_space_str(rest, p);
    if rest[p..].starts_with("et al.") {
        p += "et al.".len();
    } else if rest[p..].starts_with("et al") {
        p += "et al".len();
    } else if let Some(q) = take_and_name(rest, p) {
        p = q;
    } else {
        p = saved;
    }
    p = skip_space_str(rest, p);
    if !rest[p..].starts_with('(') {
        return None;
    }
    let inside = &rest[p + 1..];
    let year_len = year_token_len(inside)?;
    let mut q = year_len;
    q = skip_space_str(inside, q);
    if inside[q..].starts_with(',') {
        let tail = skip_space_str(inside, q + 1);
        if inside[tail..].starts_with("p.") || inside[tail..].starts_with("pp.") {
            let mut t = tail;
            while t < inside.len() && !inside[t..].starts_with(')') {
                let ch = inside[t..].chars().next()?;
                if ch == '\n' || ch == '(' {
                    return None;
                }
                t += ch.len_utf8();
            }
            q = t;
        }
    }
    if !inside[q..].starts_with(')') {
        return None;
    }
    Some(i + p + 1 + q + 1)
}

fn take_and_name(rest: &str, p: usize) -> Option<usize> {
    let p = skip_space_str(rest, p);
    let p = if rest[p..].starts_with("and ") {
        p + 4
    } else if rest[p..].starts_with("& ") {
        p + 2
    } else {
        return None;
    };
    let (_, len) = take_name(&rest[p..])?;
    Some(p + len)
}

fn take_name(text: &str) -> Option<(&str, usize)> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if !first.is_uppercase() {
        return None;
    }
    let mut len = first.len_utf8();
    for ch in chars {
        if ch.is_alphabetic() || matches!(ch, '\'' | '’' | '-' | '‐') {
            len += ch.len_utf8();
        } else {
            break;
        }
    }
    if len == first.len_utf8() {
        return None;
    }
    Some((&text[..len], len))
}

fn is_stop_surname(name: &str) -> bool {
    matches!(
        name,
        "Figure"
            | "Fig"
            | "Table"
            | "Tab"
            | "Section"
            | "Equation"
            | "Eq"
            | "Chapter"
            | "Appendix"
            | "Algorithm"
            | "Theorem"
            | "Lemma"
            | "Page"
            | "Volume"
            | "Vol"
            | "The"
            | "This"
            | "That"
            | "For"
            | "With"
            | "From"
            | "Using"
            | "See"
    )
}

fn skip_space_str(text: &str, mut p: usize) -> usize {
    while text[p..].starts_with(' ') {
        p += 1;
    }
    p
}

fn year_token_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.len() < 4 || !bytes[..4].iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: u32 = text[..4].parse().ok()?;
    if !(1900..2100).contains(&year) {
        return None;
    }
    if bytes.len() > 4 && bytes[4].is_ascii_digit() {
        return None;
    }
    let mut n = 4;
    if bytes.get(n).is_some_and(|b| b.is_ascii_alphabetic()) {
        n += 1;
    }
    Some(n)
}

fn is_citation_body(body: &str) -> bool {
    let body = body.trim();
    if body.is_empty() || body.chars().count() > 180 || body.contains('\n') {
        return false;
    }
    let parts = split_semicolons(body);
    !parts.is_empty() && parts.iter().all(|part| citation_chunk(part.trim()))
}

fn split_semicolons(body: &str) -> Vec<&str> {
    body.split(';')
        .filter(|part| !part.trim().is_empty())
        .collect()
}

fn citation_chunk(part: &str) -> bool {
    let part = strip_citation_prefix(part.trim());
    let Some(year_at) = find_year_in(part) else {
        return false;
    };
    let prefix = part[..year_at].trim().trim_end_matches([',', '，']).trim();
    if prefix.is_empty() || has_disallowed_word(prefix) {
        return false;
    }
    has_surname(prefix)
}

fn strip_citation_prefix(mut part: &str) -> &str {
    loop {
        let lower = part.to_ascii_lowercase();
        let next = if lower.starts_with("e.g. ") {
            Some(&part["e.g. ".len()..])
        } else if lower.starts_with("e.g ") {
            Some(&part["e.g ".len()..])
        } else if lower.starts_with("cf. ") {
            Some(&part["cf. ".len()..])
        } else if lower.starts_with("cf ") {
            Some(&part["cf ".len()..])
        } else if lower.starts_with("see ") {
            Some(&part["see ".len()..])
        } else if lower.starts_with("ibid. ") {
            Some(&part["ibid. ".len()..])
        } else if lower.starts_with("ibid ") {
            Some(&part["ibid ".len()..])
        } else {
            None
        };
        match next {
            Some(rest) => part = rest.trim_start(),
            None => return part,
        }
    }
}

fn find_year_in(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_digit()) {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j - i == 4 {
                if let Ok(year) = text[i..j].parse::<u32>() {
                    if (1900..2100).contains(&year) {
                        return Some(i);
                    }
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    None
}

fn has_disallowed_word(prefix: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "and", "with", "from", "und", "von", "van", "de", "del", "der", "di", "la", "le", "da",
        "dos", "das", "et", "al", "al.",
    ];
    prefix.split_whitespace().any(|word| {
        let bare = word.trim_matches(|ch: char| matches!(ch, ',' | '.' | ':' | ';' | '(' | ')'));
        bare.len() >= 4
            && bare.chars().all(|ch| ch.is_ascii_lowercase())
            && !ALLOWED.contains(&bare)
    })
}

fn has_surname(prefix: &str) -> bool {
    prefix.split_whitespace().any(|word| {
        let bare = word.trim_matches(|ch: char| matches!(ch, ',' | '.' | ':' | ';'));
        let Some((name, len)) = take_name(bare) else {
            return false;
        };
        len == bare.len() && !is_stop_surname(name)
    })
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
    fn shields_numeric_and_author_year_citations() {
        let src = "See [12], [1, 3-5], and [1–3] plus (Smith et al., 2020) and Smith (2019).";
        let shielded = shield(src, &[]);
        assert!(!shielded.text.contains("[12]"), "{}", shielded.text);
        assert!(!shielded.text.contains("Smith"), "{}", shielded.text);
        assert!(!shielded.text.contains("2019"), "{}", shielded.text);
        assert!(!shielded.text.contains("2020"), "{}", shielded.text);
        assert_eq!(restore(&shielded.text, &shielded.slots).unwrap(), src);

        let multi = "(Smith, 2020; Jones et al., 2021)";
        let shielded = shield(multi, &[]);
        assert!(!shielded.text.contains("Smith"), "{}", shielded.text);
        assert_eq!(restore(&shielded.text, &shielded.slots).unwrap(), multi);

        let prose = "established in 2020 and (the paper from 2020)";
        let shielded = shield(prose, &[]);
        assert!(shielded.text.contains("established"), "{}", shielded.text);
        assert!(shielded.text.contains("paper"), "{}", shielded.text);
        assert!(!shielded.text.contains("2020"), "{}", shielded.text);
        assert_eq!(restore(&shielded.text, &shielded.slots).unwrap(), prose);
    }

    #[test]
    fn missing_placeholder_is_an_error() {
        let shielded = shield("see https://example.com/x", &[]);
        let broken = shielded.text.replace('⟦', "");
        assert!(restore(&broken, &shielded.slots).is_err());
    }
}
