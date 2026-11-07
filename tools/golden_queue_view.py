"""Writes the reference answers of the Python queue model (bridge/queue_model.py) for the Rust
port's test (src-tauri/src/downloader/queue_view.rs).

    python tools/golden_queue_view.py

Output: desktop/src-tauri/tests/fixtures/queue_view.json

A made-up queue goes through the window's operations (filters, pausing / resuming / cancelling /
retrying / removing posts, single files, new files, progress, clearing). After each step the
fixture has the files (as the app sends them to Rust), the view's settings, and what the Python
model shows: its counts, the file list and the per-post groups.
"""

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

from PySide6.QtCore import QCoreApplication, QObject  # noqa: E402

from bridge.queue_model import QueueModel  # noqa: E402
from core.downloader import DownloadTask  # noqa: E402
from services.rust_queue_view import row_of  # noqa: E402

FIXTURE = os.path.join(ROOT, "desktop", "src-tauri", "tests", "fixtures", "queue_view.json")


class Saved:
    """The recovery manager's saved failed files (used when the queue has none)"""
    def retry_counts(self, wait=False):
        return (7, 5)

    def retry_counts_cached(self):
        return (7, 5)


class Parent(QObject):
    def __init__(self):
        super().__init__()
        self.skipRetry404 = False
        self.recovery_manager = Saved()


def T(name, status="pending", size=0, done=0, batch="b1", title="Post one", creator="Alice", service="patreon",
      post="1", err="", http=0, speed="0 KB/s", pct=0, retries=0):
    t = DownloadTask(url=f"https://x/{name}", target_path=f"C:\\dl\\{creator}\\{name}", post_title=title,
                     creator_name=creator, service=service, post_id=post, file_id=f"{post}_{name}",
                     file_size=size, batch_id=batch)
    t.status = status
    t.downloaded_bytes = done
    t.error_msg = err
    t.http_status = http
    t.speed_str = speed
    t.progress_pct = pct
    t.retry_count = retries
    return t


def tasks_a():
    return [
        T("a.jpg", "completed", 1000, 1000),
        T("b.png", "downloading", 1310720, 655360, speed="1.2 MB/s", pct=50),
        T("c.mp4", "pending", 1610612736),
        T("d.zip", "failed", 1023, err="HTTP 404 not found", http=404),
        T("e.gif", "skipped", 1024, 0, batch="b2", title="", creator="Bob", post="2"),
        T("f.jpg", "cancelled", 2048, batch="b2", title="", creator="Bob", post="2", err="Download cancelled by user"),
        T("g.jpg", "paused", 5000, 100, batch="", title="Third", creator="Carol", service="fanbox", post="3"),
        T("h.jpg", "retrying", 0, 0, batch="", title="Third", creator="Carol", service="fanbox", post="3", speed=""),
        T("i.jpg", "pending", 1048575, batch="artist_x", title="Whatever", creator=""),
        T("j.jpg", "failed", 300, batch="b3", title="Fourth", post="4", err="Timed out", http=0, retries=2),
        T("k.jpg", "completed", 0, 0, batch="b3", title="Fourth", post="4"),
        T("l.jpg", "pending", 10, batch="", title="", creator="", service="", post=""),
    ]


def snapshot(name, m, parent, cases):
    m._recalculate_counts()
    m._rebuild_visible()
    g = m._groups_model
    g.rebuild(m._tasks)                     # (worked out at once: no background thread for small queues)
    files = []
    for t in m._visible_tasks:
        idx = m.index(m._visible_tasks.index(t), 0)
        files.append({bytes(r).decode(): m.data(idx, role) for role, r in m.roleNames().items()})
    cases.append({
        "name": name,
        "rows": [row_of(t) for t in m._tasks],
        "state": {"filter_status": m._filter_status, "min_file_size": m._min_file_size,
                  "selected_batch_id": m._selected_batch_id, "paused": sorted(g._paused_batch_ids),
                  "skip_404": parent.skipRetry404, "saved_failed": [7, 5]},
        "want": {
            "props": {"totalCount": m.totalCount, "downloadingCount": m.downloadingCount, "completedCount": m.completedCount,
                      "skippedCount": m.skippedCount, "failedCount": m.failedCount, "pendingCount": m.pendingCount,
                      "count": m.count},
            "files": files,
            "groups": json.loads(json.dumps(g.to_dict_list())),          # (a copy: the next step changes them)
        },
    })


def main():
    app = QCoreApplication.instance() or QCoreApplication(sys.argv)  # noqa: F841
    parent = Parent()
    m = QueueModel(parent)
    cases = []
    snap = lambda name: snapshot(name, m, parent, cases)  # noqa: E731

    m.setTasks(tasks_a())
    snap("all")
    for f in ("downloading", "completed", "skipped", "failed", "pending", "weird"):
        m.filterStatus = f
        snap(f"filter {f}")
    m.filterStatus = "all"
    m.minFileSize = 2000
    snap("min size")
    m.minFileSize = 0
    m.selectedBatchId = "b2"
    snap("one post")
    m.selectedBatchId = "fanbox_Carol_3"
    snap("one post without batch id")
    m.selectedBatchId = ""
    parent.skipRetry404 = True
    snap("skip 404s")
    parent.skipRetry404 = False

    m.pauseBatch("b1")
    snap("pause post")
    m.resumeBatch("b1")
    snap("resume post")
    m.cancelBatch("b1")
    snap("cancel post")
    parent.skipRetry404 = True
    m.retryBatch("b1")
    snap("retry post, skipping 404s")
    parent.skipRetry404 = False
    m.retryBatch("b1")
    snap("retry post")
    m.pauseTask("1_c.mp4")
    snap("pause file")
    m.resumeTask("1_c.mp4")
    snap("resume file")
    m.pauseBatch("fanbox_Carol_3")
    snap("pause post without batch id")
    m.removeBatch("b2")
    snap("remove post")

    added = [T("m.jpg", "pending", 4000, batch="b9", title="New", creator="Dan", post="9"),
             T("a.jpg", "completed", 1000, 1000)]          # (already queued: dropped)
    m.appendTasks(added)
    snap("new files")
    t = m._tasks[-1]
    t.status, t.downloaded_bytes, t.speed_str, t.progress_pct = "downloading", 2000, "3.0 MB/s", 50
    m.updateTask(t)
    snap("progress")
    t.status, t.downloaded_bytes = "completed", 4000
    m.updateTask(t)
    snap("finished")

    m.clearFailedTasks()
    snap("clear failed")
    m.setTasks(tasks_a())
    m.retryFailed()
    snap("retry failed")
    m.setTasks(tasks_a())
    m.retrySelected(["1_d.zip"])
    snap("retry one")
    m.clear()
    snap("empty (saved failed files)")
    parent.skipRetry404 = True
    snap("empty, skipping 404s")

    with open(FIXTURE, "w", encoding="utf-8") as fh:
        json.dump(cases, fh, ensure_ascii=False, indent=1)
    print(f"{len(cases)} cases -> tests/fixtures/queue_view.json")


if __name__ == "__main__":
    main()
