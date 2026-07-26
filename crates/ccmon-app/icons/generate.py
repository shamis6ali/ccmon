#!/usr/bin/env python3
"""Generate ccmon's icons from code, with no image-library dependency.

Run from the repo root:

    python3 crates/ccmon-app/icons/generate.py
    cargo tauri icon crates/ccmon-app/icons/source.png \
        --output crates/ccmon-app/icons

The glyph is a three-row list with the top row highlighted: a set of sessions,
one of which wants you. That is the whole product in one mark.

The tray variant is monochrome with alpha so macOS can treat it as a template
image and tint it for light and dark menu bars.
"""

import struct
import zlib
from pathlib import Path

HERE = Path(__file__).parent

# Supersampling factor for anti-aliasing. 4x is plenty at these sizes.
SS = 4


def write_png(path: Path, width: int, height: int, pixels: bytearray) -> None:
    """Write RGBA8 pixels as a PNG."""
    stride = width * 4
    raw = bytearray()
    for y in range(height):
        raw.append(0)  # filter type 0 (None)
        raw += pixels[y * stride : (y + 1) * stride]

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )
    path.write_bytes(png)
    print(f"wrote {path} ({len(png)} bytes)")


class Canvas:
    def __init__(self, size: int):
        self.size = size
        self.w = size * SS
        self.h = size * SS
        # Float RGBA accumulation at supersampled resolution.
        self.buf = [(0.0, 0.0, 0.0, 0.0)] * (self.w * self.h)

    def _blend(self, i: int, rgba: tuple[float, float, float, float]) -> None:
        sr, sg, sb, sa = rgba
        dr, dg, db, da = self.buf[i]
        out_a = sa + da * (1 - sa)
        if out_a <= 0:
            self.buf[i] = (0.0, 0.0, 0.0, 0.0)
            return
        self.buf[i] = (
            (sr * sa + dr * da * (1 - sa)) / out_a,
            (sg * sa + dg * da * (1 - sa)) / out_a,
            (sb * sa + db * da * (1 - sa)) / out_a,
            out_a,
        )

    def rounded_rect(self, x, y, w, h, r, color) -> None:
        """Coordinates are in final-image units; scaled up internally."""
        x, y, w, h, r = (v * SS for v in (x, y, w, h, r))
        r = min(r, w / 2, h / 2)
        x0, y0, x1, y1 = int(x), int(y), int(x + w + 1), int(y + h + 1)
        for py in range(max(0, y0), min(self.h, y1)):
            for px in range(max(0, x0), min(self.w, x1)):
                cx, cy = px + 0.5, py + 0.5
                # Distance to the rounded rect's inner box.
                dx = max(x + r - cx, 0, cx - (x + w - r))
                dy = max(y + r - cy, 0, cy - (y + h - r))
                if dx * dx + dy * dy <= r * r:
                    self._blend(py * self.w + px, color)

    def circle(self, cx, cy, r, color) -> None:
        self.rounded_rect(cx - r, cy - r, r * 2, r * 2, r, color)

    def to_bytes(self) -> bytearray:
        """Box-downsample the supersampled buffer to the final size."""
        out = bytearray(self.size * self.size * 4)
        n = SS * SS
        for y in range(self.size):
            for x in range(self.size):
                r = g = b = a = 0.0
                for sy in range(SS):
                    row = (y * SS + sy) * self.w + x * SS
                    for sx in range(SS):
                        pr, pg, pb, pa = self.buf[row + sx]
                        r += pr * pa
                        g += pg * pa
                        b += pb * pa
                        a += pa
                i = (y * self.size + x) * 4
                if a > 0:
                    out[i] = min(255, int(r / a * 255 + 0.5))
                    out[i + 1] = min(255, int(g / a * 255 + 0.5))
                    out[i + 2] = min(255, int(b / a * 255 + 0.5))
                out[i + 3] = min(255, int(a / n * 255 + 0.5))
        return out


def rgba(hex_str: str, alpha: float = 1.0):
    hex_str = hex_str.lstrip("#")
    return (
        int(hex_str[0:2], 16) / 255,
        int(hex_str[2:4], 16) / 255,
        int(hex_str[4:6], 16) / 255,
        alpha,
    )


def app_icon(size: int = 1024) -> None:
    """The full-colour app icon: a dark squircle holding the list glyph."""
    c = Canvas(size)
    u = size / 1024  # design units

    c.rounded_rect(72 * u, 72 * u, 880 * u, 880 * u, 200 * u, rgba("1E1E24"))

    rows = [
        (360, "FB923C", "FB923C", 440),  # the one that wants you
        (512, "9A9AA6", "5A5A66", 440),
        (664, "6E6E7A", "44444E", 320),
    ]
    for cy, dot, bar, bar_w in rows:
        c.circle(258 * u, cy * u, 30 * u, rgba(dot))
        c.rounded_rect(322 * u, (cy - 26) * u, bar_w * u, 52 * u, 26 * u, rgba(bar))

    write_png(HERE / "source.png", size, size, c.to_bytes())


def tray_icon(size: int = 64) -> None:
    """Monochrome template image for the menu bar.

    Solid black plus alpha only: macOS inverts template images automatically,
    so this stays legible on light and dark menu bars without shipping two
    assets. The top bar is thicker so the mark still reads at 16pt.
    """
    c = Canvas(size)
    u = size / 64
    black = rgba("000000")

    c.rounded_rect(6 * u, 14 * u, 52 * u, 9 * u, 4.5 * u, black)
    c.rounded_rect(6 * u, 29 * u, 52 * u, 7 * u, 3.5 * u, rgba("000000", 0.75))
    c.rounded_rect(6 * u, 42 * u, 36 * u, 7 * u, 3.5 * u, rgba("000000", 0.55))

    write_png(HERE / "tray.png", size, size, c.to_bytes())
    # A 2x asset keeps the menu bar crisp on Retina displays.
    c2 = Canvas(size * 2)
    u = size * 2 / 64
    c2.rounded_rect(6 * u, 14 * u, 52 * u, 9 * u, 4.5 * u, black)
    c2.rounded_rect(6 * u, 29 * u, 52 * u, 7 * u, 3.5 * u, rgba("000000", 0.75))
    c2.rounded_rect(6 * u, 42 * u, 36 * u, 7 * u, 3.5 * u, rgba("000000", 0.55))
    write_png(HERE / "tray@2x.png", size * 2, size * 2, c2.to_bytes())


if __name__ == "__main__":
    app_icon()
    tray_icon()
