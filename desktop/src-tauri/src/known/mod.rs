//! The Known list (Known.txt): which series and character a post belongs to, for "Put matches in
//! their own folders". A port of core/known_manager.py's matching (KnownManager.load's indexes,
//! find_matching_hierarchy and what it uses), checked against it by tools/golden_known.py.
//!
//! Known.txt: `[Franchise | Alias | …]` section headers, then one character per line
//! (`Name | Alias | …`); characters before any section stand alone.

pub mod difflib;
pub mod resolver;

use indexmap::IndexMap;
use regex::Regex;
use resolver::Resolver;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

// ── The word lists (exported from the Python module: tools/export_known_data.py) ──────────────

#[derive(serde::Deserialize)]
pub struct Data {
    ignored_tags: HashSet<String>,
    generic_title_words: HashSet<String>,
    noise_tags: HashSet<String>,
    generic_segment_tags: HashSet<String>,
    blocked_franchises: HashSet<String>,
    pub ignore_franchise_terms: Vec<String>,
    multilingual_franchises: Vec<(String, Vec<String>)>,
    honorific_suffixes: Vec<String>,
    diminutive_prefixes: Vec<String>,
    pub seed_franchises: Vec<(String, Vec<String>)>,
}

pub fn data() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(|| serde_json::from_str(include_str!("data.json")).expect("known data"))
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid pattern")
}

macro_rules! cached {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| re($p))
    }};
}

// ── Python's string helpers ──────────────────────────────────────────────────

/// str.isdigit for one character (decimal digits, and digits like ² ① ⑴)
pub fn py_isdigit(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    if get_general_category(c) == G::DecimalNumber {
        return true;
    }
    matches!(c, '²' | '³' | '¹' | '⁰' | '⁴'..='⁹' | '₀'..='₉' | '①'..='⑨' | '⑴'..='⑼' | '⒈'..='⒐' | '⓵'..='⓽' | '❶'..='❾' | '➀'..='➈' | '➊'..='➒' | '⓪' | '⓿')
}

fn isdigit(s: &str) -> bool {
    !s.is_empty() && s.chars().all(py_isdigit)
}

fn clen(s: &str) -> usize {
    s.chars().count()
}

/// str.strip(chars)
fn strip<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_matches(|c| chars.contains(c))
}

const DELIM_STRIP: &str = "【】[]() _|-+=~～?!¿¡\"'`";

pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{ac00}'..='\u{d7af}'))
}

pub fn tokenize(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    cached!(r#"[\s_\-,;:|/\\~～+&'’"“”‘`()\[\]{}<>【】《》「」『』（）〔〕〈〉［］〖〗〘〙〚〛«»!?·・•、。，；：]+"#)
        .split(&text.to_lowercase())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn canonical_entry(entry: &str) -> String {
    let first = entry.split('|').next().unwrap_or("").trim();
    if first.is_empty() { entry.trim().to_string() } else { first.to_string() }
}

/// An entry's names (Name | Alias | …), plus "Last First" for two-word names
fn entry_aliases(entry: &str) -> Vec<String> {
    let parts: Vec<String> = entry.split('|').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect();
    let aliases = if parts.is_empty() { vec![entry.trim().to_string()] } else { parts };
    let mut extra = Vec::new();
    for a in &aliases {
        let w: Vec<&str> = a.split_whitespace().collect();
        if w.len() == 2 {
            let rev = format!("{} {}", w[1], w[0]);
            if rev.to_lowercase() != a.to_lowercase() && !aliases.contains(&rev) {
                extra.push(rev);
            }
        }
    }
    aliases.into_iter().chain(extra).collect()
}

fn find_token_sequence(hay: &[String], needle: &[String]) -> Option<usize> {
    let n = needle.len();
    if n == 0 || n > hay.len() {
        return None;
    }
    (0..=hay.len() - n).find(|&i| hay[i..i + n] == *needle)
}

fn is_invalid_franchise(f: &str) -> bool {
    if clen(f) < 2 {
        return true;
    }
    let fl = f.to_lowercase();
    let fl = fl.trim();
    if data().blocked_franchises.contains(fl) || isdigit(fl) {
        return true;
    }
    if [".log", ".png", ".jpg", ".jpeg", ".txt", ".zip", ".rar", ".7z", ".psd", ".mp4"].iter().any(|e| fl.ends_with(e)) {
        return true;
    }
    if fl.starts_with([')', ']', '}', '"', '\'']) || fl.ends_with(['(', '[', '{']) {
        return true;
    }
    if fl.matches('(').count() != fl.matches(')').count() {
        return true;
    }
    matches!((fl.find(')'), fl.find('(')), (Some(c), Some(o)) if c < o)
}

fn is_filler(tok: &str) -> bool {
    let d = data();
    d.noise_tags.contains(tok) || d.generic_title_words.contains(tok) || isdigit(tok) || cached!(r"^[\d\s\-_~～\.,:;]+$").is_match(tok)
}

fn split_camel_case(s: &str) -> String {
    let s = cached!(r"([a-z0-9])([A-Z])").replace_all(s, "$1 $2");
    cached!(r"([A-Z]+)([A-Z][a-z])").replace_all(&s, "$1 $2").into_owned()
}

/// A file name's word groups: the meaningful words (and their runs of up to 4), then all of them
fn filename_tokens(filename: &str) -> Vec<Vec<String>> {
    if filename.is_empty() {
        return Vec::new();
    }
    let base = crate::downloader::py::basename(filename.split('?').next().unwrap_or(""));
    let stem = crate::downloader::py::splitext(base).0;
    let raw = tokenize(&split_camel_case(stem));
    if raw.is_empty() {
        return Vec::new();
    }
    let d = data();
    let clean: Vec<String> = raw
        .iter()
        .filter(|t| !d.noise_tags.contains(*t) && !d.generic_title_words.contains(*t) && !d.ignored_tags.contains(*t) && !isdigit(t) && !cached!(r"^[\d\s\-_~～\.,:;]+$").is_match(t))
        .cloned()
        .collect();
    let mut res: Vec<Vec<String>> = Vec::new();
    let add = |g: Vec<String>, res: &mut Vec<Vec<String>>| {
        if !res.contains(&g) {
            res.push(g);
        }
    };
    if !clean.is_empty() {
        res.push(clean.clone());
        for k in (1..=clean.len().min(4)).rev() {
            for i in 0..=clean.len() - k {
                add(clean[i..i + k].to_vec(), &mut res);
            }
        }
    }
    for k in (1..=raw.len().min(4)).rev() {
        for i in 0..=raw.len() - k {
            add(raw[i..i + k].to_vec(), &mut res);
        }
    }
    res
}

fn clean_content_text(content: &str) -> String {
    let snippet: String = content.chars().take(1500).collect();
    let s = cached!(r"<[^>]+>").replace_all(&snippet, " ");
    s.replace("&nbsp;", " ").replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'")
}

/// The ngrams (longest first, then left to right) of up to `max` tokens
fn ngrams(toks: &[String], max: usize) -> impl Iterator<Item = &[String]> {
    (1..=max.min(toks.len())).rev().flat_map(move |k| (0..=toks.len() - k).map(move |i| &toks[i..i + k]))
}

// ── The list and its indexes ─────────────────────────────────────────────────

type Toks = Vec<String>;

#[derive(Default)]
struct Index {
    entry_franchise_map: HashMap<String, String>,
    /// franchise -> its characters (Known.txt order)
    franchise_sections: IndexMap<String, Vec<String>>,
    custom_franchise_map: IndexMap<Toks, String>,
    custom_cjk_names: Vec<String>,
    custom_cjk_franchises: Vec<String>,
    custom_exact_map: IndexMap<Toks, (String, String)>,
    custom_first_token_map: HashMap<String, Vec<(String, Toks)>>,
    custom_last_token_map: HashMap<String, Vec<(String, Toks)>>,
    custom_single_names: Vec<String>,
    custom_words: HashSet<String>,
}

pub struct Known {
    resolver: Resolver,
    ix: Index,
    cache: HashMap<String, Option<(String, String)>>,
}

impl Known {
    /// The list from Known.txt's text (`franchise_cache` : an older version's learned names)
    pub fn new(known_txt: &str, franchise_cache: Option<&str>) -> Self {
        let mut k = Self { resolver: Resolver::new(data(), franchise_cache), ix: Index::default(), cache: HashMap::new() };
        k.load(known_txt);
        k
    }

    /// KnownManager.load: the list and its indexes
    pub fn load(&mut self, text: &str) {
        let d = data();
        let mut entries: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut efm: HashMap<String, String> = HashMap::new();
        let mut sections: IndexMap<String, Vec<String>> = IndexMap::new();
        let mut seen_in_section: HashMap<String, HashSet<String>> = HashMap::new();
        let mut aliases_of: IndexMap<String, Vec<String>> = IndexMap::new();
        let mut current: Option<String> = None;
        for line in text.lines() {
            let cleaned = line.trim();
            if cleaned.is_empty() || cleaned.starts_with('#') {
                continue;
            }
            if let Some(c) = cached!(r"^\[([^\]]+)\]$").captures(cleaned) {
                let raw = c[1].trim().to_string();
                let parts: Vec<String> = raw.split('|').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect();
                let primary = parts.first().cloned().unwrap_or(raw);
                sections.entry(primary.clone()).or_default();
                let al = aliases_of.entry(primary.clone()).or_default();
                for a in &parts[1.min(parts.len())..] {
                    if !al.contains(a) {
                        al.push(a.clone());
                    }
                    self.resolver.register_alias(a, &primary);
                }
                current = Some(primary);
                continue;
            }
            if seen.insert(cleaned.to_string()) {
                entries.push(cleaned.to_string());
            }
            if let Some(fr) = &current {
                efm.insert(cleaned.to_lowercase(), fr.clone());
                efm.insert(canonical_entry(cleaned).to_lowercase(), fr.clone());
                for a in entry_aliases(cleaned) {
                    efm.insert(a.to_lowercase(), fr.clone());
                }
                if seen_in_section.entry(fr.clone()).or_default().insert(cleaned.to_string()) {
                    sections.get_mut(fr).unwrap().push(cleaned.to_string());
                }
            }
        }

        let mut ix = Index { entry_franchise_map: efm, ..Default::default() };
        for entry in &entries {
            let canonical = canonical_entry(entry);
            if canonical.is_empty() {
                continue;
            }
            let fr = ix.entry_franchise_map.get(&canonical.to_lowercase()).cloned().unwrap_or_else(|| canonical.clone());
            for alias in entry_aliases(entry) {
                let t = tokenize(&alias);
                if t.is_empty() {
                    continue;
                }
                ix.custom_exact_map.insert(t.clone(), (fr.clone(), canonical.clone()));
                ix.custom_words.extend(t.iter().cloned());
                if t.len() == 1 {
                    ix.custom_single_names.push(canonical.clone());
                } else {
                    let (first, last) = (&t[0], &t[t.len() - 1]);
                    let ok = |w: &String| clen(w) >= 3 && !d.noise_tags.contains(w) && !d.generic_title_words.contains(w);
                    if ok(first) {
                        ix.custom_first_token_map.entry(first.clone()).or_default().push((canonical.clone(), t.clone()));
                    }
                    if last != first && ok(last) {
                        ix.custom_last_token_map.entry(last.clone()).or_default().push((canonical.clone(), t.clone()));
                    }
                }
            }
        }

        // Franchise names: the section, its aliases, and names made from it
        let roman = [("vii", "7"), ("viii", "8"), ("ix", "9"), ("iv", "4"), ("vi", "6"), ("iii", "3"), ("ii", "2"), ("x", "10"), ("v", "5")];
        let small = ["the", "of", "and", "in", "no", "to"];
        let fmap = &mut ix.custom_franchise_map;
        for fr in sections.keys() {
            let fr_toks = tokenize(fr);
            if !fr_toks.is_empty() {
                fmap.insert(fr_toks.clone(), fr.clone());
            }
            for al in aliases_of.get(fr).into_iter().flatten() {
                let t = tokenize(al);
                if !t.is_empty() {
                    fmap.insert(t, fr.clone());
                }
            }
            if fr.to_lowercase().starts_with("the ") {
                let t = tokenize(&fr.chars().skip(4).collect::<String>());
                if !t.is_empty() {
                    fmap.insert(t, fr.clone());
                }
            }
            let words: Vec<&str> = fr.split_whitespace().collect();
            if words.len() >= 3 {
                let acro: String = words.iter().filter(|w| !small.contains(&w.to_lowercase().as_str())).filter_map(|w| w.chars().next()).collect::<String>().to_lowercase();
                if clen(&acro) >= 2 {
                    fmap.insert(vec![acro], fr.clone());
                }
            }
            for (r, d_) in roman {
                if fr_toks.iter().any(|t| t == r) {
                    let sub: Toks = fr_toks.iter().map(|t| if t == r { d_.to_string() } else { t.clone() }).collect();
                    fmap.insert(sub, fr.clone());
                    if fr_toks.len() >= 2 {
                        let (a, b) = (fr_toks[0].chars().next().unwrap(), fr_toks[1].chars().next().unwrap());
                        fmap.insert(vec![format!("{a}{b}{d_}")], fr.clone());
                        fmap.insert(vec![format!("{a}{b}{r}")], fr.clone());
                    }
                }
            }
            for w in &fr_toks {
                if clen(w) >= 6 && !d.noise_tags.contains(w) && !d.generic_title_words.contains(w) && !d.ignored_tags.contains(w) {
                    fmap.entry(vec![w.clone()]).or_insert_with(|| fr.clone());
                }
            }
            let fr_lower = fr.to_lowercase();
            for (key, trans) in &d.multilingual_franchises {
                if fr_lower.contains(key.as_str()) || trans.iter().any(|t| fr_lower.contains(&t.to_lowercase())) {
                    for t in trans {
                        let tt = tokenize(t);
                        if !tt.is_empty() {
                            fmap.entry(tt).or_insert_with(|| fr.clone());
                        }
                    }
                }
            }
            for a in self.resolver.get_aliases(fr) {
                let t = tokenize(&a);
                if !t.is_empty() {
                    fmap.entry(t).or_insert_with(|| fr.clone());
                }
            }
        }

        // Names in Chinese, Japanese or Korean (found inside titles without spaces)
        let mut cjk_names: Vec<String> = Vec::new();
        let push = |v: &mut Vec<String>, s: &str| {
            let s = s.trim().to_string();
            if !v.contains(&s) {
                v.push(s);
            }
        };
        for e in &entries {
            if has_cjk(e) {
                push(&mut cjk_names, e);
            }
            for a in entry_aliases(e) {
                if has_cjk(&a) {
                    push(&mut cjk_names, &a);
                }
            }
        }
        let mut cjk_fr: Vec<String> = Vec::new();
        for fr in sections.keys() {
            if has_cjk(fr) {
                push(&mut cjk_fr, fr);
            }
            for a in aliases_of.get(fr).into_iter().flatten() {
                if has_cjk(a) {
                    push(&mut cjk_fr, a);
                }
            }
        }
        for t in ix.custom_franchise_map.keys() {
            let phrase: String = t.concat();
            if has_cjk(&phrase) && clen(&phrase) >= 2 {
                push(&mut cjk_fr, &phrase);
            }
        }
        ix.custom_cjk_names = cjk_names;
        ix.custom_cjk_franchises = cjk_fr;
        ix.franchise_sections = sections;
        self.ix = ix;
        self.cache.clear();
    }

    // ── Matching ─────────────────────────────────────────────────────────────

    /// (franchise, character) for a post, or None. The character is "" when only the series
    /// is known.
    pub fn find_matching_hierarchy(&mut self, title: &str, tags: Option<&Value>, filenames: &[String], content: &str) -> Option<(String, String)> {
        let tag_list = tags.map(crate::downloader::naming::normalize_tags).unwrap_or_default();
        let files: Vec<String> = filenames.iter().filter(|f| !f.is_empty()).cloned().collect();
        if title.is_empty() && tag_list.is_empty() && files.is_empty() && content.is_empty() {
            return None;
        }
        let key = serde_json::to_string(&(title, &tag_list, &files, content.chars().take(200).collect::<String>())).unwrap_or_default();
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let low = title.to_lowercase();
        let polls = [
            "survey", "waifu poll", "feedback", "which waifu", "what character would you like", "next big collab", "character catalog", "what's coming next",
            "new generation style", "image style", "winner of the latest post",
        ];
        let res = if !title.is_empty() && polls.iter().any(|w| low.contains(w)) {
            None
        } else {
            match if title.is_empty() { None } else { self.match_delimited_title(title) } {
                Some(r) => Some(r),
                None => self.find_fast(title, &tag_list, &files, content),
            }
        };
        if self.cache.len() > 10000 {
            self.cache.clear();
        }
        self.cache.insert(key, res.clone());
        res
    }

    fn franchise_of_character(&self, name: &str) -> Option<String> {
        if clen(name) < 2 {
            return None;
        }
        let efm = &self.ix.entry_franchise_map;
        for k in [canonical_entry(name).to_lowercase(), name.trim().to_lowercase()] {
            if let Some(fr) = efm.get(&k).filter(|f| !is_invalid_franchise(f)) {
                return Some(fr.clone());
            }
        }
        None
    }

    fn resolve_franchise_text(&mut self, text: &str) -> Option<String> {
        let cleaned = strip(text, "【】[]() _|-").trim();
        if cleaned.is_empty() {
            return None;
        }
        if let Some(f) = self.ix.custom_franchise_map.get(&tokenize(cleaned)) {
            return Some(f.clone());
        }
        self.resolver.resolve(cleaned)
    }

    /// "Character | Series", "Series _ Character", "[Extras] | Character | Series"…
    fn match_delimited_title(&mut self, title: &str) -> Option<(String, String)> {
        let delim = cached!(r"\s*[|]\s*|\s+_\s+|\s+[—–]\s+");
        if !delim.is_match(title) {
            return None;
        }
        let head = cached!(r"^[【\[][^】\]]+[】\]]\s*:?\s*");
        let t = head.replace(title, "");
        let t = cached!(r"(?i)^(?:Request|Commission)\s*[-—:]\s*").replace(&t, "");
        let mut segs: Vec<String> = Vec::new();
        for s in delim.split(&t).map(str::trim).filter(|s| !s.is_empty()) {
            let cs: String = s.chars().filter(|c| (*c as u32) < 0x10000).collect();
            let cs = cached!(r"^[\[(]?\d{4}[-/.]\d{2}[-/.]\d{2}[\])]?\s*").replace(&cs, "").into_owned();
            let cs = strip(&cs, DELIM_STRIP).trim().to_string();
            let cs = cached!(r"(?i)^(?:Request|Commission|Preview|Extras|SFW|NSFW)\s*[-—:]\s*").replace(&cs, "").into_owned();
            let cs = strip(&cs, DELIM_STRIP).trim().to_string();
            if !cs.is_empty()
                && !data().generic_segment_tags.contains(&cs.to_lowercase())
                && !cached!(r"(?i)^(?:Request|Commission|Preview|Extras|SFW|NSFW|\d+\s*pics?|\d+k|standar|standard)$").is_match(&cs)
            {
                segs.push(cs);
            }
        }
        if segs.is_empty() {
            return None;
        }
        if segs.len() == 1 {
            if let Some(fr) = self.resolve_franchise_text(&segs[0]) {
                return Some((fr, String::new()));
            }
            let fr = self.franchise_of_character(&segs[0])?;
            let ch = self.canonical_char(&fr, &segs[0]);
            return Some((fr, ch));
        }

        let mut candidates: Vec<(i64, Option<String>, String)> = Vec::new();
        for i in 0..segs.len() {
            let others: Vec<&str> = segs.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, s)| s.as_str()).collect();
            let ch_raw = if others.len() == 1 { others[0].to_string() } else { others.join(" ") };
            let fr_is_right = i == segs.len() - 1;
            let fr_clean = strip(&segs[i], DELIM_STRIP).trim().to_string();
            let fr_toks = tokenize(&fr_clean);
            let (mut resolved, mut fr_score): (Option<String>, i64) = (None, 0);
            if let Some(f) = self.ix.custom_franchise_map.get(&fr_toks) {
                resolved = Some(f.clone());
                fr_score = 10000;
            } else if let Some(res) = self.resolver.resolve(&fr_clean) {
                let c = resolver::normalize(&res);
                let sec = self.ix.franchise_sections.keys().find(|s| resolver::normalize(s) == c).cloned();
                fr_score = if sec.is_some() { 8000 } else { 5000 };
                resolved = Some(sec.unwrap_or(res));
            }
            let Some(resolved) = resolved else {
                candidates.push((if self.franchise_of_character(&fr_clean).is_some() { -10000 } else { -1000 }, None, String::new()));
                continue;
            };
            let mut ch_score: i64 = 0;
            let clean_ch = cached!(r"\s*\([^)]*\)|\s*\[[^\]]*\]").replace_all(&ch_raw, "").trim().to_string();
            let clean_ch = cached!(r"(?i)\b(nsfw|sfw|pack|pics|standar|standard|extra|preview|request|commission)\b").replace_all(&clean_ch, "");
            let clean_ch = strip(&clean_ch, " |_-").to_string();
            let ch_toks = tokenize(&clean_ch);
            if self.ix.custom_franchise_map.contains_key(&ch_toks) {
                ch_score -= 5000;
            }
            if let Some(chars) = self.ix.franchise_sections.get(&resolved).filter(|_| !ch_toks.is_empty()) {
                let cand: HashSet<&String> = ch_toks.iter().collect();
                for c in chars {
                    let et = tokenize(c);
                    let es: HashSet<&String> = et.iter().collect();
                    if cand.is_subset(&es) || es.is_subset(&cand) {
                        ch_score += 20000;
                        break;
                    }
                }
            }
            if self.ix.entry_franchise_map.contains_key(&clean_ch.to_lowercase()) {
                ch_score += 2000;
            }
            if ch_toks.len() >= 2 {
                ch_score += 500;
            } else if clen(&clean_ch) >= 3 {
                ch_score += 100;
            }
            let total = fr_score + ch_score + if fr_is_right { 50 } else { 0 };
            let canon = self.canonical_char(&resolved, &clean_ch);
            candidates.push((total, Some(resolved), canon));
        }
        // (stable, highest first, like Python's sort(reverse=True))
        candidates.sort_by(|a, b| b.0.cmp(&a.0));
        if let Some((score, Some(fr), ch)) = candidates.first() {
            if *score > 0 {
                return Some((fr.clone(), ch.clone()));
            }
        }
        // A segment that's a known character: its series
        for s in &segs {
            let cs = head.replace(s, "");
            let cs = cached!(r"(?i)^(?:Request|Commission|Preview|Extras|SFW|NSFW)\s*[-—:]\s*").replace(&cs, "");
            let cs = strip(&cs, "【】[]() _|-");
            let cs = cached!(r"(?i)\b(nsfw|sfw|pack|pics|standar|standard|extra|preview|request|commission)\b").replace_all(cs, "");
            let cs = strip(&cs, " |_-");
            let cand = cached!(r"\s*\([^)]*\)|\s*\[[^\]]*\]").replace_all(cs, "").trim().to_string();
            if clen(&cand) >= 3 && !data().generic_segment_tags.contains(&cand.to_lowercase()) {
                if let Some(fr) = self.franchise_of_character(&cand) {
                    let ch = self.canonical_char(&fr, &cand);
                    return Some((fr, ch));
                }
            }
        }
        // Two segments, the right one looking like a series
        let (left, right) = (&segs[0], &segs[segs.len() - 1]);
        if clen(right) >= 3 && !is_invalid_franchise(right) && !data().generic_segment_tags.contains(&right.to_lowercase()) {
            let c = cached!(r"\s*\([^)]*\)|\s*\[[^\]]*\]").replace_all(left, "").trim().to_string();
            let c = cached!(r"(?i)\b(nsfw|sfw|pack|pics|standar|standard|extra|preview)\b").replace_all(&c, "");
            let c = strip(&c, " |_-");
            if !c.is_empty() {
                return Some((right.clone(), c.to_string()));
            }
        }
        None
    }

    /// The Known.txt spelling of a character of this series
    fn canonical_char(&self, fr: &str, ch: &str) -> String {
        if ch.is_empty() {
            return String::new();
        }
        if let Some(chars) = self.ix.franchise_sections.get(fr) {
            let t = tokenize(ch);
            let toks: HashSet<&String> = t.iter().collect();
            for c in chars {
                let canon = canonical_entry(c);
                for alias in entry_aliases(c) {
                    let at = tokenize(&alias);
                    let a: HashSet<&String> = at.iter().collect();
                    if !a.is_empty() && (toks.is_subset(&a) || a.is_subset(&toks)) {
                        return canon;
                    }
                }
            }
        }
        ch.to_string()
    }

    fn detect_franchise(&self, tag_tokens: &[Toks], title_tokens: &[String], file_tokens: &[Toks], content_tokens: &[String]) -> Option<String> {
        let m = &self.ix.custom_franchise_map;
        if m.is_empty() {
            return None;
        }
        for t in tag_tokens.iter().chain(std::iter::empty()) {
            if let Some(f) = m.get(t) {
                return Some(f.clone());
            }
        }
        for g in ngrams(title_tokens, 4) {
            if let Some(f) = m.get(g) {
                return Some(f.clone());
            }
        }
        for t in file_tokens {
            if let Some(f) = m.get(t) {
                return Some(f.clone());
            }
        }
        for g in ngrams(content_tokens, 4) {
            if let Some(f) = m.get(g) {
                return Some(f.clone());
            }
        }
        let combined = title_tokens.join(" ");
        if has_cjk(&combined) {
            for cf in &self.ix.custom_cjk_franchises {
                if clen(cf) >= 2 && combined.contains(cf.as_str()) {
                    if let Some(f) = m.get(&tokenize(cf)) {
                        return Some(f.clone());
                    }
                }
            }
        }
        None
    }

    fn pick_longest(hits: &[(String, Toks)]) -> String {
        // max(hits, key=(len(tokens), len(name))): the first of the longest
        let mut best = &hits[0];
        for h in &hits[1..] {
            if (h.1.len(), clen(&h.0)) > (best.1.len(), clen(&best.0)) {
                best = h;
            }
        }
        best.0.clone()
    }

    fn with_franchise(&self, pick: &str) -> (String, String) {
        let fr = self.ix.entry_franchise_map.get(&pick.to_lowercase()).cloned().unwrap_or_else(|| pick.to_string());
        (fr, pick.to_string())
    }

    fn exact(fr: &str, canonical: &str) -> (String, String) {
        (if fr.is_empty() { canonical.to_string() } else { fr.to_string() }, canonical.to_string())
    }

    fn fuzzy_custom(&self, query: &[String]) -> Option<(String, String)> {
        let d = data();
        let mut cands: Vec<String> = Vec::new();
        for k in (1..=query.len().min(3)).rev() {
            for i in 0..=query.len() - k {
                let p = query[i..i + k].join(" ");
                if clen(&p) >= 5 && !d.noise_tags.contains(&p) && !d.generic_title_words.contains(&p) && !d.ignored_tags.contains(&p) {
                    cands.push(p);
                }
            }
        }
        for cand in &cands {
            let mut sm = difflib::SequenceMatcher::new(cand, "");
            for (toks, (fr, canonical)) in &self.ix.custom_exact_map {
                let phrase = toks.join(" ");
                let lp = clen(&phrase);
                if (clen(cand) as isize - lp as isize).abs() > 2 || lp < 5 {
                    continue;
                }
                sm.set_seq2(&phrase);
                if sm.quick_ratio() >= 0.88 && sm.ratio() >= 0.88 {
                    return Some((fr.clone(), canonical.clone()));
                }
            }
        }
        None
    }

    fn find_fast(&mut self, title: &str, tag_list: &[String], filenames: &[String], content: &str) -> Option<(String, String)> {
        let d = data();
        let hay = tokenize(title);
        let n = hay.len();
        let mut tag_tokens: Vec<Toks> = Vec::new();
        let mut flat_tags: Vec<String> = Vec::new();
        for t in tag_list {
            let tt = tokenize(t);
            if !tt.is_empty() {
                flat_tags.extend(tt.iter().cloned());
                tag_tokens.push(tt);
            }
        }
        let file_tokens: Vec<Toks> = filenames.iter().flat_map(|f| filename_tokens(f)).collect();
        let content_tokens = if content.is_empty() { Vec::new() } else { tokenize(&clean_content_text(content)) };

        // Honorifics and diminutives: "ninochan" -> "nino" when that's a name in the list
        let mut expanded = hay.clone();
        let words = &self.ix.custom_words;
        let cjk = &self.ix.custom_cjk_names;
        for t in &hay {
            let tl = clen(t);
            for sfx in &d.honorific_suffixes {
                if t.ends_with(sfx.as_str()) && tl > clen(sfx) + 1 {
                    let stem: String = t.chars().take(tl - clen(sfx)).collect();
                    if (words.contains(&stem) || cjk.contains(&stem)) && !expanded.contains(&stem) {
                        expanded.push(stem);
                    }
                }
            }
            for pfx in &d.diminutive_prefixes {
                if t.starts_with(pfx.as_str()) && tl >= 2 {
                    let stem: String = t.chars().skip(clen(pfx)).collect();
                    if (words.contains(&stem) || cjk.contains(&stem)) && !expanded.contains(&stem) {
                        expanded.push(stem);
                    }
                }
            }
        }
        if has_cjk(title) {
            for c in cjk {
                if clen(c) >= 2 && title.contains(c.as_str()) {
                    if let Some(first) = tokenize(c).into_iter().next() {
                        if !expanded.contains(&first) {
                            expanded.push(first);
                        }
                    }
                }
            }
        }

        // A series named in the post: then one of its characters, else the series alone
        if let Some(fr) = self.detect_franchise(&tag_tokens, &hay, &file_tokens, &content_tokens) {
            if let Some(chars) = self.ix.franchise_sections.get(&fr).filter(|c| !c.is_empty()) {
                let mut context: Vec<String> = expanded.clone();
                context.extend(flat_tags.iter().cloned());
                for ft in &file_tokens {
                    context.extend(ft.iter().cloned());
                }
                context.extend(content_tokens.iter().take(150).cloned());
                let context: HashSet<String> = context.into_iter().collect();
                let content200: Vec<String> = content_tokens.iter().take(200).cloned().collect();
                for c in chars {
                    let canonical = canonical_entry(c);
                    for alias in entry_aliases(c) {
                        let a = tokenize(&alias);
                        if a.is_empty() {
                            continue;
                        }
                        if tag_tokens.contains(&a)
                            || file_tokens.contains(&a)
                            || find_token_sequence(&hay, &a).is_some()
                            || (!content_tokens.is_empty() && find_token_sequence(&content200, &a).is_some())
                        {
                            return Some((fr, canonical));
                        }
                    }
                }
                for c in chars {
                    let canonical = canonical_entry(c);
                    for alias in entry_aliases(c) {
                        for tok in tokenize(&alias) {
                            let min = if has_cjk(&tok) { 2 } else { 3 };
                            if clen(&tok) >= min && context.contains(&tok) && !is_filler(&tok) {
                                return Some((fr, canonical));
                            }
                        }
                    }
                }
            }
            return Some((fr, String::new()));
        }

        let ix = &self.ix;
        if ix.custom_exact_map.is_empty() {
            return None;
        }
        // A tag that's a name in the list
        for t in &tag_tokens {
            if let Some((fr, c)) = ix.custom_exact_map.get(t) {
                return Some(Self::exact(fr, c));
            }
        }
        // Several tags together ("nakano" + "nino")
        if flat_tags.len() >= 2 {
            for k in (2..=flat_tags.len().min(4)).rev() {
                for i in 0..=flat_tags.len() - k {
                    if let Some((fr, c)) = ix.custom_exact_map.get(&flat_tags[i..i + k]) {
                        return Some(Self::exact(fr, c));
                    }
                }
            }
        }
        // The title's word runs (5 words down to 1)
        for k in (1..=n.min(5)).rev() {
            for i in 0..=n - k {
                let g = &hay[i..i + k];
                if let Some((fr, c)) = ix.custom_exact_map.get(g) {
                    if k == 1 && !ix.custom_first_token_map.is_empty() {
                        if let Some(hits) = ix.custom_first_token_map.get(&g[0]).filter(|h| !h.is_empty()) {
                            if hay[i + 1..].iter().all(|t| is_filler(t)) {
                                let pick = Self::pick_longest(hits);
                                return Some(self.with_franchise(&pick));
                            }
                        }
                    }
                    return Some(Self::exact(fr, c));
                }
            }
        }
        // Honorific stems
        if expanded.len() > n {
            for tok in &expanded[n..] {
                if let Some((fr, c)) = ix.custom_exact_map.get(std::slice::from_ref(tok)) {
                    return Some(Self::exact(fr, c));
                }
                if let Some(hits) = ix.custom_first_token_map.get(tok) {
                    let pick = Self::pick_longest(hits);
                    return Some(self.with_franchise(&pick));
                }
            }
        }
        // File names
        for f in &file_tokens {
            if let Some((fr, c)) = ix.custom_exact_map.get(f) {
                return Some(Self::exact(fr, c));
            }
        }
        // Close spellings
        if let Some(r) = self.fuzzy_custom(&hay) {
            return Some(r);
        }
        let ix = &self.ix;
        // A first or last name found alone
        let mut cand: Vec<String> = expanded.clone();
        cand.extend(flat_tags.iter().cloned());
        for f in &file_tokens {
            cand.extend(f.iter().cloned());
        }
        if !ix.custom_first_token_map.is_empty() || !ix.custom_last_token_map.is_empty() {
            for tok in &cand {
                if clen(tok) < 3 || is_filler(tok) {
                    continue;
                }
                // {name: (name, tokens)} over first then last name hits: one per name, the last one seen
                let mut all: IndexMap<String, (String, Toks)> = IndexMap::new();
                for h in ix.custom_first_token_map.get(tok).into_iter().flatten().chain(ix.custom_last_token_map.get(tok).into_iter().flatten()) {
                    all.insert(h.0.clone(), h.clone());
                }
                if all.len() == 1 {
                    let pick = all.values().next().unwrap().0.clone();
                    return Some(self.with_franchise(&pick));
                } else if all.len() > 1 {
                    for other in &cand {
                        if other != tok && clen(other) >= 3 && !is_filler(other) {
                            let sub: Vec<&(String, Toks)> = all.values().filter(|h| h.1.contains(other)).collect();
                            if sub.len() == 1 {
                                let pick = sub[0].0.clone();
                                return Some(self.with_franchise(&pick));
                            }
                        }
                    }
                    let shared: HashSet<String> = all.values().map(|h| ix.entry_franchise_map.get(&h.0.to_lowercase()).cloned().unwrap_or_else(|| h.0.clone())).collect();
                    if shared.len() == 1 {
                        let fr = shared.into_iter().next().unwrap();
                        return Some((fr.clone(), fr));
                    }
                }
            }
        }
        // A short start of a long single-word name ("kat" -> "Katarin")
        if !ix.custom_single_names.is_empty() {
            for tok in &cand {
                let tl = clen(tok);
                if (3..=5).contains(&tl) {
                    let mut uniq: Vec<&String> = Vec::new();
                    for name in &ix.custom_single_names {
                        if name.to_lowercase().starts_with(tok.as_str()) && clen(name) >= 6 && !uniq.contains(&name) {
                            uniq.push(name);
                        }
                    }
                    if uniq.len() == 1 {
                        let pick = uniq[0].clone();
                        return Some(self.with_franchise(&pick));
                    }
                }
            }
        }
        // The post's text
        if !content_tokens.is_empty() {
            let c = content_tokens.len();
            for k in (1..=c.min(4)).rev() {
                for i in 0..(c - k + 1).min(200) {
                    if let Some((fr, cn)) = ix.custom_exact_map.get(&content_tokens[i..i + k]) {
                        return Some(Self::exact(fr, cn));
                    }
                }
            }
        }
        None
    }
}

struct Loaded {
    path: String,
    stamp: Option<(std::time::SystemTime, u64)>,
    known: Known,
}

fn stamp(path: &str) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The app's Known list: Known.txt at `path` (read again when it changes, keeping the names learned
/// so far, like KnownManager.load) and `franchise_cache` (an older version's learned names).
/// None when Known.txt can't be read.
pub fn with_list<R>(path: &str, franchise_cache: &str, f: impl FnOnce(&mut Known) -> R) -> Option<R> {
    static K: OnceLock<Mutex<Option<Loaded>>> = OnceLock::new();
    let mut g = K.get_or_init(|| Mutex::new(None)).lock().unwrap_or_else(|e| e.into_inner());
    let now = stamp(path);
    let fresh = matches!(&*g, Some(l) if l.path == path && l.stamp == now && now.is_some());
    if !fresh {
        let text = std::fs::read_to_string(path).ok()?;
        match g.as_mut().filter(|l| l.path == path) {
            Some(l) => l.known.load(&text),
            None => {
                let cache = std::fs::read_to_string(franchise_cache).ok();
                *g = Some(Loaded { path: path.to_string(), stamp: None, known: Known::new(&text, cache.as_deref()) });
            }
        }
        g.as_mut().unwrap().stamp = now;
    }
    Some(f(&mut g.as_mut().unwrap().known))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tools/golden_known.py's posts give the same (series, character) as the Python matcher
    #[test]
    fn same_as_python() {
        let v: Value = serde_json::from_str(include_str!("../../tests/fixtures/known.json")).unwrap();
        let txt = v["known_txt"].as_str().unwrap();
        let cases = v["cases"].as_array().unwrap();
        assert!(cases.len() >= 500);
        let mut bad = Vec::new();
        for c in cases {
            let mut k = Known::new(txt, None);
            let files: Vec<String> = c["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
            let tags = if c["tags"].is_null() { None } else { Some(&c["tags"]) };
            let got = k.find_matching_hierarchy(c["title"].as_str().unwrap(), tags, &files, c["content"].as_str().unwrap());
            let want: Option<(String, String)> = serde_json::from_value(c["match"].clone()).unwrap();
            if got != want {
                bad.push(format!("{} {} {:?} {:?}: got {got:?}, want {want:?}", c["title"], c["tags"], files, c["content"]));
            }
        }
        assert!(bad.is_empty(), "{} of {} differ:\n{}", bad.len(), cases.len(), bad.join("\n"));
    }

    /// An edited Known.txt is read again on the next match
    #[test]
    fn reads_the_list_again_when_it_changes() {
        let dir = std::env::temp_dir().join(format!("pawchive-known-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Known.txt");
        let p = path.to_str().unwrap();
        std::fs::write(&path, "[The Witcher]
Ciri
").unwrap();
        let m = |t: &str| with_list(p, "", |k| k.find_matching_hierarchy(t, None, &[], "")).unwrap();
        assert_eq!(m("Ciri"), Some(("The Witcher".into(), "Ciri".into())));
        assert_eq!(m("Makima"), None);
        std::fs::write(&path, "[The Witcher]
Ciri

[Chainsaw Man]
Makima
").unwrap();
        assert_eq!(m("Makima"), Some(("Chainsaw Man".into(), "Makima".into())));
        assert!(with_list(dir.join("missing.txt").to_str().unwrap(), "", |_| ()).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
