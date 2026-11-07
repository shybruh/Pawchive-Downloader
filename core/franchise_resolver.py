"""
Franchise Resolver Subsystem
Provides intelligent, automated franchise and series name resolution.
Features:
- Normalized dictionary lookup across English, Romaji, and colloquial acronyms.
- Automatic high-confidence typo and spelling tolerance (fuzzy string matching).
- Substring & subtitle stripping (e.g. "Sword Art Online: Alicization" -> "Sword Art Online").
- Zero manual burden on users: new franchises and artist abbreviations are automatically learned.
"""

import os
import re
import sys
import json
import difflib
import threading
from typing import Optional, Dict, Set, List, Tuple, Any
from core.logger import logger
from core.path_utils import get_app_version


_IGNORE_FRANCHISE_TERMS: Set[str] = {
    "nsfw", "sfw", "pack", "pics", "4k", "8k", "hd", "extras", "preview",
    "standard", "cosplay", "collab", "special", "reward", "request", "poll",
    "survey", "update", "announcement", "original", "art", "artwork", "illustration",
    "cg", "video", "audio", "voice", "zip", "psd", "wip", "sketch", "doodle",
    "commission", "wallpaper", "uncensored", "censored", "sample", "unknown",
    "general", "other", "cat", "anime", "manga", "game", "comic"
}

# Subtitle / season pattern to clean up noisy franchise candidate strings
_SUBTITLE_CLEANER = re.compile(
    r'(?i)\s*[-—:]\s*(?:season\s*\d+|part\s*\d+|\d+(?:st|nd|rd|th)\s*season|the\s*final\s*season|movie|tv|ova|ona|cour\s*\d+).*$'
)
_SEASON_SUFFIX = re.compile(
    r'(?i)\s+(?:season\s*\d+|part\s*\d+|\d+(?:st|nd|rd|th)\s*season|s\d+|2nd\s*season|3rd\s*season|4th\s*season)\b'
)


# Built-in comprehensive seed catalog: English <-> Romaji <-> Abbreviations
_SEED_FRANCHISES: Dict[str, Dict[str, Any]] = {
    "Alya Sometimes Hides Her Feelings in Russian": {
        "aliases": [
            "roshidere", "tokidoki bosotto", "tokidoki bosotto rossiya-go de dereru tonari no alya-san",
            "alya sometimes hides her feelings"
        ]
    },
    "Classroom of the Elite": {
        "aliases": [
            "youkoso jitsuryoku", "youkoso jitsuryoku shijou shugi no kyoushitsu e",
            "cote", "youjitsu", "you-zitsu"
        ]
    },
    "Otonari no Tenshi-sama": {
        "aliases": [
            "the angel next door spoils me rotten", "the angel next door",
            "otonari no tenshi sama", "otonari no tenshi-sama ni itsu no ma ni ka dame ningen ni sareteita ken",
            "tenshi-sama"
        ]
    },
    "My Dress-Up Darling": {
        "aliases": [
            "my dress up darling", "sono bisque doll wa koi wo suru", "sono bisque doll", "bisque doll"
        ]
    },
    "Gotoubun no Hanayome": {
        "aliases": [
            "the quintessential quintuplets", "5-toubun no hanayome", "5toubun", "gotoubun",
            "5-toubun", "the quintuplets", "quintuplets"
        ]
    },
    "Kanojo Okarishimasu": {
        "aliases": [
            "rent a girlfriend", "rent-a-girlfriend", "kanokari", "kanojo, okarishimasu"
        ]
    },
    "The Eminence in Shadow": {
        "aliases": [
            "kage no jitsuryokusha ni naritakute!", "kage no jitsuryokusha", "eminence in shadow",
            "kagejitsu"
        ]
    },
    "Kaguya-sama: Love Is War": {
        "aliases": [
            "love is war", "kaguya-sama wa kokurasetai", "kaguya-sama", "kaguya"
        ]
    },
    "Amagami-san Chi no Enmusubi": {
        "aliases": [
            "tying the knot with an amagami sister", "amagami sisters", "amagami-san"
        ]
    },
    "The Apothecary Diaries": {
        "aliases": [
            "kusuriya no hitorigoto", "apothecary diaries", "kusuriya"
        ]
    },
    "Wistoria: Wand and Sword": {
        "aliases": [
            "tsue to tsurugi no wistoria", "wistoria"
        ]
    },
    "Chained Soldier": {
        "aliases": [
            "mato seihei no slave", "matoi seihei"
        ]
    },
    "Fire Force": {
        "aliases": [
            "enen no shouboutai"
        ]
    },
    "The Fragrant Flower Blooms with Dignity": {
        "aliases": [
            "kaoru hana wa rin to saku", "fragrant flower"
        ]
    },
    "The 100 Girlfriends Who Really Love You": {
        "aliases": [
            "100 kanojo", "100 girlfriends", "kimi no koto ga daidaidaidaidaisuki na 100-nin no kanojo"
        ]
    },
    "2.5 Dimensional Seduction": {
        "aliases": [
            "2.5-jigen no ririsa", "2.5-jigen no seduction", "ririsa"
        ]
    },
    "The Shiunji Family Children": {
        "aliases": [
            "the shiunji family", "shiunji-ke no kodomo-tachi", "shiunji"
        ]
    },
    "Let This Grieving Soul Retire!": {
        "aliases": [
            "let this grieving soul retire", "nageki no bourei wa intai shitai", "nageki no bourei"
        ]
    },
    "The Dangers in My Heart": {
        "aliases": [
            "boku no kokoro no yabai yatsu", "bokuyaba"
        ]
    },
    "Horimiya": {
        "aliases": [
            "hori-san to miyamura-kun"
        ]
    },
    "Mayonaka Heart Tune": {
        "aliases": [
            "tune in to the midnight heart", "mayochu"
        ]
    },
    "Class de 2-banme ni Kawaii": {
        "aliases": [
            "class de 2-banme ni kawaii onnanoko to tomodachi ni natta"
        ]
    },
    "Class no Daikirai": {
        "aliases": [
            "class no daikirai na joshi to kekkon suru koto ni natta"
        ]
    },
    "Demon Slayer": {
        "aliases": [
            "kimetsu no yaiba", "kimetsu"
        ]
    },
    "Fate": {
        "aliases": [
            "fate saga", "fate series", "fate/stay night", "fate stay night", "fate zero", "fate/zero"
        ]
    },
    "Fate/Grand Order": {
        "aliases": [
            "fate grand order", "fgo"
        ]
    },
    "Sword Art Online": {
        "aliases": [
            "sao", "sword art online ii", "sword art online progressive"
        ]
    },
    "Danmachi": {
        "aliases": [
            "is it wrong to try to pick up girls in a dungeon?",
            "dungeon ni deai wo motomeru no wa machigatteiru darou ka",
            "danmacchi"
        ]
    },
    "Genshin Impact": {
        "aliases": [
            "genshin", "原神"
        ]
    },
    "Honkai: Star Rail": {
        "aliases": [
            "honkai star rail", "hsr", "star rail"
        ]
    },
    "Zenless Zone Zero": {
        "aliases": [
            "zzz"
        ]
    },
    "Bocchi the Rock!": {
        "aliases": [
            "bocchi the rock", "bocchi"
        ]
    },
    "Oshi no Ko": {
        "aliases": [
            "【oshi no ko】", "oshinoko", "my star"
        ]
    },
    "Frieren": {
        "aliases": [
            "sousou no frieren", "frieren: beyond journey's end", "frieren beyond journey's end"
        ]
    },
    "Konosuba": {
        "aliases": [
            "kono subarashii sekai ni shukufuku wo!", "kono subarashii sekai ni shukufuku wo",
            "god's blessing on this wonderful world!"
        ]
    },
    "Mushoku Tensei": {
        "aliases": [
            "mushoku tensei: isekai ittara honki dasu", "jobless reincarnation"
        ]
    },
    "Re:Zero": {
        "aliases": [
            "re:zero kara hajimeru isekai seikatsu", "re: zero", "rezero"
        ]
    },
    "Makeine: Too Many Losing Heroines!": {
        "aliases": [
            "make heroine ga oosugiru!", "make heroine ga oosugiru", "makeine"
        ]
    },
    "Bunny Girl Senpai": {
        "aliases": [
            "seishun buta yarou wa bunny girl senpai no yume wo minai",
            "rascal does not dream of bunny girl senpai", "aobuta"
        ]
    },
    "High School DxD": {
        "aliases": [
            "high school dxd", "highschool dxd", "dxd"
        ]
    },
    "Oregairu": {
        "aliases": [
            "yahari ore no seishun love come wa machigatteiru.",
            "my teen romantic comedy snafu", "yahari ore no seishun"
        ]
    },
    "Jujutsu Kaisen": {
        "aliases": [
            "jjk"
        ]
    },
    "Chainsaw Man": {
        "aliases": [
            "csm"
        ]
    },
    "Cosmic Princess Kaguya": {
        "aliases": [
            "cosmic princess kaguya!"
        ]
    },
    "Yani Neko": {
        "aliases": [
            "chainsmoker cat"
        ]
    }
}


class FranchiseResolver:
    """
    Intelligent, self-learning Franchise & Series Resolver.
    Combines a pre-seeded catalog, a local cache of learned names, and fuzzy matching.
    """

    def __init__(self, cache_path: Optional[str] = None):
        self._lock = threading.Lock()
        self._alias_to_canonical: Dict[str, str] = {}
        self._canonicals: Dict[str, str] = {}  # lowercase canonical -> display canonical
        # Answers reused while the alias map is unchanged (_ver goes up when it changes): Known.txt asks
        # for every franchise's aliases at each reload, and each answer walked the whole 100,000-alias map
        self._ver = 0
        self._offline_miss: Dict[str, int] = {}          # normalized text -> _ver when nothing matched
        self._by_canonical: Optional[Dict[str, Set[str]]] = None
        self._by_canonical_ver = -1

        if cache_path:
            self._cache_path = cache_path
        else:
            from core.path_utils import get_data_dir
            self._cache_path = os.path.join(get_data_dir(), "data", "franchise_cache.json")

        self._load()

    def _normalize(self, text: str) -> str:
        """Cleans and normalizes candidate franchise text."""
        if not text:
            return ""
        # Strip brackets, quotes, and symbols
        cleaned = re.sub(r'^[【\[][^】\]]+[】\]]\s*:?\s*', '', text)
        cleaned = re.sub(r'[【】\[\]()_|\-+=~～!！?？"\'`]', ' ', cleaned)
        # Strip subtitles like " - Season 2"
        cleaned = _SUBTITLE_CLEANER.sub('', cleaned)
        cleaned = _SEASON_SUFFIX.sub('', cleaned)
        # Collapse whitespace
        return " ".join(cleaned.lower().split())

    def _load(self):
        """Loads pre-seeded franchises and merges local persistent cache."""
        with self._lock:
            # 1. Populate seed catalog
            for canonical, info in _SEED_FRANCHISES.items():
                c_norm = self._normalize(canonical)
                self._canonicals[c_norm] = canonical
                self._alias_to_canonical[c_norm] = canonical
                for alias in info.get("aliases", []):
                    a_norm = self._normalize(alias)
                    if a_norm:
                        self._alias_to_canonical[a_norm] = canonical

            # 2. Merge local cache file if exists
            if os.path.exists(self._cache_path):
                try:
                    with open(self._cache_path, "r", encoding="utf-8") as f:
                        data = json.load(f)
                        canonicals = data.get("canonicals", {})
                        for canonical, aliases in canonicals.items():
                            c_norm = self._normalize(canonical)
                            self._canonicals[c_norm] = canonical
                            self._alias_to_canonical[c_norm] = canonical
                            for alias in aliases:
                                a_norm = self._normalize(alias)
                                if a_norm:
                                    self._alias_to_canonical[a_norm] = canonical
                except Exception as ex:
                    logger.debug(f"Could not load franchise_cache.json: {ex}", category="known")

    def register_alias(self, alias: str, canonical: str):
        """Registers a franchise alias (in memory)."""
        if not alias or not canonical:
            return
        a_norm = self._normalize(alias)
        c_norm = self._normalize(canonical)
        if not a_norm or not c_norm:
            return

        with self._lock:
            if self._alias_to_canonical.get(a_norm) != canonical or self._canonicals.get(c_norm) != canonical:
                self._ver += 1
            self._canonicals[c_norm] = canonical
            self._alias_to_canonical[a_norm] = canonical

    def get_aliases(self, canonical_or_alias: str) -> List[str]:
        """Returns all known aliases for the given franchise or alias."""
        if not canonical_or_alias:
            return []
        resolved = self.resolve(canonical_or_alias) or canonical_or_alias
        c_norm = self._normalize(resolved)
        with self._lock:
            target_canonical = self._canonicals.get(c_norm, resolved)
            if self._by_canonical is None or self._by_canonical_ver != self._ver:
                index: Dict[str, Set[str]] = {}
                for a_norm, can in self._alias_to_canonical.items():
                    index.setdefault(can.lower(), set()).add(a_norm)
                self._by_canonical, self._by_canonical_ver = index, self._ver
            aliases = set(self._by_canonical.get(target_canonical.lower(), ()))
            aliases |= self._by_canonical.get(resolved.lower(), set())
        return sorted(aliases)

    def resolve(self, text: str) -> Optional[str]:
        """
        Resolves an input franchise candidate to its canonical name.
        Uses exact alias match, substring containment and fuzzy matching.
        """
        if not text:
            return None

        norm = self._normalize(text)
        if not norm or len(norm) < 2 or norm in _IGNORE_FRANCHISE_TERMS or norm.isdigit():
            return None

        with self._lock:
            # 1. Exact match in alias or canonical map (O(1))
            if norm in self._alias_to_canonical:
                return self._alias_to_canonical[norm]
            known_miss = self._offline_miss.get(norm) == self._ver
        if not known_miss:
            hit = self._resolve_offline(norm)
            if hit:
                return hit
        return None

    def _resolve_offline(self, norm: str) -> Optional[str]:
        """Steps 2 and 3 of resolve (substring, then fuzzy match); a miss is remembered until the alias
        map changes."""
        with self._lock:
            # 2. Substring & Prefix match (e.g. "Classroom of the Elite Season 2" -> "Classroom of the Elite")
            for alias_norm, canonical in self._alias_to_canonical.items():
                alias_words = alias_norm.split()
                # Multi-word franchise titles can match as substring of longer title (e.g. with season suffix)
                if len(alias_words) >= 2:
                    if alias_norm in norm or (len(norm.split()) >= 2 and norm in alias_norm):
                        return canonical
                # Single-word aliases require exact match or word-boundary match
                elif alias_norm == norm:
                    return canonical

            # 3. High-Confidence Fuzzy Match (Auto-heals artist typos like "Danmacchi", "Fellings").
            # difflib's cheap upper bounds rule most aliases out before the full comparison.
            best_canonical = None
            best_ratio = 0.0
            sm = difflib.SequenceMatcher(None)
            sm.set_seq2(norm)
            for alias_norm, canonical in self._alias_to_canonical.items():
                if len(alias_norm) >= 4 and abs(len(alias_norm) - len(norm)) <= 4:
                    sm.set_seq1(alias_norm)
                    if sm.real_quick_ratio() < 0.85 or sm.quick_ratio() < 0.85:
                        continue
                    ratio = sm.ratio()
                    if ratio > best_ratio and ratio >= 0.85:
                        best_ratio = ratio
                        best_canonical = canonical

            if best_canonical:
                # Register the typo variant so next time is O(1)
                self._alias_to_canonical[norm] = best_canonical
                self._ver += 1
                return best_canonical
            self._offline_miss[norm] = self._ver
        return None
