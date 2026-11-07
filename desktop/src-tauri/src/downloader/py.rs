//! Small helpers that behave exactly like the Python built-ins the original code used, so the
//! ported logic gives the same results (file names, numbers, text).

use unicode_general_category::{get_general_category, GeneralCategory as G};

/// os.path.splitext on Windows (ntpath): the extension starts at the last dot of the file name,
/// unless everything before that dot is dots (".hidden" has no extension).
pub fn splitext(p: &str) -> (&str, &str) {
    let sep = p.rfind(['/', '\\']).map(|i| i as isize).unwrap_or(-1);
    if let Some(dot) = p.rfind('.') {
        if dot as isize > sep {
            let start = (sep + 1) as usize;
            if p[start..dot].chars().any(|c| c != '.') {
                return (&p[..dot], &p[dot..]);
            }
        }
    }
    (p, "")
}

/// os.path.basename on Windows (both separators)
pub fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

/// str.strip(chars)
pub fn strip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_matches(|c| chars.contains(c))
}

/// str.rstrip(chars)
pub fn rstrip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_end_matches(|c| chars.contains(c))
}

/// The first `n` characters (Python slicing counts characters, not bytes)
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// str.casefold() (full case folding for the letters where it differs from lower())
pub fn casefold(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'ß' | 'ẞ' => "ss".chars().collect::<Vec<_>>(),
            'ﬁ' => "fi".chars().collect(),
            'ﬂ' => "fl".chars().collect(),
            'ς' => vec!['σ'],
            _ => c.to_lowercase().collect(),
        })
        .collect()
}

/// unicodedata.category(ch) as Python's two-letter code
pub fn category(c: char) -> &'static str {
    match get_general_category(c) {
        G::UppercaseLetter => "Lu",
        G::LowercaseLetter => "Ll",
        G::TitlecaseLetter => "Lt",
        G::ModifierLetter => "Lm",
        G::OtherLetter => "Lo",
        G::NonspacingMark => "Mn",
        G::SpacingMark => "Mc",
        G::EnclosingMark => "Me",
        G::DecimalNumber => "Nd",
        G::LetterNumber => "Nl",
        G::OtherNumber => "No",
        G::ConnectorPunctuation => "Pc",
        G::DashPunctuation => "Pd",
        G::OpenPunctuation => "Ps",
        G::ClosePunctuation => "Pe",
        G::InitialPunctuation => "Pi",
        G::FinalPunctuation => "Pf",
        G::OtherPunctuation => "Po",
        G::MathSymbol => "Sm",
        G::CurrencySymbol => "Sc",
        G::ModifierSymbol => "Sk",
        G::OtherSymbol => "So",
        G::SpaceSeparator => "Zs",
        G::LineSeparator => "Zl",
        G::ParagraphSeparator => "Zp",
        G::Control => "Cc",
        G::Format => "Cf",
        G::Surrogate => "Cs",
        G::PrivateUse => "Co",
        G::Unassigned => "Cn",
        _ => "Cn",
    }
}

/// format(x, "g"): 6 significant digits, trailing zeros dropped, scientific outside 1e-4..1e6
pub fn fmt_g(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    if !x.is_finite() {
        return if x.is_nan() { "nan".into() } else if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sci = format!("{x:.5e}");
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let trim = |s: String| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if (-4..6).contains(&exp) {
        trim(format!("{:.*}", (5 - exp).max(0) as usize, x))
    } else {
        let m = trim(mant.to_string());
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// float(s) for the plain decimal strings the filters read ("1.5", ".5", "1.", " 7 ")
pub fn float(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.contains('_') {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite() || t.eq_ignore_ascii_case("inf"))
}

/// int(s) for decimal strings
pub fn int(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok()
}

/// Entity names HTML5 also accepts without the closing ";" (html.entities.html5)
const LEGACY_ENTITIES: &[&str] = &[
    "AElig", "AMP", "Aacute", "Acirc", "Agrave", "Aring", "Atilde", "Auml", "COPY", "Ccedil", "ETH", "Eacute", "Ecirc", "Egrave",
    "Euml", "GT", "Iacute", "Icirc", "Igrave", "Iuml", "LT", "Ntilde", "Oacute", "Ocirc", "Ograve", "Oslash", "Otilde", "Ouml",
    "QUOT", "REG", "THORN", "Uacute", "Ucirc", "Ugrave", "Uuml", "Yacute", "aacute", "acirc", "acute", "aelig", "agrave", "amp",
    "aring", "atilde", "auml", "brvbar", "ccedil", "cedil", "cent", "copy", "curren", "deg", "divide", "eacute", "ecirc", "egrave",
    "eth", "euml", "frac12", "frac14", "frac34", "gt", "iacute", "icirc", "iexcl", "igrave", "iquest", "iuml", "laquo", "lt", "macr",
    "micro", "middot", "nbsp", "not", "ntilde", "oacute", "ocirc", "ograve", "ordf", "ordm", "oslash", "otilde", "ouml", "para",
    "plusmn", "pound", "quot", "raquo", "reg", "sect", "shy", "sup1", "sup2", "sup3", "szlig", "thorn", "times", "uacute", "ucirc",
    "ugrave", "uml", "uuml", "yacute", "yen", "yuml",
];

/// The text a named entity ("amp;", or an old name without ";" like "copy") stands for
fn named_entity(name: &str) -> Option<String> {
    let full = match name.strip_suffix(';') {
        Some(n) => n,
        None if LEGACY_ENTITIES.contains(&name) => name,
        None => return None,
    };
    let probe = format!("&{full};");
    let out = html_escape::decode_html_entities(&probe);
    (out != probe).then(|| out.into_owned())
}

/// A numeric character reference, the way html.unescape treats odd numbers
fn char_ref(num: Option<u32>) -> String {
    const CP1252: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{0192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{02c6}', '\u{2030}', '\u{0160}',
        '\u{2039}', '\u{0152}', '\u{8d}', '\u{017d}', '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}',
        '\u{2013}', '\u{2014}', '\u{02dc}', '\u{2122}', '\u{0161}', '\u{203a}', '\u{0153}', '\u{9d}', '\u{017e}', '\u{0178}',
    ];
    let Some(n) = num.filter(|n| *n <= 0x10FFFF && !(0xD800..=0xDFFF).contains(n)) else { return "\u{fffd}".into() };
    match n {
        0 => "\u{fffd}".into(),
        0x0d => "\r".into(),
        0x80..=0x9f => CP1252[(n - 0x80) as usize].to_string(),
        0x1..=0x8 | 0xb | 0xe..=0x1f | 0x7f | 0xfdd0..=0xfdef => String::new(),
        n if n & 0xFFFE == 0xFFFE => String::new(),
        n => char::from_u32(n).map(String::from).unwrap_or_default(),
    }
}

/// html.unescape, exactly (numeric references, HTML5 names, old names without ";")
pub fn html_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| regex::Regex::new(r"&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\x0c <&#;]{1,32};?)").unwrap());
    r.replace_all(s, |c: &regex::Captures| {
        let m = &c[1];
        if let Some(num) = m.strip_prefix('#') {
            let num = num.trim_end_matches(';');
            let v = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => num.parse::<u32>().ok(),
            };
            return char_ref(v);
        }
        if let Some(v) = named_entity(m) {
            return v;
        }
        let cuts: Vec<usize> = m.char_indices().map(|(i, _)| i).collect();
        for x in (2..cuts.len()).rev() {
            if let Some(v) = named_entity(&m[..cuts[x]]) {
                return format!("{v}{}", &m[cuts[x]..]);
            }
        }
        format!("&{m}")
    })
    .into_owned()
}

// ── os.path on Windows (ntpath) ─────────────────────────────────────────────

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// ntpath.splitroot: (drive, root, rest)
fn splitroot(p: &str) -> (&str, &str, &str) {
    let b = p.as_bytes();
    if b.len() >= 2 && is_sep(b[0] as char) && is_sep(b[1] as char) {
        // UNC: \server\share
        let rest = &p[2..];
        let i = rest.find(['\\', '/']).map(|i| i + 2).unwrap_or(p.len());
        let j = p[(i + 1).min(p.len())..].find(['\\', '/']).map(|j| j + i + 1).unwrap_or(p.len());
        let (drive, tail) = p.split_at(j);
        return if tail.starts_with(is_sep) { (drive, &tail[..1], &tail[1..]) } else { (drive, "", tail) };
    }
    if b.len() >= 2 && b[1] == b':' && (b[0] as char).is_ascii() {
        let (drive, tail) = p.split_at(2);
        return if tail.starts_with(is_sep) { (drive, &tail[..1], &tail[1..]) } else { (drive, "", tail) };
    }
    if p.starts_with(is_sep) {
        return ("", &p[..1], &p[1..]);
    }
    ("", "", p)
}

/// os.path.join
pub fn join(parts: &[&str]) -> String {
    let Some(first) = parts.first() else { return String::new() };
    let (d, r, p) = splitroot(first);
    let (mut drive, mut root, mut path) = (d.to_string(), r.to_string(), p.to_string());
    for part in &parts[1..] {
        let (pd, pr, pp) = splitroot(part);
        if !pr.is_empty() {
            if !pd.is_empty() || drive.is_empty() {
                drive = pd.to_string();
            }
            root = pr.to_string();
            path = pp.to_string();
            continue;
        } else if !pd.is_empty() && pd != drive {
            if pd.to_lowercase() != drive.to_lowercase() {
                drive = pd.to_string();
                root = pr.to_string();
                path = pp.to_string();
                continue;
            }
            drive = pd.to_string();
        }
        if !path.is_empty() && !path.ends_with(is_sep) {
            path.push('\\');
        }
        path.push_str(pp);
    }
    if !path.is_empty() && root.is_empty() && !drive.is_empty() && !drive.ends_with([':', '\\', '/']) {
        return format!("{drive}\\{path}");
    }
    format!("{drive}{root}{path}")
}

/// os.path.normpath
pub fn normpath(p: &str) -> String {
    let p = p.replace('/', "\\");
    let (drive, root, rest) = splitroot(&p);
    let mut out: Vec<&str> = Vec::new();
    for c in rest.split('\\') {
        match c {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if root.is_empty() {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    let s = format!("{drive}{root}{}", out.join("\\"));
    if s.is_empty() { ".".into() } else { s }
}

/// os.path.dirname
pub fn dirname(p: &str) -> String {
    let (drive, root, rest) = splitroot(p);
    let i = rest.rfind(is_sep).map(|i| i + 1).unwrap_or(0);
    let head = rest[..i].trim_end_matches(is_sep);
    format!("{drive}{root}{head}")
}

/// str(value) of a JSON value, as Python would print it
pub fn pystr(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "None".into(),
        serde_json::Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        other => other.to_string(),
    }
}
