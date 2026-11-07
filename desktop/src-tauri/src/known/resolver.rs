//! Franchise names: the built-in catalog (English, romaji, abbreviations), names learned from an
//! older version's cache file, and the Known.txt section aliases; with substring matching and
//! typo healing. A port of core/franchise_resolver.py (offline: it never went online).

use super::difflib::SequenceMatcher;
use super::Data;
use indexmap::IndexMap;
use regex::Regex;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid pattern")
}

macro_rules! cached {
    ($p:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| re($p))
    }};
}

/// FranchiseResolver._normalize
pub fn normalize(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let s = cached!(r"^[【\[][^】\]]+[】\]]\s*:?\s*").replace(text, "");
    let s = cached!(r#"[【】\[\]()_|\-+=~～!！?？"'`]"#).replace_all(&s, " ");
    let s = cached!(r"(?i)\s*[-—:]\s*(?:season\s*\d+|part\s*\d+|\d+(?:st|nd|rd|th)\s*season|the\s*final\s*season|movie|tv|ova|ona|cour\s*\d+).*$").replace(&s, "");
    let s = cached!(r"(?i)\s+(?:season\s*\d+|part\s*\d+|\d+(?:st|nd|rd|th)\s*season|s\d+|2nd\s*season|3rd\s*season|4th\s*season)\b").replace_all(&s, "");
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(super::py_isdigit)
}

pub struct Resolver {
    /// normalized alias -> canonical, in the order they were added (substring and fuzzy matching
    /// take the first / best in this order)
    alias_to_canonical: IndexMap<String, String>,
    /// normalized canonical -> canonical
    canonicals: HashMap<String, String>,
    ignore: HashSet<String>,
    ver: u64,
    /// normalized text -> ver when nothing matched
    offline_miss: HashMap<String, u64>,
}

impl Resolver {
    /// The catalog, then the names an older version learned (`cache_json`: franchise_cache.json)
    pub fn new(data: &Data, cache_json: Option<&str>) -> Self {
        let mut r = Self {
            alias_to_canonical: IndexMap::new(),
            canonicals: HashMap::new(),
            ignore: data.ignore_franchise_terms.iter().cloned().collect(),
            ver: 0,
            offline_miss: HashMap::new(),
        };
        for (canonical, aliases) in &data.seed_franchises {
            let c = normalize(canonical);
            r.canonicals.insert(c.clone(), canonical.clone());
            r.alias_to_canonical.insert(c, canonical.clone());
            for a in aliases {
                let a = normalize(a);
                if !a.is_empty() {
                    r.alias_to_canonical.insert(a, canonical.clone());
                }
            }
        }
        if let Some(Ok(v)) = cache_json.map(serde_json::from_str::<serde_json::Value>) {
            if let Some(m) = v.get("canonicals").and_then(|c| c.as_object()) {
                for (canonical, aliases) in m {
                    let c = normalize(canonical);
                    r.canonicals.insert(c.clone(), canonical.clone());
                    r.alias_to_canonical.insert(c, canonical.clone());
                    for a in aliases.as_array().into_iter().flatten().filter_map(|a| a.as_str()) {
                        let a = normalize(a);
                        if !a.is_empty() {
                            r.alias_to_canonical.insert(a, canonical.clone());
                        }
                    }
                }
            }
        }
        r
    }

    pub fn register_alias(&mut self, alias: &str, canonical: &str) {
        if alias.is_empty() || canonical.is_empty() {
            return;
        }
        let (a, c) = (normalize(alias), normalize(canonical));
        if a.is_empty() || c.is_empty() {
            return;
        }
        if self.alias_to_canonical.get(&a).map(String::as_str) != Some(canonical) || self.canonicals.get(&c).map(String::as_str) != Some(canonical) {
            self.ver += 1;
        }
        self.canonicals.insert(c, canonical.to_string());
        self.alias_to_canonical.insert(a, canonical.to_string());
    }

    /// The canonical name of a franchise (or None)
    pub fn resolve(&mut self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        let norm = normalize(text);
        if norm.chars().count() < 2 || self.ignore.contains(&norm) || is_digits(&norm) {
            return None;
        }
        if let Some(c) = self.alias_to_canonical.get(&norm) {
            return Some(c.clone());
        }
        if self.offline_miss.get(&norm) == Some(&self.ver) {
            return None;
        }
        self.resolve_offline(&norm)
    }

    fn resolve_offline(&mut self, norm: &str) -> Option<String> {
        // 2. Substring: a several-word name inside a longer title ("… Season 2"), or the reverse
        let norm_words = norm.split_whitespace().count();
        for (alias, canonical) in &self.alias_to_canonical {
            if alias.split_whitespace().count() >= 2 {
                if norm.contains(alias.as_str()) || (norm_words >= 2 && alias.contains(norm)) {
                    return Some(canonical.clone());
                }
            } else if alias == norm {
                return Some(canonical.clone());
            }
        }
        // 3. A close spelling (a typo of a known name)
        let mut best: Option<String> = None;
        let mut best_ratio = 0.0;
        let mut sm = SequenceMatcher::new("", norm);
        let n = norm.chars().count() as isize;
        for (alias, canonical) in &self.alias_to_canonical {
            let la = alias.chars().count() as isize;
            if la >= 4 && (la - n).abs() <= 4 {
                sm.set_seq1(alias);
                if sm.real_quick_ratio() < 0.85 || sm.quick_ratio() < 0.85 {
                    continue;
                }
                let ratio = sm.ratio();
                if ratio > best_ratio && ratio >= 0.85 {
                    best_ratio = ratio;
                    best = Some(canonical.clone());
                }
            }
        }
        match best {
            Some(c) => {
                // (the typo is remembered: next time it's an exact match)
                self.alias_to_canonical.insert(norm.to_string(), c.clone());
                self.ver += 1;
                Some(c)
            }
            None => {
                self.offline_miss.insert(norm.to_string(), self.ver);
                None
            }
        }
    }

    /// Every known alias (normalized) of the franchise, sorted
    pub fn get_aliases(&mut self, canonical_or_alias: &str) -> Vec<String> {
        if canonical_or_alias.is_empty() {
            return Vec::new();
        }
        let resolved = self.resolve(canonical_or_alias).unwrap_or_else(|| canonical_or_alias.to_string());
        let target = self.canonicals.get(&normalize(&resolved)).cloned().unwrap_or_else(|| resolved.clone());
        let (t, r) = (target.to_lowercase(), resolved.to_lowercase());
        let out: BTreeSet<String> = self.alias_to_canonical.iter().filter(|(_, c)| { let c = c.to_lowercase(); c == t || c == r }).map(|(a, _)| a.clone()).collect();
        out.into_iter().collect()
    }
}
