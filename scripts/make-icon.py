#!/usr/bin/env python3
"""生成 Prism 应用图标（app-icon.png，1024×1024）。

纯标准库：SDF + 逐像素覆盖度抗锯齿，直接写 PNG（zlib）。
图形与顶栏 brand-mark 同构：棱镜三角 + 入射光 + 三色分光。

用法：python3 scripts/make-icon.py [输出路径]
随后交由 `tauri icon` 生成全套平台图标。
"""

import math
import struct
import sys
import zlib

SIZE = 1024
MARGIN = 24
RADIUS = 235
STROKE = 38
BEAM = 30

# 颜色
BG_TOP = (122, 102, 239)   # #7a66ef
BG_BOTTOM = (83, 64, 204)  # #5340cc
WHITE = (255, 255, 255)
VIOLET = (167, 139, 250)   # #a78bfa
CYAN = (34, 211, 238)      # #22d3ee
PINK = (244, 114, 182)     # #f472b6

# 几何：三角顶点 / 入射线 / 三条分射线
TRIANGLE = [((512, 322), (352, 700)), ((352, 700), (672, 700)), ((672, 700), (512, 322))]
INCOMING = [((170, 511), (446, 511), WHITE)]
OUTGOING = [
    ((578, 511), (866, 340), VIOLET),
    ((578, 511), (884, 511), CYAN),
    ((578, 511), (866, 682), PINK),
]


def clamp(value, low, high):
    return low if value < low else high if value > high else value


def rounded_rect_sdf(x, y):
    """到圆角方形边界的带符号距离（负=内部）。"""
    half = SIZE / 2 - MARGIN
    qx = abs(x - SIZE / 2) - (half - RADIUS)
    qy = abs(y - SIZE / 2) - (half - RADIUS)
    ax = qx if qx > 0 else 0.0
    ay = qy if qy > 0 else 0.0
    inside = qx if qx > qy else qy
    return math.hypot(ax, ay) + (inside if inside < 0 else 0.0) - RADIUS


def segment_sdf(px, py, ax, ay, bx, by):
    vx, vy = bx - ax, by - ay
    wx, wy = px - ax, py - ay
    t = (wx * vx + wy * vy) / (vx * vx + vy * vy)
    t = clamp(t, 0.0, 1.0)
    dx, dy = wx - vx * t, wy - vy * t
    return math.hypot(dx, dy)


def render():
    pixels = bytearray()
    for y in range(SIZE):
        row = bytearray()
        # 背景渐变按行常量
        t = y / (SIZE - 1)
        bg = tuple(
            int(BG_TOP[i] + (BG_BOTTOM[i] - BG_TOP[i]) * t) for i in range(3)
        )
        for x in range(SIZE):
            # 背景：圆角方形
            d = rounded_rect_sdf(x + 0.5, y + 0.5)
            alpha = clamp(0.5 - d, 0.0, 1.0)
            if alpha <= 0.0:
                row += bytes((0, 0, 0, 0))
                continue
            r, g, b = bg
            # 白色三角
            cover = 0.0
            for (ax, ay), (bx, by) in TRIANGLE:
                dist = segment_sdf(x + 0.5, y + 0.5, ax, ay, bx, by)
                cover = max(cover, clamp(0.5 + STROKE / 2 - dist, 0.0, 1.0))
            # 入射线（白）
            for (ax, ay), (bx, by), _ in INCOMING:
                dist = segment_sdf(x + 0.5, y + 0.5, ax, ay, bx, by)
                cover = max(cover, clamp(0.5 + BEAM / 2 - dist, 0.0, 1.0))
            if cover > 0.0:
                mix = cover * 0.97
                r = int(r + (WHITE[0] - r) * mix)
                g = int(g + (WHITE[1] - g) * mix)
                b = int(b + (WHITE[2] - b) * mix)
            # 分光三色
            for (ax, ay), (bx, by), color in OUTGOING:
                dist = segment_sdf(x + 0.5, y + 0.5, ax, ay, bx, by)
                beam_cover = clamp(0.5 + BEAM / 2 - dist, 0.0, 1.0)
                if beam_cover > 0.0:
                    r = int(r + (color[0] - r) * beam_cover)
                    g = int(g + (color[1] - g) * beam_cover)
                    b = int(b + (color[2] - b) * beam_cover)
            row += bytes((r, g, b, int(alpha * 255)))
        pixels += b"\x00" + row  # 每行过滤器 0（None）
    return bytes(pixels)


def png_chunk(tag, data):
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "app-icon.png"
    raw = render()
    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(raw, 9))
        + png_chunk(b"IEND", b"")
    )
    with open(out, "wb") as handle:
        handle.write(png)
    print(f"已生成 {out}（{SIZE}×{SIZE}）")


if __name__ == "__main__":
    main()
