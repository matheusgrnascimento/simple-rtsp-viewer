"""Generates assets/icon.ico and assets/icon.png (no third-party packages).

The icon is a blue rounded square with a white video camera and a red
recording dot. Each size is rendered directly (4x4 supersampling) so small
sizes stay sharp.

    python assets/generate_icon.py
"""
import struct
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
SAMPLES = 4

TOP = (0x2B, 0x88, 0xD8)
BOTTOM = (0x00, 0x5A, 0xA6)
WHITE = (0xFF, 0xFF, 0xFF)
RED = (0xFF, 0x4D, 0x4D)


def in_rounded_rect(x, y, x0, y0, x1, y1, r):
    if not (x0 <= x <= x1 and y0 <= y <= y1):
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def in_polygon(x, y, points):
    inside = False
    j = len(points) - 1
    for i, (xi, yi) in enumerate(points):
        xj, yj = points[j]
        if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
            inside = not inside
        j = i
    return inside


def in_circle(x, y, cx, cy, r):
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


LENS = [(0.60, 0.45), (0.84, 0.33), (0.84, 0.67), (0.60, 0.55)]


def shade(x, y):
    """Color and alpha of a point in unit coordinates."""
    if not in_rounded_rect(x, y, 0.03, 0.03, 0.97, 0.97, 0.22):
        return None
    if in_circle(x, y, 0.31, 0.43, 0.045):
        return RED
    if in_rounded_rect(x, y, 0.16, 0.31, 0.62, 0.69, 0.08) or in_polygon(x, y, LENS):
        return WHITE
    t = (y - 0.03) / 0.94
    return tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM))


def render(size):
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = a = 0
            for sy in range(SAMPLES):
                for sx in range(SAMPLES):
                    color = shade((px + (sx + 0.5) / SAMPLES) / size, (py + (sy + 0.5) / SAMPLES) / size)
                    if color:
                        r += color[0]
                        g += color[1]
                        b += color[2]
                        a += 1
            if a:
                row += bytes((r // a, g // a, b // a, round(255 * a / SAMPLES**2)))
            else:
                row += b"\0\0\0\0"
        rows.append(bytes(row))
    return rows


def png(size, rows):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    raw = b"".join(b"\0" + row for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(images):
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for size, blob in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset + len(data))
        data += blob
    return header + entries + data


def main():
    images = [(size, png(size, render(size))) for size in ICO_SIZES]
    (HERE / "icon.ico").write_bytes(ico(images))
    (HERE / "icon.png").write_bytes(dict(images)[256])
    print("wrote icon.ico and icon.png")


if __name__ == "__main__":
    main()
