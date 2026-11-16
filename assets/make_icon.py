"""Draw the AIWidget logo: a blue pulse line running out from behind an orange AI sparkle.

Run: py assets/make_icon.py
Writes assets/icon.ico (16-256 px) and assets/icon.png (256 px preview).
"""

import math
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).parent
SS = 8  # supersampling factor

BG = (22, 26, 34, 255)
BLUE = (88, 180, 255)
ORANGE = (240, 128, 84)

# Pulse line, as fractions of the icon size.
PULSE = [(.27, .55), (.45, .55), (.52, .55), (.60, .30), (.70, .76), (.78, .46), (.83, .55), (.88, .55)]


def sparkle(d: ImageDraw.ImageDraw, cx: float, cy: float, r: float) -> None:
    """Four-point star with curved sides."""
    pts = []
    for i in range(240):
        t = 2 * math.pi * i / 240
        c, s = math.cos(t), math.sin(t)
        pts.append((cx + r * math.copysign(abs(c) ** 3.4, c), cy + r * math.copysign(abs(s) ** 3.4, s)))
    d.polygon(pts, fill=ORANGE)


def draw(size: int) -> Image.Image:
    s = size * SS
    im = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    pad = s * (0.02 if size <= 24 else 0.04)
    d.rounded_rectangle((pad, pad, s - pad, s - pad), s * 0.24, fill=BG)

    w = max(SS, int(s * .08))
    d.line([(s * x, s * y) for x, y in PULSE], fill=BLUE, width=w, joint="curve")
    for x, y in (PULSE[0], PULSE[-1]):  # round the ends
        d.ellipse((s * x - w / 2, s * y - w / 2, s * x + w / 2, s * y + w / 2), fill=BLUE)

    # Sparkle on top, covering where the line starts.
    sparkle(d, s * .27, s * .55, s * .19)

    return im.resize((size, size), Image.LANCZOS)


def main() -> None:
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    frames = [draw(n) for n in sizes]
    frames[-1].save(HERE / "icon.png")
    frames[-1].save(HERE / "icon.ico", format="ICO", sizes=[(n, n) for n in sizes], append_images=frames[:-1])


if __name__ == "__main__":
    main()
