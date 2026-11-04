#!/usr/bin/env python3
"""Generate the hey_buddy icon set (original artwork: rounded square + speech bubble).

Deterministic, no third-party assets. Needs Pillow (`pip install pillow`; dev tool only).
Usage: python3 tools/gen-icons.py   (writes apps/desktop/src-tauri/icons/*)
"""
from pathlib import Path
from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "apps/desktop/src-tauri/icons"
S = 1024
SS = 2  # supersampling


def master() -> Image.Image:
    n = S * SS
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    k = n / 1024
    # Background: rounded square, vertical two-tone gradient built from stacked masks.
    bg = Image.new("RGBA", (n, n))
    bd = ImageDraw.Draw(bg)
    for y in range(n):
        t = y / (n - 1)
        c = (int(52 + 30 * t), int(86 + 40 * t), int(214 - 30 * t), 255)
        bd.line([(0, y), (n, y)], fill=c)
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, n - 1, n - 1], radius=int(224 * k), fill=255)
    img.paste(bg, (0, 0), mask)
    # Speech bubble: rounded body + tail, white.
    white = (255, 255, 255, 255)
    d.rounded_rectangle([184 * k, 232 * k, 840 * k, 668 * k], radius=int(120 * k), fill=white)
    d.polygon([(300 * k, 640 * k), (300 * k, 820 * k), (480 * k, 650 * k)], fill=white)
    # Three dots in the bubble.
    dot = (60, 100, 220, 255)
    for cx in (342, 512, 682):
        r = 46
        d.ellipse([(cx - r) * k, (450 - r) * k, (cx + r) * k, (450 + r) * k], fill=dot)
    return img.resize((S, S), Image.LANCZOS)


def main() -> None:
    m = master()
    OUT.mkdir(parents=True, exist_ok=True)
    m.save(OUT / "icon-1024.png")
    m.resize((512, 512), Image.LANCZOS).save(OUT / "icon.png")
    for name, px in [("32x32.png", 32), ("64x64.png", 64), ("128x128.png", 128), ("128x128@2x.png", 256)]:
        m.resize((px, px), Image.LANCZOS).save(OUT / name)
    for px in (30, 44, 71, 89, 107, 142, 150, 284, 310):
        m.resize((px, px), Image.LANCZOS).save(OUT / f"Square{px}x{px}Logo.png")
    m.resize((50, 50), Image.LANCZOS).save(OUT / "StoreLogo.png")
    m.save(OUT / "icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    m.save(OUT / "icon.icns")
    print("wrote icons to", OUT)


if __name__ == "__main__":
    main()
