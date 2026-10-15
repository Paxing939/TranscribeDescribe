"""Generate app icons (PNG + ICO) with the standard library only: a red record dot with a white ring."""

import struct
import sys
import zlib
from pathlib import Path


def render(size: int) -> bytes:
    """RGBA pixels: dark rounded square, white ring, red dot."""
    px = bytearray()
    c = (size - 1) / 2
    for y in range(size):
        for x in range(size):
            dx, dy = x - c, y - c
            r = (dx * dx + dy * dy) ** 0.5
            # rounded-square background
            m = size * 0.18
            qx = max(abs(dx) - (c - m), 0)
            qy = max(abs(dy) - (c - m), 0)
            inside_bg = (qx * qx + qy * qy) ** 0.5 <= m + 0.5
            col = (0, 0, 0, 0)
            if inside_bg:
                col = (32, 36, 44, 255)
            if size * 0.30 <= r <= size * 0.40:
                col = (240, 240, 240, 255)
            if r <= size * 0.22:
                col = (226, 54, 54, 255)
            px += bytes(col)
    return bytes(px)


def png(size: int) -> bytes:
    raw = render(size)
    rows = b"".join(b"\x00" + raw[y * size * 4 : (y + 1) * size * 4] for y in range(size))

    def chunk(t: bytes, d: bytes) -> bytes:
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")


def ico(sizes: list[int]) -> bytes:
    images = [png(s) for s in sizes]
    head = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries = b""
    for s, data in zip(sizes, images):
        entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return head + entries + b"".join(images)


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for s in (32, 128):
        (out / f"{s}x{s}.png").write_bytes(png(s))
    (out / "128x128@2x.png").write_bytes(png(256))
    (out / "icon.png").write_bytes(png(512))
    (out / "icon.ico").write_bytes(ico([16, 24, 32, 48, 64, 128, 256]))
    print("icons written to", out)


if __name__ == "__main__":
    main()
