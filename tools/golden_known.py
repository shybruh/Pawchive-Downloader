"""Writes the reference answers of the Known list's matching (KnownManager.find_matching_hierarchy in
core/known_manager.py) for the Rust port's test (src-tauri/src/known/mod.rs).

    python tools/golden_known.py

Output: desktop/src-tauri/tests/fixtures/known.json

A made-up Known.txt (series with aliases, characters with aliases, names in Japanese, characters
outside any series) and many posts: delimited titles ("Character | Series"), names inside longer
titles, tags, file names, post text, typos, honorifics, and posts that match nothing.
"""

import json
import os
import random
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

import core.known_manager as km  # noqa: E402
from core.franchise_resolver import FranchiseResolver  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "known.json")

KNOWN_TXT = """# test list
Hatsune Miku
Katarina
Lucoa

[Final Fantasy VII | FF7]
Tifa Lockhart
Aerith Gainsborough | Aeris
Yuffie Kisaragi

[The Witcher]
Ciri | Cirilla
Yennefer
Triss Merigold

[Go-Toubun no Hanayome | The Quintessential Quintuplets | 五等分の花嫁]
Nakano Nino | Nino
Nakano Miku
Nakano Itsuki | Itsuki

[Genshin Impact]
Raiden Shogun | Raiden Ei
Ganyu
Hu Tao
Nahida

[Sword Art Online]
Asuna Yuuki | Asuna
Sinon

[Honkai Star Rail | HSR]
Kafka
Firefly
March 7th

[Re:Zero]
Rem | レム
Emilia
Ram

[Blue Archive]
Hoshino
Shiroko
Hina

[Chainsaw Man]
Makima
Power
Reze

[Spy x Family]
Yor Forger | Yor
Anya Forger
"""

TITLES = [
    "Tifa Lockhart | Final Fantasy VII", "Final Fantasy VII _ Tifa", "Tifa (FF7) pack", "[Request] Tifa | FF7",
    "Aeris | Final Fantasy 7", "Yuffie Kisaragi", "yuffie", "Kisaragi", "Lockhart Tifa nsfw", "Tifa Lockhart",
    "Ciri | The Witcher", "Cirilla and Yennefer", "Witcher - Triss", "Triss Merigold 4k", "Yennefer of Vengerberg",
    "Nino | Go-Toubun no Hanayome", "Ninochan bikini", "Nakano Miku", "Miku", "Hatsune Miku | Vocaloid", "miku nakano",
    "Itsuki", "Quintessential Quintuplets - Itsuki", "五等分の花嫁 二乃", "Nakano sisters",
    "Raiden Shogun (Genshin Impact)", "Raiden Ei", "Ganyu | Genshin", "Hu Tao halloween", "Nahida", "Genshin Impact pack",
    "genshin", "Asuna | SAO", "Sword Art Online - Sinon", "Asuna Yuuki", "Yuuki Asuna",
    "Kafka | Honkai Star Rail", "Firefly | HSR", "March 7th", "HSR pack 3", "Star Rail Kafka",
    "Rem | Re:Zero", "レム", "Emilia (Re:Zero)", "Ram and Rem", "Re Zero Emilia",
    "Hoshino | Blue Archive", "Shiroko swimsuit", "Hina | Blue Archive", "Blue Archive pack",
    "Makima | Chainsaw Man", "Power", "Reze bomb girl", "Chainsaw Man - Makima",
    "Yor Forger | Spy x Family", "Anya", "Yor", "Spy Family Yor",
    "Katarina", "Kat", "Katarin", "Lucoa", "Quetzalcoatl Lucoa | Kobayashi Maid Dragon",
    "Tiffa Lockhart", "Tifa Lokhart", "Yenefer", "Asuna Yuki", "Raidne Shogun", "Makimaa",
    "Monthly pack", "Survey: which waifu next?", "Feedback wanted", "", "New post!", "WIP", "Sketch 03",
    "Original character | OC", "Jinx | Arcane", "Jinx | League of Legends", "2B | NieR Automata", "Zelda | Breath of the Wild",
    "Nami | One Piece", "Robin (One Piece)", "Frieren | Sousou no Frieren", "Marin Kitagawa | My Dress-Up Darling",
    "Chun-Li | Street Fighter", "Lara Croft | Tomb Raider", "Samus | Metroid", "Bayonetta",
    "[Patreon] Tifa | FF7 | 2024-05-01", "【SFW】 Asuna | SAO", "Request - Nino | 5Toubun", "Commission: Kafka | HSR",
    "Tifa | Ciri", "Ciri | Tifa", "Final Fantasy | Tifa", "Tifa — Final Fantasy VII", "Tifa – FF7",
    "Extras | Tifa | FF7", "Preview | Asuna", "NSFW | Makima", "10 pics | Power", "4k | Rem",
    "FF7 Remake", "Final Fantasy 7 Rebirth", "The Witcher 3", "Witcher", "Sword Art", "Chainsaw",
    "Blue Archive: Hoshino", "Hoshino Ai", "Hina Sorasaki", "Hina", "Power Rangers", "Ram 32gb",
    "Shirokochan", "Asuna-san", "Nahida-sama", "Ganyuu", "Hutao", "Hu-Tao",
    "Emilia Clarke", "Anya Taylor", "Firefly lantern", "Kafka on the shore", "March",
]

TAGS = [
    None, [], ["tifa"], ["Tifa Lockhart"], ["final fantasy vii"], ["nakano", "nino"], ["genshin impact", "ganyu"],
    ["nsfw", "bikini"], ["sao"], ["re:zero", "rem"], "tifa, aerith", ["blue archive"], ["hsr", "kafka"],
    ["miku"], ["hatsune miku"], ["witcher", "ciri"], [{"name": "makima"}], ["chainsaw man"], ["original"],
    ["yor forger", "anya forger"], ["cirilla"], ["Raiden Shogun"], ["lockhart"], ["kisaragi"], ["asuna"],
]

FILES = [
    [], ["image.png"], ["TifaLockhart_01.png"], ["tifa-final-fantasy.jpg"], ["Nino_Nakano_bikini.png"],
    ["GanyuGenshin.mp4"], ["001.jpg", "002.jpg"], ["asuna_sao_v2.zip"], ["preview.png?f=Makima_full.png"],
    ["RemRezero_pack.zip"], ["kafka_hsr.psd"], ["anya-forger-01.png"], ["Ciri.png"], ["katarina.png"],
    ["hu_tao_halloween.png"], ["HatsuneMiku.png"], ["Yuffie.png"], ["firefly_honkai.png"],
]

CONTENTS = [
    "", "<p>New set of Tifa from Final Fantasy VII!</p>", "<p>This month: Ganyu &amp; Hu Tao</p>",
    "<b>Thanks</b> for the support!", "Requested by a patron: Nakano Nino", "Asuna from Sword Art Online",
    "<p>Rem&nbsp;and Ram</p>", "Makima again", "WIP sketches", "Yor Forger in a dress",
]


def posts():
    out = [(t, None, [], "") for t in TITLES]
    rnd = random.Random(7)
    for t in TITLES:
        for _ in range(3):
            out.append((rnd.choice(TITLES + [""]), rnd.choice(TAGS), rnd.choice(FILES), rnd.choice(CONTENTS)))
    # Every name of the list, in titles of several shapes
    names, series, current = [], [], None
    for line in KNOWN_TXT.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            current = [p.strip() for p in line[1:-1].split("|")]
            series.extend(current)
            continue
        names.extend((p.strip(), current[0] if current else "") for p in line.split("|"))
    shapes = ["{n}", "{n} | {s}", "{s} _ {n}", "{n} ({s})", "{n} bikini set", "new {low}", "{n}chan", "{s}"]
    for n, s in names:
        for sh in shapes:
            out.append((sh.format(n=n, s=s or rnd.choice(series), low=n.lower()), None, [], ""))
        out.append(("", [n.lower()], [], ""))
        out.append(("", None, [n.replace(" ", "") + "_01.png"], ""))
        out.append(("Monthly set", None, [], f"<p>This month: {n}</p>"))
    for s in series:
        out.append((f"{s} pack", None, [], ""))
        out.append(("", [s.lower()], [], ""))
    for tg in TAGS:
        out.append(("", tg, [], ""))
    for f in FILES:
        out.append(("", None, f, ""))
    for c in CONTENTS:
        out.append(("", None, [], c))
    return out


def main():
    tmp = tempfile.mkdtemp()
    path = os.path.join(tmp, "Known.txt")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(KNOWN_TXT)
    # (the catalog only: not this machine's learned names)
    no_cache = os.path.join(tmp, "franchise_cache.json")
    km.FranchiseResolver = lambda: FranchiseResolver(cache_path=no_cache)
    cases = []
    seen = set()
    for title, tags, files, content in posts():
        key = json.dumps([title, tags, files, content])
        if key in seen:
            continue
        seen.add(key)
        m = km.KnownManager(path)  # (fresh each time: the resolver learns typos)
        res = m.find_matching_hierarchy(title, tags, files, content)
        cases.append({"title": title, "tags": tags, "files": files, "content": content, "match": list(res) if res else None})
    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump({"known_txt": KNOWN_TXT, "cases": cases}, fh, ensure_ascii=False, indent=1)
    hits = sum(1 for c in cases if c["match"])
    print(f"-> {os.path.relpath(FIXTURE, ROOT)} ({len(cases)} cases, {hits} matched)")


if __name__ == "__main__":
    main()
