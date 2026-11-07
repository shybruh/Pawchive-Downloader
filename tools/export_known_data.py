"""Writes the Known matcher's word lists and franchise catalog (core/known_manager.py,
core/franchise_resolver.py) to desktop/src-tauri/src/known/data.json, which the Rust port
embeds. Run it after changing any of them:

    python tools/export_known_data.py
"""

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

import core.franchise_resolver as fr  # noqa: E402
import core.known_manager as km  # noqa: E402

OUT = os.path.join(ROOT, "desktop", "src-tauri", "src", "known", "data.json")


def main():
    data = {
        # (sets: only membership counts)
        "ignored_tags": sorted(km.IGNORED_TAGS),
        "generic_title_words": sorted(km.GENERIC_TITLE_WORDS),
        "noise_tags": sorted(km._NOISE_TAGS),
        "generic_segment_tags": sorted(km._GENERIC_SEGMENT_TAGS),
        "blocked_franchises": sorted(km._BLOCKED_FRANCHISES),
        "ignore_franchise_terms": sorted(fr._IGNORE_FRANCHISE_TERMS),
        # (in order: the order they are tried in)
        "multilingual_franchises": [[k, v] for k, v in km.MULTILINGUAL_FRANCHISE_MAP.items()],
        "honorific_suffixes": list(km.MULTILINGUAL_HONORIFIC_SUFFIXES),
        "diminutive_prefixes": list(km.DIMINUTIVE_PREFIXES),
        "seed_franchises": [[k, list(v.get("aliases", []))] for k, v in fr._SEED_FRANCHISES.items()],
    }
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=1)
    print(f"-> {os.path.relpath(OUT, ROOT)} ({len(data['seed_franchises'])} franchises)")


if __name__ == "__main__":
    main()
