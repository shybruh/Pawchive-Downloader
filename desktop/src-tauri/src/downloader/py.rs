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
