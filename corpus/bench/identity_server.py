#!/usr/bin/env python3
"""OpenAI-compatible translator that returns the source text unchanged.

BabelDOC's classic prompt ends with ``Input:\\n\\n`` plus the paragraph.
PDFMathTranslate and RapidPdfTrans send different bodies. This server returns
the text after the last ``Input:`` marker when it sees one, and otherwise
returns the last user message. Placeholders, numbers, and URLs are therefore
copied. Point every engine at this process so the benchmark shares one translator.

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
    marker = "Input:\n\n"
    if marker in user:
        return user.rsplit(marker, 1)[1].strip()
    return user.strip()


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
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": text},
                    "finish_reason": "stop",
                }
            ],
        }
        data = json.dumps(payload).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

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
