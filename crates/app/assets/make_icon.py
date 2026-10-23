"""Generates Sonora's app icon (PNG + multi-size ICO) using only the stdlib:
a coral sun sending out two sound waves, on Sonora's dark rounded square."""
import math
import struct
import sys
import zlib
from pathlib import Path

SS = 4  # supersampling per axis, for smooth edges
BG = (0x16, 0x19, 0x20)
SUN = (0xFF, 0x7A, 0x59)  # theme::ACCENT
WAVES = [((0xFF, 0xA0, 0x6E), 0.33), ((0xFB, 0xBF, 0x24), 0.47)]  # (color, radius)
WAVE_WIDTH = 0.075
WAVE_HALF_ANGLE = math.radians(48)
CENTER = (0.36, 0.50)
SUN_RADIUS = 0.17


def in_round_rect(x, y, r):
    cx = min(max(x, r), 1 - r)
    cy = min(max(y, r), 1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def color_at(x, y):
    """Color (or None) at a point of the unit square."""
    if not in_round_rect(x, y, 0.22):
        return None
    dx, dy = x - CENTER[0], y - CENTER[1]
    dist = math.hypot(dx, dy)
    if dist <= SUN_RADIUS:
        return SUN
    angle = math.atan2(dy, dx)
    for color, radius in WAVES:
        if abs(dist - radius) <= WAVE_WIDTH / 2 and abs(angle) <= WAVE_HALF_ANGLE:
            return color
        # Round ends on each wave.
        for end in (WAVE_HALF_ANGLE, -WAVE_HALF_ANGLE):
            ex = CENTER[0] + radius * math.cos(end)
            ey = CENTER[1] + radius * math.sin(end)
            if (x - ex) ** 2 + (y - ey) ** 2 <= (WAVE_WIDTH / 2) ** 2:
                return color
    return BG


def render(size):
    n = size * SS
    out = bytearray()
    for y in range(size):
        out.append(0)  # PNG filter type: none
        for x in range(size):
            acc = [0.0, 0.0, 0.0]
            covered = 0
            for sy in range(SS):
                for sx in range(SS):
                    c = color_at((x * SS + sx + 0.5) / n, (y * SS + sy + 0.5) / n)
                    if c is not None:
                        covered += 1
                        for i in range(3):
                            acc[i] += c[i]
            if covered:
                rgb = [round(acc[i] / covered) for i in range(3)]
                alpha = round(255 * covered / (SS * SS))
            else:
                rgb, alpha = [0, 0, 0], 0
            out += bytes((*rgb, alpha))
    return png(size, bytes(out))


def png(size, raw):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(images):
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, blobs = b"", b""
    for size, data in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    return header + entries + blobs


def main(out_dir):
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    images = [(s, render(s)) for s in (16, 24, 32, 48, 64, 128, 256)]
    (out / "icon.png").write_bytes(images[-1][1])
    (out / "icon.ico").write_bytes(ico(images))
    print("wrote", out / "icon.png", out / "icon.ico")


if __name__ == "__main__":
    main(sys.argv[1])
