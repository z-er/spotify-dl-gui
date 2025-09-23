"""Lightweight HTTP server to accept Spotify links for background downloads."""

from __future__ import annotations

import base64
import html
import json
import threading
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Iterable
from urllib.parse import parse_qs



from .runner import SPOTIFY_URL_RE


class ThreadedHTTPServer(HTTPServer, threading.Thread):
    """HTTPServer running in its own thread."""

    allow_reuse_address = True

    def __init__(self, server_address, handler_cls):
        HTTPServer.__init__(self, server_address, handler_cls)
        threading.Thread.__init__(self, daemon=True)
        self._stop_event = threading.Event()

    def run(self) -> None:  # pragma: no cover - thread loop
        while not self._stop_event.is_set():
            self.handle_request()

    def stop(self) -> None:
        self._stop_event.set()
        try:
            import socket

            host, port = self.server_address
            if host in ('0.0.0.0', '', None):
                host = '127.0.0.1'
            with socket.create_connection((host, port), timeout=0.2):
                pass
        except Exception:
            pass
        self.join(timeout=2)


class WebQueueServer:
    """Minimal HTTP interface for remote queue control."""

    def __init__(
        self,
        main_window,
        host: str,
        port: int,
        username: str = "",
        password: str = "",
        dest_override: str | None = None,
    ) -> None:
        self._main_window = main_window
        self.host = host or "127.0.0.1"
        self.port = int(port or 9753)
        self.username = username or ""
        self.password = password or ""
        self.dest_override = dest_override or ""
        self._server: ThreadedHTTPServer | None = None
        self._expected_auth = None
        if self.username:
            token = base64.b64encode(f"{self.username}:{self.password}".encode()).decode()
            self._expected_auth = f"Basic {token}"

    # ------------------------------------------------------------------
    def start(self) -> tuple[bool, str]:
        if self._server:
            return True, "already running"

        parent = self

        class RequestHandler(BaseHTTPRequestHandler):  # pragma: no cover - network
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args) -> None:  # noqa: N802
                return  # silence console output

            def _check_auth(self) -> bool:
                if not parent._expected_auth:
                    return True
                header = self.headers.get("Authorization", "")
                if header == parent._expected_auth:
                    return True
                self.send_response(HTTPStatus.UNAUTHORIZED)
                self.send_header("WWW-Authenticate", 'Basic realm="spotify-dl"')
                self.end_headers()
                self.wfile.write(b"Authentication required")
                return False

            def do_GET(self):  # noqa: N802
                if not self._check_auth():
                    return
                if self.path.startswith("/status"):
                    payload = parent._collect_status()
                    body = json.dumps(payload).encode()
                    self.send_response(HTTPStatus.OK)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                self._respond_form()

            def do_POST(self):  # noqa: N802
                if not self._check_auth():
                    return
                length = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(length).decode("utf-8", "ignore") if length else ""
                ctype = (self.headers.get("Content-Type") or "").lower()
                if "application/json" in ctype:
                    try:
                        data = json.loads(raw)
                    except Exception:
                        data = {}
                else:
                    data = {k: v[0] if isinstance(v, list) else v for k, v in parse_qs(raw).items()}
                links_blob = str(data.get("links", "")).strip()
                dest = str(data.get("dest", "")).strip() or parent.dest_override
                urls = [u.strip() for u in links_blob.splitlines() if SPOTIFY_URL_RE.match(u.strip())]
                if not urls:
                    self._respond_form(message="No valid Spotify URLs supplied.", success=False)
                    return
                ok, msg = parent.enqueue(urls, dest)
                display_links = '' if ok else '\n'.join(urls)
                self._respond_form(
                    message=msg,
                    success=ok,
                    last_links=display_links,
                    dest=dest,
                )

            def _respond_form(
                self,
                *,
                message: str = "",
                success: bool | None = None,
                last_links: str = "",
                dest: str = "",
            ) -> None:
                body = parent._render_form(
                    message=message,
                    success=success,
                    last_links=last_links,
                    dest=dest,
                )
                body_bytes = body.encode("utf-8")
                self.send_response(HTTPStatus.OK)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body_bytes)))
                self.end_headers()
                self.wfile.write(body_bytes)

        try:
            self._server = ThreadedHTTPServer((self.host, self.port), RequestHandler)
        except OSError as exc:
            return False, str(exc)
        self._server.start()
        return True, f"Serving on http://{self.host}:{self.port}"

    def stop(self) -> None:
        if self._server:
            self._server.stop()
            self._server = None

    # ------------------------------------------------------------------
    def enqueue(self, urls: Iterable[str], dest: str | None) -> tuple[bool, str]:
        urls = [u for u in urls if SPOTIFY_URL_RE.match(u)]
        if not urls:
            return False, "No valid Spotify URLs supplied."

        self._main_window.sig_web_enqueue.emit(list(urls), dest or "")
        return True, f"Queued {len(urls)} link(s)."

    def _collect_status(self) -> dict:
        return self._main_window.get_web_status()

    def _render_form(
        self,
        *,
        message: str,
        success: bool | None,
        last_links: str,
        dest: str,
    ) -> str:
        status = self._collect_status()
        alert = ""
        if message:
            klass = "success" if success else "error"
            alert = f'<p class="{klass}">{html.escape(message)}</p>'
        auth_info = ""
        if self.username:
            auth_info = (
                f"<p>Authentication required for <code>{html.escape(self.username)}</code>.</p>"
            )
        dest_value = html.escape(dest or self.dest_override)
        last_links = html.escape(last_links)
        body = f"""
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8" />
    <title>spotify-dl GUI Remote</title>
    <style>
        body {{ font-family: Arial, sans-serif; background:#0f131a; color:#e6eaf2; padding:30px; }}
        form {{ max-width: 600px; margin: 0 auto; background:#141a22; padding:20px; border-radius:12px; }}
        textarea, input[type=text] {{ width:100%; padding:8px; border-radius:8px; border:1px solid #2a2f39; background:#1a212b; color:#e6eaf2; }}
        button {{ background:#f4a261; border:none; padding:10px 20px; border-radius:8px; cursor:pointer; }}
        button:hover {{ background:#f4c361; }}
        .success {{ color:#8ad7a0; }}
        .error {{ color:#f7768e; }}
        code {{ background:#1a212b; padding:2px 4px; border-radius:4px; }}
        .status {{ margin-top:20px; font-size:14px; color:#b5bcc9; }}
    </style>
</head>
<body>
    <h1>spotify-dl GUI Remote</h1>
    {auth_info}
    {alert}
    <form method="post">
        <label>Spotify links (one per line)</label><br/>
        <textarea name="links" rows="6" required>{last_links}</textarea>
        <label style="margin-top:10px; display:block;">Destination folder override</label>
        <input type="text" name="dest" value="{dest_value}" placeholder="Leave blank to use GUI setting" />
        <button type="submit" style="margin-top:15px;">Queue download</button>
    </form>
    <div class="status">
        <p>Queue size: {status['queue_size']}  Running: {status['is_running']}</p>
        <p>Last run: {html.escape(status['last_run'])}</p>
    </div>
</body>
</html>
        """
        return body

