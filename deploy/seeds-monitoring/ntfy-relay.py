#!/usr/bin/env python3
"""Alertmanager -> ntfy formatter relay.

Receives Alertmanager webhook payloads on 127.0.0.1:9094 and republishes
them to an ntfy topic as human-readable push messages.
"""
import json
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

NTFY_URL = "https://ntfy.sh"
TOPIC = "vtorrent-seeds-4254e0588837"


def publish(alerts, firing):
    if not alerts:
        return
    names = sorted({a.get("labels", {}).get("alertname", "?") for a in alerts})
    lines = [
        "{} @ {}: {}".format(
            a.get("labels", {}).get("alertname", "?"),
            a.get("labels", {}).get("instance", "?"),
            a.get("annotations", {}).get("summary", ""),
        )
        for a in alerts
    ]
    icon = "\U0001F525" if firing else "\u2705"
    word = "FIRING" if firing else "RESOLVED"
    body = json.dumps(
        {
            "topic": TOPIC,
            "title": "{} {}: {}".format(icon, word, ", ".join(names)),
            "message": "\n".join(lines),
            "tags": ["rotating_light" if firing else "white_check_mark"],
            "priority": 4 if firing else 3,
        }
    ).encode()
    req = urllib.request.Request(NTFY_URL, data=body, headers={"Content-Type": "application/json"})
    urllib.request.urlopen(req, timeout=10).read()


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            length = int(self.headers.get("Content-Length", 0))
            payload = json.loads(self.rfile.read(length) or b"{}")
            alerts = payload.get("alerts", [])
            publish([a for a in alerts if a.get("status") == "firing"], True)
            publish([a for a in alerts if a.get("status") != "firing"], False)
            self.send_response(200)
        except Exception:
            import traceback; traceback.print_exc()
            self.send_response(500)
        finally:
            self.end_headers()

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", 9094), Handler).serve_forever()
