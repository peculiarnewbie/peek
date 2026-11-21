import json
import os
import struct
import sys

# Peek's pipe carries a 13-byte little-endian header and a payload.
# Write logs to stderr so stdout carries only frames.
HEADER = struct.Struct("<IBQ")
OPEN, CLOSE, TEXT, ABOUT = 1, 2, 3, 7
label = json.loads(os.environ.get("PEEK_SETTINGS", "{}")).get("label", "Count")
pages = set()
count = 0


def send(kind, channel, message):
    payload = json.dumps(message).encode("utf-8")
    sys.stdout.buffer.write(HEADER.pack(len(payload), kind, channel) + payload)
    sys.stdout.buffer.flush()


def card(place="card"):
    return {"t": "view", "place": place, "view": {"items": [
        {"kind": "text", "text": f"{label}: {count}"},
        {"kind": "button", "label": "Increment", "do": {"t": "increment"}},
    ]}}


def counter():
    return {"t": "counter", "label": label, "count": count}


def receive(kind, channel, payload):
    global count
    if kind == CLOSE:
        pages.discard(channel)
        return
    if kind not in (OPEN, TEXT):
        return
    try:
        message = json.loads(payload)
    except ValueError:
        return
    if not isinstance(message, dict):
        return
    action = message.get("do")
    if not isinstance(action, dict):
        action = {}
    if kind == OPEN:
        if message.get("from") == "page":
            pages.add(channel)
            send(TEXT, channel, counter())
    elif message.get("t") == "view":
        send(TEXT, channel, card(message.get("place", "card")))
    elif message.get("t") == "do" and action.get("t") == "increment":
        count += 1
        for page in pages:
            send(TEXT, page, counter())
        if channel not in pages:
            send(TEXT, channel, card())
    elif message.get("t") == "do" and channel in pages and isinstance(action.get("echo"), str):
        send(TEXT, channel, {"t": "echo", "text": action["echo"][:500]})
    else:
        send(TEXT, channel, {"t": "error", "why": "Unknown request."})


send(ABOUT, 0, {"places": ["card"], "page": "page"})
while True:
    header = sys.stdin.buffer.read(HEADER.size)
    if len(header) < HEADER.size:
        break  # stdin EOF
    size, kind, channel = HEADER.unpack(header)
    payload = sys.stdin.buffer.read(size)
    if len(payload) < size:
        break
    receive(kind, channel, payload)
