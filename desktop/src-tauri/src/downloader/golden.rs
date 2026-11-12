//! Checks the ported filters and naming against the Python version's answers
//! (tests/fixtures/filters.json; regenerate with: python tools/golden_filters.py).

use super::{filters as f, naming as n};
use serde_json::{json, Value};

fn s<'a>(a: &'a Value, k: &str) -> &'a str {
    a[k].as_str().unwrap_or("")
}

fn run(fun: &str, a: &Value) -> Value {
    let opt_none = |v: &Value| -> Option<i64> { v.as_i64() };
    let opt_f = |v: &Value| -> Option<f64> { v.as_f64() };
    match fun {
        "clean_filesystem_text" => json!(n::clean_filesystem_text(s(a, "text"), a["max_len"].as_u64().unwrap() as usize, s(a, "fallback"))),
        "contains_term" => json!(f::contains_term(s(a, "text"), s(a, "term"))),
        "normalize_size_str" => json!(f::normalize_size_str(s(a, "s"))),
        "parse_size_str" => json!(f::parse_size_str(s(a, "s"))),
        "size_range" => {
            let (lo, hi) = f::size_range_from_skip_words(s(a, "skip_words"));
            json!([lo, hi])
        }
        "format_size_str" => json!(f::format_size_str(a["n"].as_i64().unwrap())),
        "normalize_date" => json!(f::normalize_date(s(a, "s"))),
        "expand_date_start" => json!(f::expand_date_start(s(a, "s"))),
        "expand_date_end" => json!(f::expand_date_end(s(a, "s"))),
        "parse_extensions_list" => json!(f::parse_extensions_list(s(a, "raw")).into_iter().collect::<Vec<_>>()),
        "file_type_category" => json!(f::file_type_category(s(a, "filename"))),
        "options" => f::Options::from_dict(&a["d"]).to_dict(),
        "should_keep_post" => {
            let (k, r) = f::should_keep_post(&a["post"], &f::Options::from_dict(&a["options"]));
            json!([k, r])
        }
        "should_keep_file" => {
            let (k, r) = f::should_keep_file(s(a, "filename"), &f::Options::from_dict(&a["options"]), opt_none(&a["file_size"]));
            json!([k, r])
        }
        "sanitize_filename" => json!(n::sanitize_filename(s(a, "filename"), &f::Options::from_dict(&json!({"remove_words": s(a, "remove_words")})))),
        "format_custom_filename" => {
            let parts = n::NameParts {
                original_filename: s(a, "original_filename"),
                post_title: s(a, "post_title"),
                post_date: s(a, "post_date"),
                post_index: a["post_index"].as_i64().unwrap(),
                file_index: a["file_index"].as_i64().unwrap(),
                folder_index: a["folder_index"].as_i64(),
                post_id: s(a, "post_id"),
                artist: s(a, "artist"),
                service: s(a, "service"),
                user_id: s(a, "user_id"),
                caption: s(a, "caption"),
            };
            json!(n::format_custom_filename(&parts, &f::Options::from_dict(&a["options"])))
        }
        "extract_post_title_and_caption" => {
            let (t, c) = n::post_title_and_caption(&a["post"], 100, "Untitled");
            json!([t, c])
        }
        "extract_content_images" => json!(n::content_images(s(a, "html"))),
        "normalize_tags" => json!(n::normalize_tags(&a["raw"])),
        "fit_path_for_windows" => json!(n::fit_path_for_windows(s(a, "path"), n::WINDOWS_MAX_PATH)),
        "creator_folder" => json!(n::creator_folder(s(a, "name"), s(a, "service"), a["include_service"].as_bool().unwrap())),
        "creator_folder_names" => json!(n::creator_folder_names(s(a, "name"), s(a, "service"), a["include_service"].as_bool().unwrap())),
        "is_creator_folder" => json!(n::is_creator_folder(s(a, "folder"), s(a, "name"), s(a, "service"), a["include_service"].as_bool().unwrap())),
        other => panic!("no Rust counterpart for {other}"),
    }
}

/// Numbers compare by value (Python's 45.0 and Rust's 45 are the same answer)
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

#[test]
fn matches_python_filters_and_naming() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/filters.json")).unwrap();
    assert!(cases.len() >= 900, "the reference set looks truncated");
    let mut wrong = Vec::new();
    for c in &cases {
        let fun = c["fn"].as_str().unwrap();
        let got = run(fun, &c["args"]);
        if !same(&got, &c["result"]) {
            wrong.push(format!("{fun} {}\n  python: {}\n  rust:   {}", c["args"], c["result"], got));
        }
    }
    let shown: Vec<_> = wrong.iter().take(25).cloned().collect();
    assert!(wrong.is_empty(), "{} of {} cases differ (first {}):\n{}", wrong.len(), cases.len(), shown.len(), shown.join("\n"));
}

/// tests/fixtures/extract.json (python tools/golden_extract.py)
fn run_extract(fun: &str, a: &Value) -> Value {
    use super::{extract as x, order as o};
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    match fun {
        "clean_and_normalize_url" => json!(x::clean_and_normalize_url(&s("s"))),
        "categorize_url" => json!(x::categorize_url(&s("s"))),
        "is_non_media_host" => json!(x::is_non_media_host(&s("s"))),
        "links_from_text" => json!(x::links_from_text(&s("s"))),
        "rescue" => json!(x::rescue_obfuscated_and_base64(&s("s"))),
        "passwords_with_positions" => json!(x::passwords_with_positions(&s("s")).into_iter().map(|p| json!([p.password, p.start, p.end])).collect::<Vec<_>>()),
        "links_with_details" => json!(x::links_with_details(&s("s"))),
        "clean_password_candidate" => json!(x::clean_password_candidate(&s("s"))),
        "links_from_post" => json!(x::links_from_post(&a["post"])),
        "embed_urls" => json!(x::embed_urls(&a["post"])),
        "post_vault_record" => {
            /// The made-up site of the cross-post cases: {"posts": {id: post}, "comments": {id: [...]}}
            struct Site(Value);
            impl x::PostSource for Site {
                fn single_post(&self, p: &super::links::ParsedLink) -> Option<Value> {
                    self.0["posts"].get(p.post_id.as_deref()?).cloned()
                }
                fn comments(&self, _: &str, _: &str, _: &str, post_id: &str) -> Vec<Value> {
                    self.0["comments"].get(post_id).and_then(Value::as_array).cloned().unwrap_or_default()
                }
            }
            let w = &a["where"];
            let site = a.get("site").map(|s| Site(s.clone()));
            let ws = |i: usize| w[i].as_str().unwrap_or("").to_string();
            x::post_vault_record(&a["post"], site.as_ref().map(|s| s as &dyn x::PostSource), &ws(0), &ws(1), &ws(2), &|_, _| {})
        }
        "order_files" => {
            let names: Vec<String> = serde_json::from_value(a["names"].clone()).unwrap();
            json!(o::order_files(names, &s("order"), |n| n.clone()))
        }
        "cmp" => json!(match o::name_sort_key(&s("a")).cmp(&o::name_sort_key(&s("b"))) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        other => panic!("no Rust counterpart for {other}"),
    }
}

#[test]
fn matches_python_extractor_and_order() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("../../tests/fixtures/extract.json")).unwrap();
    let mut wrong = Vec::new();
    for c in &cases {
        let fun = c["fn"].as_str().unwrap();
        let got = run_extract(fun, &c["args"]);
        if !same(&got, &c["result"]) {
            wrong.push(format!("{fun} {}\n  python: {}\n  rust:   {}", c["args"], c["result"], got));
        }
    }
    let shown: Vec<_> = wrong.iter().take(30).cloned().collect();
    assert!(wrong.is_empty(), "{} of {} cases differ (first {}):\n{}", wrong.len(), cases.len(), shown.len(), shown.join("\n"));
}
