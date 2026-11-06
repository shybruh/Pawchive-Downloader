"""
Scheduler screen: the scheduler's settings and the list of schedules (add, edit, switch on/off,
delete).

Exposed to the UI as "scheduler". Running a schedule when it fires (a watchlist sync or a
creator download) is download work and stays with the downloads code in AppBridge.
"""

import json

from PySide6.QtCore import QObject, Property, Signal, Slot


def _scheduler():
    from core.task_scheduler import task_scheduler
    return task_scheduler


class SchedulerBridge(QObject):
    changed = Signal()

    def __init__(self, app_bridge, parent=None):
        super().__init__(parent)
        self._app = app_bridge          # for the settings that affect running downloads

    # ── Settings ────────────────────────────────────────────────────────────
    @Property(bool, notify=changed)
    def enabled(self) -> bool:
        return _scheduler().enabled

    @Property(bool, notify=changed)
    def lockThreadsDelay(self) -> bool:
        return _scheduler().lock_threads_delay

    @Property(bool, notify=changed)
    def nightOwlEnabled(self) -> bool:
        return _scheduler().night_owl_enabled

    @Property(str, notify=changed)
    def nightOwlStart(self) -> str:
        return _scheduler().night_owl_start

    @Property(str, notify=changed)
    def nightOwlEnd(self) -> str:
        return _scheduler().night_owl_end

    @Property(bool, notify=changed)
    def preventSleep(self) -> bool:
        return _scheduler().prevent_sleep

    @Property(bool, notify=changed)
    def sweepRetry(self) -> bool:
        return _scheduler().sweep_retry

    @Slot(bool, bool, bool, str, str, bool, bool)
    def setSettings(self, enabled: bool, lockThreads: bool, nightOwl: bool, start: str, end: str, preventSleep: bool, sweepRetry: bool):
        s = _scheduler()
        s.enabled = enabled
        s.lock_threads_delay = lockThreads
        s.night_owl_enabled = nightOwl
        s.night_owl_start = start
        s.night_owl_end = end
        s.prevent_sleep = preventSleep
        s.sweep_retry = sweepRetry
        s.save()
        self._app.threadsLocked = lockThreads
        self._app._update_sleep_prevention()
        self.changed.emit()

    # ── Schedules ───────────────────────────────────────────────────────────
    @Property(str, notify=changed)
    def schedulesJson(self) -> str:
        return json.dumps(_scheduler().schedules, ensure_ascii=False)

    @Slot(str, str, str, str, int, str, result=str)
    def addTask(self, name: str, targetType: str, targetUrl: str, triggerType: str, intervalHours: int, timeOfDay: str) -> str:
        sid = _scheduler().add_schedule(name, targetType, targetUrl, triggerType, intervalHours, timeOfDay)
        self.changed.emit()
        return sid

    @Slot(str, str, str, str, str, int, str, result=bool)
    def updateTask(self, schedId: str, name: str, targetType: str, targetUrl: str, triggerType: str, intervalHours: int, timeOfDay: str) -> bool:
        ok = _scheduler().update_schedule(schedId, name, targetType, targetUrl, triggerType, intervalHours, timeOfDay)
        if ok:
            self.changed.emit()
        return ok

    @Slot(str, bool, result=bool)
    def toggleTask(self, schedId: str, enabled: bool) -> bool:
        ok = _scheduler().toggle_schedule(schedId, enabled)
        if ok:
            self.changed.emit()
        return ok

    @Slot(str, result=bool)
    def deleteTask(self, schedId: str) -> bool:
        ok = _scheduler().delete_schedule(schedId)
        if ok:
            self.changed.emit()
        return ok
