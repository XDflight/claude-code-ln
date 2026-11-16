#!/usr/bin/env python3
"""Draws the app icon (a 3x3 link matrix) as a 1024px PNG. No dependencies.

Usage: make-icon.py <out.png>
"""
import struct, sys, zlib

N = 1024
BG = (30, 34, 42)
GREEN, AMBER, PURPLE, GRAY = (46, 160, 67), (214, 148, 0), (150, 90, 220), (120, 126, 138)
# (color, filled) per cell, row by row
CELLS = [
    (GREEN, True), (GREEN, True), (GRAY, False),
    (GREEN, True), (GRAY, False), (AMBER, True),
    (GRAY, False), (PURPLE, True), (GREEN, True),
]
MARGIN, RADIUS = 100, 180            # rounded-square plate, per Apple's icon grid
STEP, DOT, RING = 230, 78, 16
ORIGIN = N / 2 - STEP


def coverage(distance):
    """Anti-aliased coverage for a signed distance to an edge (negative = inside)."""
    return max(0.0, min(1.0, 0.5 - distance))


def plate(x, y):
    half = N / 2 - MARGIN
    dx, dy = abs(x - N / 2) - (half - RADIUS), abs(y - N / 2) - (half - RADIUS)
    outside = (max(dx, 0) ** 2 + max(dy, 0) ** 2) ** 0.5 + min(max(dx, dy), 0) - RADIUS
    return coverage(outside)


rows = []
for py in range(N):
    row = bytearray([0])
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
                k = coverage(d - DOT) if filled else coverage(abs(d - DOT + RING / 2) - RING / 2)
                r, g, b = (round(c0 + (c1 - c0) * k) for c0, c1 in zip(BG, color))
        row += bytes((r, g, b, round(alpha * 255)))
    rows.append(bytes(row))


def chunk(tag, data):
    body = tag + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))


png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
