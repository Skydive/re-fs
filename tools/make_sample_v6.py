"""Write a synthetic reMarkable v6 .rm page (made-up strokes) for re-render's tests.

Usage: python3 tools/make_sample_v6.py crates/re-render/tests/data/sample-v6.rm

It holds an unrelated block (skipped by the parser), five ballpoint strokes in
the top part of the page (the first with 41 points) and one text highlight.
"""
import math
import struct
import sys

def varuint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        out.append(b | (0x80 if n else 0))
        if not n:
            return bytes(out)

def tag(index, ty):
    return varuint((index << 4) | ty)

ID, LEN4, BYTE8, BYTE4 = 0xF, 0xC, 0x8, 0x4

def crdt_id(index, a, b):
    return tag(index, ID) + bytes([a]) + varuint(b)

def u32(index, v):
    return tag(index, BYTE4) + struct.pack("<I", v)

def f32(index, v):
    return tag(index, BYTE4) + struct.pack("<f", v)

def f64(index, v):
    return tag(index, BYTE8) + struct.pack("<d", v)

def sub(index, body):
    return tag(index, LEN4) + struct.pack("<I", len(body)) + body

def block(ty, body, version=2):
    return struct.pack("<I", len(body)) + bytes([0, 1, version, ty]) + body

def item(n, value):
    # parent, item id, left, right, deleted length, value
    return crdt_id(1, 0, 11) + crdt_id(2, 1, n) + crdt_id(3, 0, 0) + crdt_id(4, 0, 0) + u32(5, 0) + sub(6, value)

X0 = 702.0  # v6 stores x relative to the page's centre line

def stroke(n, pts):
    data = b"".join(struct.pack("<ffHHBB", x - X0, y, 0, 12, 0, 128) for x, y in pts)
    value = bytes([3]) + u32(1, 15) + u32(2, 0) + f64(3, 2.0) + f32(4, 0.0) + sub(5, data)
    return block(0x05, item(n, value))

def highlight(n, text, rect):
    t = text.encode()
    value = bytes([1]) + u32(2, 0) + u32(3, len(t)) + u32(4, 3)
    value += sub(5, varuint(len(t)) + bytes([1]) + t)
    x, y, w, h = rect
    value += sub(6, varuint(1) + struct.pack("<dddd", x - X0, y, w, h))
    return block(0x03, item(n, value), version=1)

out = bytearray(b"reMarkable .lines file, version=6".ljust(43, b" "))
out += block(0x09, varuint(1) + sub(0, b"\x10" + bytes(16)), version=1)  # author ids: skipped
# A wavy line of 41 points, then four more strokes further right and lower.
out += stroke(20, [(200 + 5 * i, 260 + 25 * math.sin(i / 4)) for i in range(41)])
for k in range(4):
    out += stroke(30 + k, [(420 + 130 * k + 4 * i, 300 + 15 * k + 20 * math.cos(i / 3)) for i in range(30)])
out += highlight(50, "a sample highlight", (250.0, 330.0, 300.0, 40.0))
open(sys.argv[1], "wb").write(out)
print(f"{sys.argv[1]}: {len(out)} bytes")
