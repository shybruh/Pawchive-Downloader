//! The "File order in posts" setting: posted, reversed, or natural name order that works in any
//! script ("Mya2" < "Mya2v10", "一枚目" < "十枚目", "Ａ２" == "A2", "étude" next to "etude").
//! A port of core/file_order.py.

use super::py::{casefold, category};
use std::cmp::Ordering;
use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::UnicodeNormalization;

pub fn normalize_file_order(v: &str) -> &'static str {
    match v.trim().to_lowercase().as_str() {
        "reversed" => "reversed",
        "name" => "name",
        _ => "posted",
    }
}

fn cjk_digit(c: char) -> Option<u128> {
    Some(match c {
        '〇' | '零' => 0,
        '一' | '壱' | '壹' => 1,
        '二' | '弐' | '貳' | '两' | '兩' => 2,
        '三' | '参' | '參' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '七' => 7,
        '八' => 8,
        '九' => 9,
        _ => return None,
    })
}

fn cjk_unit(c: char) -> Option<u128> {
    Some(match c {
        '十' | '拾' => 10,
        '百' => 100,
        '千' => 1000,
        _ => return None,
    })
}

fn cjk_big(c: char) -> Option<u128> {
    Some(match c {
        '万' | '萬' => 10_000,
        '億' => 100_000_000,
        _ => return None,
    })
}

fn is_cjk_numeral(c: char) -> bool {
    cjk_digit(c).is_some() || cjk_unit(c).is_some() || cjk_big(c).is_some()
}

fn cjk_value(run: &[char]) -> u128 {
    if !run.iter().any(|&c| cjk_unit(c).is_some() || cjk_big(c).is_some()) {
        // Written digit by digit, like "二〇二四" (2024)
        return run.iter().fold(0u128, |acc, &c| acc.saturating_mul(10).saturating_add(cjk_digit(c).unwrap_or(0)));
    }
    let (mut total, mut section, mut num) = (0u128, 0u128, 0u128);
    for &c in run {
        if let Some(d) = cjk_digit(c) {
            num = d;
        } else if let Some(u) = cjk_unit(c) {
            section += if num == 0 { 1 } else { num } * u;
            num = 0;
        } else if let Some(b) = cjk_big(c) {
            let s = section + num;
            total += if s == 0 { 1 } else { s } * b;
            section = 0;
            num = 0;
        }
    }
    total + section + num
}

/// str.isspace()
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn is_separator(c: char) -> bool {
    is_space(c) || ".·・~,".contains(c) || matches!(category(c), "Pd" | "Pc")
}

/// str.isdecimal() for one character, and its value
fn decimal(c: char) -> Option<u128> {
    if category(c) != "Nd" {
        return None;
    }
    // Decimal digits come in runs of ten from zero: count the digits before this one
    let mut k = 0u32;
    let mut cp = c as u32;
    while cp > 0 {
        match char::from_u32(cp - 1) {
            Some(p) if category(p) == "Nd" => {
                k += 1;
                cp -= 1;
            }
            _ => break,
        }
    }
    Some((k % 10) as u128)
}

fn strips_marks(base: char) -> bool {
    let cp = base as u32;
    cp < 0x0250 || (0x1E00..=0x1EFF).contains(&cp) || (0x0370..=0x03FF).contains(&cp) || (0x1F00..=0x1FFF).contains(&cp)
        || (0x0400..=0x04FF).contains(&cp) || (0x0590..=0x05FF).contains(&cp) || (0x0600..=0x06FF).contains(&cp)
}

fn strip_accents(text: &str) -> String {
    let mut out = String::new();
    let mut base: Option<char> = None;
    for ch in text.nfd() {
        if canonical_combining_class(ch) != 0 {
            // "й" (и + breve) is a separate letter in Russian, Ukrainian, Belarusian...
            let keep = match base {
                None => true,
                Some(b) => !strips_marks(b) || (ch == '\u{306}' && ('Ѐ'..='ӿ').contains(&b)),
            };
            if keep {
                out.push(ch);
            }
            continue;
        }
        base = Some(ch);
        out.push(ch);
    }
    out.nfc().collect()
}

fn fold(text: &str) -> Vec<char> {
    let t: String = text.nfkc().collect();
    let t = strip_accents(&casefold(&t));
    let mut chars: Vec<char> = t
        .chars()
        .map(|c| if ('\u{30A1}'..='\u{30F6}').contains(&c) { char::from_u32(c as u32 - 0x60).unwrap() } else { c })
        .collect();
    // Thai and Lao vowels written before their consonant sort by the consonant
    let mut i = 0;
    while i + 1 < chars.len() {
        let (a, b) = (chars[i], chars[i + 1]);
        if (('เ'..='ไ').contains(&a) && ('ก'..='ฮ').contains(&b)) || (('ເ'..='ໄ').contains(&a) && ('ກ'..='ຮ').contains(&b)) {
            chars.swap(i, i + 1);
            i += 2;
        } else {
            i += 1;
        }
    }
    chars
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone)]
pub enum Token {
    Sep,
    Num(u128),
    Text(String),
}

fn tokens(text: &[char]) -> Vec<Token> {
    let mut out = Vec::new();
    let (mut i, n) = (0, text.len());
    while i < n {
        let ch = text[i];
        if is_separator(ch) {
            while i < n && is_separator(text[i]) {
                i += 1;
            }
            out.push(Token::Sep);
        } else if decimal(ch).is_some() {
            let mut v = 0u128;
            while i < n {
                let Some(d) = decimal(text[i]) else { break };
                v = v.saturating_mul(10).saturating_add(d);
                i += 1;
            }
            out.push(Token::Num(v));
        } else if is_cjk_numeral(ch) {
            let j0 = i;
            while i < n && is_cjk_numeral(text[i]) {
                i += 1;
            }
            out.push(Token::Num(cjk_value(&text[j0..i])));
        } else {
            let j0 = i;
            while i < n && !(is_separator(text[i]) || decimal(text[i]).is_some() || is_cjk_numeral(text[i])) {
                i += 1;
            }
            out.push(Token::Text(text[j0..i].iter().collect()));
        }
    }
    while out.last() == Some(&Token::Sep) {
        out.pop();
    }
    out
}

fn split_ext(name: &[char]) -> (&[char], &[char]) {
    let Some(dot) = name.iter().rposition(|&c| c == '.') else { return (name, &[]) };
    if dot == 0 || dot == name.len() - 1 || name.len() - dot > 12 || name[dot..].contains(&' ') {
        return (name, &[]);
    }
    (&name[..dot], &name[dot + 1..])
}

/// The natural sort key of a file name
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum NameKey {
    Named(Vec<Token>, Vec<Token>, String, String),
    /// Nameless files go after the named ones
    Nameless,
}

pub fn name_sort_key(name: &str) -> NameKey {
    let raw = name.replace('\\', "/");
    let raw = raw.rsplit('/').next().unwrap_or("").trim();
    if raw.is_empty() {
        return NameKey::Nameless;
    }
    let chars: Vec<char> = raw.chars().collect();
    let (stem, ext) = split_ext(&chars);
    let (fs, fe) = (fold(&stem.iter().collect::<String>()), fold(&ext.iter().collect::<String>()));
    let exact = casefold(&raw.nfkc().collect::<String>());
    NameKey::Named(tokens(&fs), tokens(&fe), fs.into_iter().collect(), exact)
}

/// The items in the chosen order; ties keep their posted order
pub fn order_files<T>(mut items: Vec<T>, order: &str, name_of: impl Fn(&T) -> String) -> Vec<T> {
    match normalize_file_order(order) {
        "reversed" => {
            items.reverse();
            items
        }
        "name" => {
            let mut keyed: Vec<(NameKey, T)> = items.into_iter().map(|t| (name_sort_key(&name_of(&t)), t)).collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
            keyed.into_iter().map(|(_, t)| t).collect()
        }
        _ => items,
    }
}
