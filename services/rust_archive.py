"""
The download archive's lookups and records answered by the desktop shell's Rust port
(desktop/src-tauri/src/archive/): "was this file downloaded before?", recording a downloaded
file and recording a post's link. ArchiveManager calls these; the rest of the archive (the Archive
tab, maintenance, repair) is still Python. Both use the same database file.

When the archive closes its database (to repair, replace or clear it), Rust lets go of it too and
waits until it's open again.
"""

from services import rust_queue

FAILED = object()


def available() -> bool:
    return rust_queue.available()


def call(path: str, op: str, **args):
    """Rust's answer, or FAILED (then the Python code answers)."""
    if not available():
        return FAILED
    link = rust_queue.shared_link()
    if not link.connect():
        return FAILED
    args.update(op=op, path=path)
    ok, value = link.call("archive", args)
    return value if ok else FAILED
