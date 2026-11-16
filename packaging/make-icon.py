#!/usr/bin/env python3
"""Draws the app icon (a 3x3 link matrix). No dependencies.

Usage: make-icon.py <out> [size=1024]
The extension picks the format: .png, .ico (a PNG inside an ICO container, for Windows)
or .rgba (raw 8-bit RGBA, embedded in the binary as the window icon).
"""
import struct, sys, zlib

N = int(sys.argv[2]) if len(sys.argv) > 2 else 1024
S = N / 1024
BG = (30, 34, 42)
GREEN, AMBER, PURPLE, GRAY = (46, 160, 67), (214, 148, 0), (150, 90, 220), (120, 126, 138)
# (color, filled) per cell, row by row
CELLS = [
    (GREEN, True), (GREEN, True), (GRAY, False),
    (GREEN, True), (GRAY, False), (AMBER, True),
    (GRAY, False), (PURPLE, True), (GREEN, True),
]
MARGIN, RADIUS = 100 * S, 180 * S    # rounded-square plate, per Apple's icon grid
STEP, DOT, RING = 230 * S, 78 * S, 16 * S
ORIGIN = N / 2 - STEP


def coverage(distance):
    """Anti-aliased coverage for a signed distance to an edge (negative = inside)."""
    return max(0.0, min(1.0, 0.5 - distance))


def plate(x, y):
    half = N / 2 - MARGIN
    dx, dy = abs(x - N / 2) - (half - RADIUS), abs(y - N / 2) - (half - RADIUS)
    outside = (max(dx, 0) ** 2 + max(dy, 0) ** 2) ** 0.5 + min(max(dx, dy), 0) - RADIUS
    return coverage(outside)


pixels = bytearray()
for py in range(N):
    y = py + 0.5
    for px in range(N):
        x = px + 0.5
        alpha = plate(x, y)
        r, g, b = BG
        if alpha > 0:
            col, line = round((x - ORIGIN) / STEP), round((y - ORIGIN) / STEP)
            if 0 <= col < 3 and 0 <= line < 3:
                color, filled = CELLS[line * 3 + col]
                d = ((x - ORIGIN - col * STEP) ** 2 + (y - ORIGIN - line * STEP) ** 2) ** 0.5
                d /= S  # anti-alias in output pixels, measure in 1024-grid units
                k = (coverage((d - 78) * S) if filled
                     else coverage((abs(d - 78 + 8) - 8) * S))
                r, g, b = (round(c0 + (c1 - c0) * k) for c0, c1 in zip(BG, color))
        pixels += bytes((r, g, b, round(alpha * 255)))


def chunk(tag, data):
    body = tag + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))


rows = b"".join(b"\0" + bytes(pixels[y * N * 4:(y + 1) * N * 4]) for y in range(N))
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")

out = sys.argv[1]
if out.endswith(".rgba"):
    data = bytes(pixels)
elif out.endswith(".ico"):
    side = N if N < 256 else 0  # 0 means 256 in an ICO directory entry
    data = struct.pack("<HHH", 0, 1, 1) + struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(png), 22) + png
else:
    data = png
open(out, "wb").write(data)
