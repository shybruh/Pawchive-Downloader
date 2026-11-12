"""The post picker ("Select posts…"): the preview cards of scanned posts, and the posts (with only the
files ticked) to download. The Rust engine does the same for the archive sites
(src-tauri/src/downloader/selection.rs, checked against this with tools/golden_selection.py)."""

import html
import os
import re
from typing import Any, Callable, Dict, List, Optional

from core.filter_engine import FilterEngine

_IMAGE_EXTS = (".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".avif")
_VIDEO_EXTS = (".mp4", ".m4v", ".webm", ".mov", ".mkv")


def norm_rel_key(f_dict: Dict[str, Any]) -> str:
    from core.downloader import FileDownloader
    return FileDownloader.extract_norm_rel_key(f_dict)


def _is_image(p_str: str) -> bool:
    if not p_str:
        return False
    return p_str.lower().split("?")[0].endswith(_IMAGE_EXTS)


def _thumb(path: str, domain: str) -> str:
    clean_p = path if path.startswith("/") else f"/{path}"
    if clean_p.startswith("/data/"):
        clean_p = clean_p[5:]
    return f"https://img.{domain}/thumbnail/data{clean_p}"


def _video_url(fobj, fpath, fname, domain) -> str:
    """A streamable link for a video file, so the picker can show a frame of it."""
    if not (fname.lower().endswith(_VIDEO_EXTS) or fpath.lower().split("?")[0].endswith(_VIDEO_EXTS)
            or str(fobj.get("mimeType") or "").startswith("video/")):
        return ""
    if fobj.get("temp_url"):
        return fobj["temp_url"]
    if fpath.startswith("http"):
        return fpath
    if fobj.get("storageKey") and "cum" in domain:
        ext = os.path.splitext(fname)[1].lower() or ".mp4"
        return f"https://cdn.cum.st/media/{fobj['storageKey']}/original{ext}"
    if fpath and "pawchive" in domain:
        clean = fpath if fpath.startswith("/") else f"/{fpath}"
        if clean.startswith("/data/"):
            clean = clean[5:]
        return f"https://file.pawchive.pw/data{clean}"
    return ""


def _file_thumb(fobj, path, name, domain) -> str:
    if path and (_is_image(path) or _is_image(name)):
        return _thumb(path, domain)
    if fobj.get("storageKey") and ("cum.st" in domain or "cumst" in domain) and (
            _is_image(name) or (fobj.get("mimeType") or "").startswith("image/")):
        ext = os.path.splitext(name)[1].lower() or ".jpg"
        return f"https://cdn.cum.st/media/{fobj.get('storageKey')}/original{ext}"
    return ""


def card(p: Dict[str, Any], domain: str) -> Dict[str, Any]:
    """One post's preview card: title, text, date, thumbnail and its files"""
    pid = str(p.get("id", ""))
    title, _ = FilterEngine.extract_post_title_and_caption(p, fallback_title="Untitled Post")
    published = str(p.get("published") or "")[:10]

    thumb_url = ""
    f = p.get("file")
    if f and isinstance(f, dict):
        fpath = f.get("path") or ""
        fname = f.get("name") or ""
        if fpath and (_is_image(fpath) or _is_image(fname)):
            thumb_url = _thumb(fpath, domain)
    if not thumb_url:
        for att in p.get("attachments", []) or []:
            if isinstance(att, dict) and att.get("path"):
                apath = att["path"]
                aname = att.get("name") or ""
                if _is_image(apath) or _is_image(aname):
                    thumb_url = _thumb(apath, domain)
                    break

    post_files = []
    seen = set()
    # 1. Main post file
    if f and isinstance(f, dict) and (f.get("path") or f.get("storageKey")):
        mf_name = f.get("name") or f.get("originalFilename") or os.path.basename(f.get("path", "") or "main_file")
        mf_path = f.get("path") or ""
        mf_key = norm_rel_key(f) or mf_path.lower()
        if mf_key:
            seen.add(mf_key)
        mf_thumb = _file_thumb(f, mf_path, mf_name, domain)
        # (cum.st files have no path: their storage key names them for picking)
        post_files.append({"name": mf_name, "path": mf_path or f.get("storageKey") or "", "thumbnail": mf_thumb, "previewUrl": mf_thumb,
                           "videoUrl": "" if mf_thumb else _video_url(f, mf_path, mf_name, domain),
                           "is_main": True, "selected": True})

    # 2. Attachments
    for att in p.get("attachments", []) or []:
        if isinstance(att, dict) and (att.get("path") or att.get("storageKey")):
            att_key = norm_rel_key(att) or (att.get("path") or "").lower()
            if att_key and att_key in seen:
                # Identical file already listed from post.file (the API lists it twice)
                att_name = att.get("name") or att.get("originalFilename") or os.path.basename(att.get("path", "") or "")
                if att_name and not att_name.lower().startswith("cover") and not att_name.lower().startswith("preview") and post_files:
                    if post_files[0].get("is_main"):
                        post_files[0]["name"] = att_name
                continue
            if att_key:
                seen.add(att_key)
            att_name = att.get("name") or att.get("originalFilename") or os.path.basename(att.get("path", "") or "attachment")
            att_path = att.get("path") or ""
            att_thumb = _file_thumb(att, att_path, att_name, domain)
            post_files.append({"name": att_name, "path": att_path or att.get("storageKey") or "", "thumbnail": att_thumb, "previewUrl": att_thumb,
                               "videoUrl": "" if att_thumb else _video_url(att, att_path, att_name, domain),
                               "is_main": False, "selected": True})

    # Post text
    raw_content = p.get("content") or p.get("captionHtml") or p.get("caption") or ""
    clean_content = re.sub(r'<br\s*/?>', '\n', str(raw_content), flags=re.IGNORECASE)
    clean_content = html.unescape(re.sub(r'<[^>]+>', '', clean_content).strip())

    return {"id": pid, "title": title, "content": clean_content, "published": published, "thumbnail": thumb_url,
            "fileCount": len(post_files), "files": [dict(pf, postedIndex=i) for i, pf in enumerate(post_files)],
            "selected": True}


def cards(posts: List[Dict[str, Any]], domain: str, stop: Optional[Callable[[], bool]] = None,
          progress: Optional[Callable[[int, int], None]] = None) -> List[Dict[str, Any]]:
    out = []
    total = len(posts)
    domain = domain or "pawchive.pw"
    for idx, p in enumerate(posts, 1):
        if stop is not None and stop():
            break
        if progress is not None and (idx % 100 == 0 or idx == total):
            progress(idx, total)
        out.append(card(p, domain))
    return out


def _norm_path(p) -> str:
    if not p:
        return ""
    s = str(p).replace("\\", "/").strip()
    if s.startswith("/data/"):
        s = s[5:]
    if not s.startswith("/"):
        s = "/" + s
    return s.lower()


def picked(posts: List[Dict[str, Any]], selected_ids, files_map: Dict[str, Any]) -> List[Dict[str, Any]]:
    """The posts ticked in the picker, in scan order; where the files of a post were picked
    ({post id: [file paths]}), only those"""
    selected_id_set = {str(pid) for pid in selected_ids}
    out = []
    for p in posts:
        pid = str(p.get("id", ""))
        if pid not in selected_id_set:
            continue
        raw_allowed = files_map.get(pid) if files_map else None
        if not isinstance(raw_allowed, list):
            out.append(p)
            continue
        allowed_set = set(str(x) for x in raw_allowed)
        allowed_norm = {_norm_path(x) for x in raw_allowed}
        allowed_names = {os.path.basename(str(x)).lower() for x in raw_allowed if x}

        def keep(f_dict):
            if not isinstance(f_dict, dict):
                return False
            fp = str(f_dict.get("path") or f_dict.get("storageKey") or "")
            fn = (f_dict.get("name") or os.path.basename(fp) or "").lower()
            if fp in allowed_set or _norm_path(fp) in allowed_norm:
                return True
            return bool(fn and fn in allowed_names)

        p_copy = dict(p)
        if "attachments" in p and isinstance(p["attachments"], list):
            p_copy["attachments"] = [att for att in p["attachments"] if keep(att)]
        if "file" in p and isinstance(p["file"], dict):
            if not keep(p["file"]):
                p_copy["file"] = {}
        out.append(p_copy)
    return out
