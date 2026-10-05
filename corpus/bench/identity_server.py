#!/usr/bin/env python3
"""OpenAI-compatible translator that returns the source text unchanged.

Three request shapes are echoed so RapidPdfTrans, BabelDOC, and
PDFMathTranslate can share this process:

* RapidPdfTrans posts ``{"segments":[{"id","text",...}]}`` and expects
  ``{"translations":[{"id","text"}]}``.
* BabelDOC's paragraph batch ends with ``## Here is the input:`` and a JSON
  array of ``{"id","input"}``. The reply is ``[{"id","output"}]`` with the
  input copied into ``output``.
* The classic one-paragraph prompt ends with ``Input:\\n\\n`` plus the text.
  That tail is returned as-is.

    python3 corpus/bench/identity_server.py --port 8765

No API key is required. The server does not call a network model.
"""

from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def identity_text(messages: list[dict]) -> str:
    user = ""
    for message in messages:
        if message.get("role") == "user":
            content = message.get("content") or ""
            if isinstance(content, list):
                content = "".join(
                    part.get("text", "") for part in content if isinstance(part, dict)
                )
            user = str(content)
    echoed = echo_rpt_batch(user)
    if echoed is not None:
        return echoed
    echoed = echo_babeldoc_batch(user)
    if echoed is not None:
        return echoed
    marker = "Input:\n\n"
    if marker in user:
        return user.rsplit(marker, 1)[1].strip()
    return user.strip()


def echo_rpt_batch(user: str) -> str | None:
    """RapidPdfTrans sends ``{"segments":[{"id","text",...}]}`` and expects JSON back."""
    stripped = user.strip()
    if not stripped.startswith("{") or '"segments"' not in stripped[:4000]:
        return None
    try:
        payload = json.loads(stripped)
    except json.JSONDecodeError:
        return None
    segments = payload.get("segments") if isinstance(payload, dict) else None
    if not isinstance(segments, list):
        return None
    translations = []
    for segment in segments:
        if not isinstance(segment, dict) or "id" not in segment:
            continue
        translations.append({"id": segment["id"], "text": segment.get("text") or ""})
    return json.dumps({"translations": translations}, ensure_ascii=False)


def echo_babeldoc_batch(user: str) -> str | None:
    """BabelDOC's LLM prompt ends with a JSON array of ``{"id","input"}``."""
    marker = "## Here is the input:"
    if marker not in user:
        return None
    tail = user.rsplit(marker, 1)[1].strip()
    start = tail.find("[")
    if start < 0:
        return None
    try:
        items, _end = json.JSONDecoder().raw_decode(tail[start:])
    except json.JSONDecodeError:
        return None
    if not isinstance(items, list):
        return None
    outputs = []
    for item in items:
        if not isinstance(item, dict) or "id" not in item:
            continue
        text = item.get("input")
        if not isinstance(text, str):
            text = item.get("text") if isinstance(item.get("text"), str) else ""
        outputs.append({"id": item["id"], "output": text})
    if not outputs:
        return None
    return json.dumps(outputs, ensure_ascii=False)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw.decode("utf-8") or "{}")
        except json.JSONDecodeError:
            self.send_error(400, "invalid json")
            return
        text = identity_text(body.get("messages") or [])
        payload = {
            "id": "rpt-identity",
            "object": "chat.completion",
            "model": body.get("model") or "identity",
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": text},
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2,
            },
        }
        data = json.dumps(payload).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802
        if self.path.rstrip("/").endswith("/models"):
            payload = {
                "object": "list",
                "data": [{"id": "identity", "object": "model", "owned_by": "rpt"}],
            }
            data = json.dumps(payload).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self.send_error(404, "not found")

    def log_message(self, fmt: str, *args) -> None:
        return


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"identity translator on http://127.0.0.1:{args.port}/v1", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
