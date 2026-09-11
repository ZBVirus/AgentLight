#!/usr/bin/env python3
"""Generate AgentLight's app icons.

No third-party deps: draws a rounded dark tile with a vertical traffic light
(red / yellow / green) using 4x supersampling, then writes the PNG sizes Tauri
expects plus a PNG-encoded .ico. Run from the repo root:

    python3 scripts/make_icons.py
"""

from __future__ import annotations

import os
import struct
import zlib

BG = (0x1D, 0x20, 0x29)
BORDER = (0x3A, 0x3F, 0x4D)
RED = (0xE0, 0x6C, 0x75)
YELLOW = (0xE5, 0xC0, 0x7B)
GREEN = (0x98, 0xC3, 0x79)

SS = 4  # supersample factor


def blend(dst, src, a):
    return tuple(round(d + (s - d) * a) for d, s in zip(dst, src))


def in_rounded_rect(x, y, size, radius):
    # distance to the corner circles for a rounded square
    cx = min(max(x, radius), size - radius)
    cy = min(max(y, radius), size - radius)
    dx = x - cx
    dy = y - cy
    return dx * dx + dy * dy <= radius * radius


def in_circle(x, y, cx, cy, r):
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def render(size):
    hi = size * SS
    radius = hi * 0.22
    # vertical traffic light: three lamps
    lamp_r = hi * 0.15
    cx = hi / 2
    centers = [(cx, hi * 0.26, RED), (cx, hi * 0.5, YELLOW), (cx, hi * 0.74, GREEN)]

    # build hi-res then downsample
    rows = []
    for py in range(size):
        row = []
        for px in range(size):
            acc = [0.0, 0.0, 0.0]
            n = 0
            for oy in range(SS):
                for ox in range(SS):
                    x = px * SS + ox + 0.5
                    y = py * SS + oy + 0.5
                    if not in_rounded_rect(x, y, hi, radius):
                        color = (0, 0, 0)
                        a = 0.0
                    else:
                        color = BORDER
                        a = 1.0
                        if in_rounded_rect(x, y, hi, radius - hi * 0.03):
                            color = BG
                        for lx, ly, lamp in centers:
                            if in_circle(x, y, lx, ly, lamp_r):
                                color = lamp
                    acc[0] += color[0] * a
                    acc[1] += color[1] * a
                    acc[2] += color[2] * a
                    n += 1
            rows.append(tuple(round(c / n) for c in acc))
    return rows


def png_bytes(size, pixels):
    raw = bytearray()
    for y in range(size):
        raw.append(0)  # filter: none
        for x in range(size):
            r, g, b = pixels[y * size + x]
            raw += bytes((r, g, b, 255))
    compressed = zlib.compress(bytes(raw), 9)

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", compressed)
        + chunk(b"IEND", b"")
    )


def ico_bytes(images):
    # images: list of (size, png_bytes); Vista+ allows PNG-compressed entries
    header = struct.pack("<HHH", 0, 1, len(images))
    entries = b""
    offset = 6 + 16 * len(images)
    payload = b""
    for size, data in images:
        w = 0 if size >= 256 else size
        h = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", w, h, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        payload += data
    return header + entries + payload


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    out = os.path.join(root, "src-tauri", "icons")
    os.makedirs(out, exist_ok=True)

    wanted = [16, 32, 48, 64, 128, 256]
    rendered = {}
    for size in wanted:
        pixels = render(size)
        data = png_bytes(size, pixels)
        rendered[size] = data
        if size in (32, 128, 256):
            name = f"{size}x{size}.png"
            with open(os.path.join(out, name), "wb") as fh:
                fh.write(data)
        print(f"wrote {size}x{size}.png")

    with open(os.path.join(out, "icon.png"), "wb") as fh:
        fh.write(rendered[256])
    with open(os.path.join(out, "128x128@2x.png"), "wb") as fh:
        fh.write(rendered[256])
    with open(os.path.join(out, "icon.ico"), "wb") as fh:
        fh.write(ico_bytes([(s, rendered[s]) for s in wanted]))
    print("wrote icon.png, 128x128@2x.png, icon.ico")


if __name__ == "__main__":
    main()
