//! Python's difflib.SequenceMatcher (no junk function, autojunk on) for strings: ratio(),
//! quick_ratio() and real_quick_ratio(), giving the same numbers (the Known matcher's typo
//! tolerance depends on them).

use std::collections::HashMap;

pub struct SequenceMatcher {
    a: Vec<char>,
    b: Vec<char>,
    /// b's positions of each character (without the "popular" ones of a long b)
    b2j: HashMap<char, Vec<usize>>,
    full_bcount: Option<HashMap<char, usize>>,
}

impl SequenceMatcher {
    pub fn new(a: &str, b: &str) -> Self {
        let mut m = Self { a: a.chars().collect(), b: Vec::new(), b2j: HashMap::new(), full_bcount: None };
        m.set_seq2(b);
        m
    }

    pub fn set_seq1(&mut self, a: &str) {
        self.a = a.chars().collect();
    }

    pub fn set_seq2(&mut self, b: &str) {
        self.b = b.chars().collect();
        self.full_bcount = None;
        let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
        for (i, c) in self.b.iter().enumerate() {
            b2j.entry(*c).or_default().push(i);
        }
        // autojunk: in a long b, characters that make up more than 1% of it are left out
        let n = self.b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            b2j.retain(|_, idxs| idxs.len() <= ntest);
        }
        self.b2j = b2j;
    }

    fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> (usize, usize, usize) {
        let (a, b) = (&self.a, &self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, ch) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(ch) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j.checked_sub(1).and_then(|p| j2len.get(&p)).copied().unwrap_or(0) + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        // (no junk: the match is grown over equal neighbours, popular characters included)
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi && bestj + bestsize < bhi && a[besti + bestsize] == b[bestj + bestsize] {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    fn matching_chars(&self) -> usize {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut total = 0;
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(alo, ahi, blo, bhi);
            if k > 0 {
                total += k;
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        total
    }

    fn ratio_of(&self, matches: usize) -> f64 {
        let t = self.a.len() + self.b.len();
        if t == 0 { 1.0 } else { 2.0 * matches as f64 / t as f64 }
    }

    pub fn ratio(&self) -> f64 {
        self.ratio_of(self.matching_chars())
    }

    pub fn quick_ratio(&mut self) -> f64 {
        if self.full_bcount.is_none() {
            let mut c: HashMap<char, usize> = HashMap::new();
            for ch in &self.b {
                *c.entry(*ch).or_default() += 1;
            }
            self.full_bcount = Some(c);
        }
        let full = self.full_bcount.as_ref().unwrap();
        let mut avail: HashMap<char, isize> = HashMap::new();
        let mut matches = 0;
        for ch in &self.a {
            let n = *avail.entry(*ch).or_insert_with(|| full.get(ch).copied().unwrap_or(0) as isize);
            avail.insert(*ch, n - 1);
            if n > 0 {
                matches += 1;
            }
        }
        self.ratio_of(matches)
    }

    pub fn real_quick_ratio(&self) -> f64 {
        self.ratio_of(self.a.len().min(self.b.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_as_python() {
        // (python: difflib.SequenceMatcher(None, a, b).ratio() / quick_ratio() / real_quick_ratio())
        let cases: &[(&str, &str, f64, f64, f64)] = &[
            ("danmachi", "danmacchi", 0.9411764705882353, 0.9411764705882353, 0.9411764705882353),
            ("abcd", "bcda", 0.75, 1.0, 1.0),
            ("hatsune miku", "hatsun miku", 0.9565217391304348, 0.9565217391304348, 0.9565217391304348),
            ("", "", 1.0, 1.0, 1.0),
            ("abc", "", 0.0, 0.0, 0.0),
            ("tifa lockhart", "lockhart tifa", 0.6153846153846154, 1.0, 1.0),
        ];
        for (a, b, r, q, rq) in cases {
            let mut m = SequenceMatcher::new(a, b);
            assert!((m.ratio() - r).abs() < 1e-12, "{a} {b} ratio {}", m.ratio());
            assert!((m.quick_ratio() - q).abs() < 1e-12, "{a} {b} quick");
            assert!((m.real_quick_ratio() - rq).abs() < 1e-12, "{a} {b} real quick");
        }
        // A long b: its "popular" characters are left out of the search (autojunk)
        let a = format!("{}xyz{}", "a".repeat(150), "b".repeat(60));
        let b = format!("{}xyz{}", "b".repeat(120), "a".repeat(90));
        let mut m = SequenceMatcher::new(&a, &b);
        assert!((m.ratio() - 0.014084507042253521).abs() < 1e-12, "long ratio {}", m.ratio());
        assert!((m.quick_ratio() - 0.7183098591549296).abs() < 1e-12);
        assert!((m.real_quick_ratio() - 1.0).abs() < 1e-12);
    }
}
