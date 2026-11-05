"""Local media for the desktop UI: thumbnails, full images and video, over loopback HTTP.

The webview can't read the engine's QML image providers, so the sidecar serves the same
pictures here. Every request needs the sidecar's token.

  GET /thumb?path=…&token=…   JPEG thumbnail (the engine's thumbnail cache, Windows shell
                              thumbnails for video)
  GET /image?path=…&token=…   the picture itself; formats a browser can't show (HEIC, TIFF,
                              PSD…) are decoded by the engine and sent as PNG
  GET /file?path=…&token=…    the raw file, with Range support so video can seek
"""
import mimetypes
import os
import re
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from PySide6.QtCore import QBuffer, QByteArray, QIODevice, QSize

WEB_IMAGES = {".jpg", ".jpeg", ".png", ".gif", ".webp", ".avif", ".bmp", ".svg", ".ico", ".jfif"}
CHUNK = 256 * 1024

mimetypes.add_type("video/mp4", ".m4v")
mimetypes.add_type("video/x-matroska", ".mkv")
mimetypes.add_type("image/webp", ".webp")
mimetypes.add_type("image/avif", ".avif")
mimetypes.add_type("audio/flac", ".flac")
mimetypes.add_type("audio/ogg", ".opus")


def _encode(img, fmt: bytes = b"JPG", quality: int = 85) -> bytes:
    data = QByteArray()
    buf = QBuffer(data)
    buf.open(QIODevice.OpenModeFlag.WriteOnly)
    img.save(buf, fmt.decode(), quality)
    buf.close()
    return bytes(data)


class _Handler(BaseHTTPRequestHandler):
    server_version = "PawchiveMedia/1"
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):     # keep the engine log clean
        pass

    def _query(self):
        parsed = urllib.parse.urlparse(self.path)
        return parsed.path, urllib.parse.parse_qs(parsed.query)

    def do_GET(self):  # noqa: N802
        route, q = self._query()
        if (q.get("token") or [""])[0] != self.server.token:
            return self._fail(403, "forbidden")
        path = os.path.normpath((q.get("path") or [""])[0])
        if not path or not os.path.isfile(path):
            return self._fail(404, "not found")
        try:
            if route == "/thumb":
                return self._thumb(path)
            if route == "/image":
                return self._image(path)
            if route == "/file":
                return self._file(path)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            return None
        except Exception as e:  # never take the server down
            return self._fail(500, str(e))
        return self._fail(404, "unknown route")

    def _fail(self, code, text):
        body = text.encode("utf-8")
        try:
            self.send_response(code)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()
            self.wfile.write(body)
        except OSError:
            pass

    def _send_bytes(self, body: bytes, ctype: str, cache=True):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        if cache:
            self.send_header("Cache-Control", "private, max-age=86400")
        self.end_headers()
        self.wfile.write(body)

    def _thumb(self, path):
        img = self.server.thumbs.requestImage(path, QSize(), QSize())
        if img is None or img.isNull() or img.width() <= 1:
            return self._fail(415, "no thumbnail")
        self._send_bytes(_encode(img), "image/jpeg")

    def _image(self, path):
        if os.path.splitext(path)[1].lower() in WEB_IMAGES:
            return self._file(path)
        from bridge.thumbnail_provider import decode_full_image
        img = decode_full_image(path)
        if img is None or img.isNull():
            return self._fail(415, "can't decode")
        self._send_bytes(_encode(img, b"PNG", -1), "image/png")

    def _file(self, path):
        size = os.path.getsize(path)
        ctype = mimetypes.guess_type(path)[0] or "application/octet-stream"
        start, end = 0, size - 1
        rng = self.headers.get("Range")
        partial = False
        if rng:
            m = re.match(r"bytes=(\d*)-(\d*)", rng.strip())
            if m:
                if m.group(1):
                    start = int(m.group(1))
                    if m.group(2):
                        end = min(int(m.group(2)), size - 1)
                elif m.group(2):            # suffix range: the last N bytes
                    start = max(0, size - int(m.group(2)))
                partial = True
        if size == 0 or start > end:
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{size}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        length = end - start + 1
        self.send_response(206 if partial else 200)
        self.send_header("Content-Type", ctype)
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(length))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "private, max-age=3600")
        if partial:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        with open(path, "rb") as f:
            f.seek(start)
            left = length
            while left > 0:
                chunk = f.read(min(CHUNK, left))
                if not chunk:
                    break
                self.wfile.write(chunk)
                left -= len(chunk)


class MediaServer:
    def __init__(self, token: str):
        from bridge.thumbnail_provider import ThumbnailProvider
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
        self.httpd.daemon_threads = True
        self.httpd.token = token
        self.httpd.thumbs = ThumbnailProvider()

    @property
    def port(self) -> int:
        return self.httpd.server_address[1]

    def start(self):
        threading.Thread(target=self.httpd.serve_forever, name="media-http", daemon=True).start()
