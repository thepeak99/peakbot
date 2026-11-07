#!/usr/bin/env python3
"""Dev helper (plan D11): evaluate JS in the app's WebView over Chrome DevTools Protocol.

usage: cdp.py <port> '<js expression>'   (prints the JSON result)
       cdp.py <port> --url                (prints the current page URL)

Needs `adb forward tcp:<port> localabstract:webview_devtools_remote_<pid>` and a
debuggable WebView (Tauri debug builds enable it).
"""
import json
import sys
import urllib.request

import websocket  # python3-websocket


def page(port):
    pages = json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/json"))
    pages = [p for p in pages if p.get("type") == "page"]
    if not pages:
        sys.exit("no page target")
    return pages[0]


def evaluate(port, expr):
    # suppress_origin: Chrome's devtools socket rejects foreign Origin headers.
    ws = websocket.create_connection(page(port)["webSocketDebuggerUrl"], timeout=20,
                                     suppress_origin=True)
    ws.send(json.dumps({"id": 1, "method": "Runtime.evaluate", "params": {
        "expression": expr, "awaitPromise": True, "returnByValue": True}}))
    while True:
        msg = json.loads(ws.recv())
        if msg.get("id") == 1:
            ws.close()
            res = msg.get("result", {})
            if "exceptionDetails" in res:
                return {"exception": res["exceptionDetails"].get("text"),
                        "detail": res.get("result", {}).get("description")}
            return res.get("result", {}).get("value")


if __name__ == "__main__":
    port, arg = sys.argv[1], sys.argv[2]
    if arg == "--url":
        print(page(port)["url"])
    else:
        print(json.dumps(evaluate(port, arg)))
