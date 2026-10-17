"""Logging for the executor: console, rotating files, and optional alerts on errors.

  logs/alm.log     everything at ALM_LOG_LEVEL (default INFO), 5 x 5 MB
  logs/errors.log  WARNING and above, with tracebacks, 5 x 5 MB
Alerts (ERROR and above), all optional:
  ALM_ALERT_WEBHOOK                          generic / Discord / Slack webhook URL (JSON POST)
  ALM_TELEGRAM_BOT_TOKEN + ALM_TELEGRAM_CHAT_ID
The same alert text is sent at most once an hour, so a persistent failure does not flood the channel.
"""
import json
import logging
import logging.handlers
import os
import threading
import time
import urllib.request

from .paths import HOME as ROOT, LOGS as LOG_DIR  # noqa: E402,F401
FMT = "%(asctime)s %(levelname)-7s %(name)s: %(message)s"


class AlertHandler(logging.Handler):
    def __init__(self) -> None:
        super().__init__(level=logging.ERROR)
        self.webhook = os.environ.get("ALM_ALERT_WEBHOOK")
        self.tg_token = os.environ.get("ALM_TELEGRAM_BOT_TOKEN")
        self.tg_chat = os.environ.get("ALM_TELEGRAM_CHAT_ID")
        self.sent: dict = {}
        self.setFormatter(logging.Formatter("%(levelname)s %(name)s: %(message)s"))

    def enabled(self) -> bool:
        return bool(self.webhook or (self.tg_token and self.tg_chat))

    def emit(self, record: logging.LogRecord) -> None:
        text = self.format(record)[:3500]
        key = record.getMessage()[:200]
        if time.time() - self.sent.get(key, 0) < 3600:
            return
        self.sent[key] = time.time()
        threading.Thread(target=self._post, args=(f"[stable-alm] {text}",), daemon=True).start()

    def _post(self, text: str) -> None:
        try:
            if self.webhook:
                body = json.dumps({"content": text, "text": text}).encode()
                req = urllib.request.Request(self.webhook, data=body, headers={"Content-Type": "application/json",
                                                                             "User-Agent": "stable-alm/0.1"})
                urllib.request.urlopen(req, timeout=15).read()
            if self.tg_token and self.tg_chat:
                body = json.dumps({"chat_id": self.tg_chat, "text": text}).encode()
                req = urllib.request.Request(f"https://api.telegram.org/bot{self.tg_token}/sendMessage", data=body,
                                             headers={"Content-Type": "application/json"})
                urllib.request.urlopen(req, timeout=15).read()
        except Exception:  # an alert failure must never take the executor down
            pass


_done = False


def setup_logging() -> logging.Logger:
    global _done
    root = logging.getLogger("alm")
    if _done:
        return root
    _done = True
    os.makedirs(LOG_DIR, exist_ok=True)
    root.setLevel(os.environ.get("ALM_LOG_LEVEL", "INFO").upper())
    root.propagate = False
    console = logging.StreamHandler()
    console.setFormatter(logging.Formatter(FMT))
    root.addHandler(console)
    full = logging.handlers.RotatingFileHandler(os.path.join(LOG_DIR, "alm.log"), maxBytes=5 << 20, backupCount=5,
                                                encoding="utf-8")
    full.setFormatter(logging.Formatter(FMT))
    root.addHandler(full)
    errors = logging.handlers.RotatingFileHandler(os.path.join(LOG_DIR, "errors.log"), maxBytes=5 << 20, backupCount=5,
                                                  encoding="utf-8")
    errors.setLevel(logging.WARNING)
    errors.setFormatter(logging.Formatter(FMT))
    root.addHandler(errors)
    alert = AlertHandler()
    if alert.enabled():
        root.addHandler(alert)
    return root


def get(name: str) -> logging.Logger:
    return logging.getLogger(f"alm.{name}")
