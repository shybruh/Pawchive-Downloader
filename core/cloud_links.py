"""Recognising cloud-storage links (MEGA, Google Drive, Dropbox, GoFile).

These are pages, not files: they're downloaded by services/cloud_downloader.py, never by the
normal file downloader. Queue rows for them are recognised by their URL.
"""

PLATFORM_NAMES = {"mega": "MEGA", "gdrive": "Google Drive", "dropbox": "Dropbox", "gofile": "GoFile"}


def cloud_platform(url: str) -> str:
    """'mega', 'gdrive', 'dropbox' or 'gofile' for a link the cloud downloader handles, else ''."""
    u = (url or "").lower()
    if "mega.nz" in u or "mega.co.nz" in u or "mega.io" in u:
        return "mega"
    if "drive.google.com" in u or "docs.google.com" in u or "drive.usercontent.google.com" in u:
        return "gdrive"
    if "dropbox.com" in u:
        return "dropbox"
    if "gofile.io" in u:
        return "gofile"
    return ""
