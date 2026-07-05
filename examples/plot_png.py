"""Tiny stdlib-only PNG helpers for example validation plots."""

from __future__ import annotations

import struct
import zlib


WHITE = (255, 255, 255)
BLACK = (20, 20, 20)
GRAY = (110, 110, 110)
BLUE = (37, 99, 235)
GREEN = (22, 163, 74)
RED = (220, 38, 38)
LIGHT_GREEN = (210, 245, 222)


FONT = {
    " ": ["000", "000", "000", "000", "000", "000", "000"],
    "-": ["000", "000", "000", "111", "000", "000", "000"],
    ".": ["000", "000", "000", "000", "000", "110", "110"],
    "0": ["111", "101", "101", "101", "101", "101", "111"],
    "1": ["010", "110", "010", "010", "010", "010", "111"],
    "2": ["111", "001", "001", "111", "100", "100", "111"],
    "3": ["111", "001", "001", "111", "001", "001", "111"],
    "4": ["101", "101", "101", "111", "001", "001", "001"],
    "5": ["111", "100", "100", "111", "001", "001", "111"],
    "6": ["111", "100", "100", "111", "101", "101", "111"],
    "7": ["111", "001", "001", "010", "010", "010", "010"],
    "8": ["111", "101", "101", "111", "101", "101", "111"],
    "9": ["111", "101", "101", "111", "001", "001", "111"],
    "A": ["010", "101", "101", "111", "101", "101", "101"],
    "B": ["110", "101", "101", "110", "101", "101", "110"],
    "C": ["111", "100", "100", "100", "100", "100", "111"],
    "D": ["110", "101", "101", "101", "101", "101", "110"],
    "E": ["111", "100", "100", "111", "100", "100", "111"],
    "F": ["111", "100", "100", "111", "100", "100", "100"],
    "G": ["111", "100", "100", "101", "101", "101", "111"],
    "H": ["101", "101", "101", "111", "101", "101", "101"],
    "I": ["111", "010", "010", "010", "010", "010", "111"],
    "L": ["100", "100", "100", "100", "100", "100", "111"],
    "M": ["101", "111", "111", "101", "101", "101", "101"],
    "N": ["101", "111", "111", "111", "111", "111", "101"],
    "O": ["111", "101", "101", "101", "101", "101", "111"],
    "P": ["111", "101", "101", "111", "100", "100", "100"],
    "R": ["111", "101", "101", "111", "110", "101", "101"],
    "S": ["111", "100", "100", "111", "001", "001", "111"],
    "T": ["111", "010", "010", "010", "010", "010", "010"],
    "U": ["101", "101", "101", "101", "101", "101", "111"],
    "V": ["101", "101", "101", "101", "101", "101", "010"],
    "W": ["101", "101", "101", "101", "111", "111", "101"],
    "X": ["101", "101", "101", "010", "101", "101", "101"],
    "Y": ["101", "101", "101", "010", "010", "010", "010"],
}


class Canvas:
    def __init__(self, width: int, height: int, color=WHITE) -> None:
        self.width = width
        self.height = height
        self.pixels = bytearray(color * width * height)

    def rect(self, x0: int, y0: int, x1: int, y1: int, color) -> None:
        x0 = max(0, min(self.width, x0))
        x1 = max(0, min(self.width, x1))
        y0 = max(0, min(self.height, y0))
        y1 = max(0, min(self.height, y1))
        for y in range(y0, y1):
            row = y * self.width * 3
            for x in range(x0, x1):
                i = row + x * 3
                self.pixels[i : i + 3] = bytes(color)

    def line(self, x0: int, y0: int, x1: int, y1: int, color) -> None:
        dx = abs(x1 - x0)
        dy = -abs(y1 - y0)
        sx = 1 if x0 < x1 else -1
        sy = 1 if y0 < y1 else -1
        err = dx + dy
        while True:
            self.rect(x0, y0, x0 + 2, y0 + 2, color)
            if x0 == x1 and y0 == y1:
                break
            e2 = 2 * err
            if e2 >= dy:
                err += dy
                x0 += sx
            if e2 <= dx:
                err += dx
                y0 += sy

    def text(self, x: int, y: int, text: str, color=BLACK, scale: int = 2) -> None:
        cursor = x
        for ch in text.upper():
            glyph = FONT.get(ch, FONT[" "])
            for gy, row in enumerate(glyph):
                for gx, bit in enumerate(row):
                    if bit == "1":
                        self.rect(
                            cursor + gx * scale,
                            y + gy * scale,
                            cursor + (gx + 1) * scale,
                            y + (gy + 1) * scale,
                            color,
                        )
            cursor += 4 * scale

    def save(self, path) -> None:
        raw = bytearray()
        stride = self.width * 3
        for y in range(self.height):
            raw.append(0)
            raw.extend(self.pixels[y * stride : (y + 1) * stride])
        with open(path, "wb") as f:
            f.write(b"\x89PNG\r\n\x1a\n")
            self._chunk(f, b"IHDR", struct.pack(">IIBBBBB", self.width, self.height, 8, 2, 0, 0, 0))
            self._chunk(f, b"IDAT", zlib.compress(bytes(raw), 9))
            self._chunk(f, b"IEND", b"")

    @staticmethod
    def _chunk(f, kind: bytes, data: bytes) -> None:
        f.write(struct.pack(">I", len(data)))
        f.write(kind)
        f.write(data)
        f.write(struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))


def y_linear(value: float, vmax: float, top: int, bottom: int) -> int:
    return int(bottom - (value / vmax) * (bottom - top))
