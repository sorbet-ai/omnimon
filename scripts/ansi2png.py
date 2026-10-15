"""Render a `tmux capture-pane -e` dump to a PNG (dev tool for screenshots).

    python scripts/ansi2png.py capture.ans out.png [font.ttf]

Block elements are drawn as rectangles so bars look like they do in a real
terminal; everything else uses the font (needs braille glyphs, e.g. DejaVu
Sans Mono).
"""
import re
import sys

from PIL import Image, ImageDraw, ImageFont

BG = (26, 27, 38)
FG = (200, 211, 245)
PALETTE16 = [
    (0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16), (36, 114, 200), (188, 63, 188), (17, 168, 205), (229, 229, 229),
    (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67), (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255),
]


def xterm256(n):
    if n < 16:
        return PALETTE16[n]
    if n < 232:
        n -= 16
        lv = [0, 95, 135, 175, 215, 255]
        return (lv[n // 36], lv[(n // 6) % 6], lv[n % 6])
    v = 8 + 10 * (n - 232)
    return (v, v, v)


def parse(text):
    rows = []
    for line in text.split("\n"):
        cells = []
        fg, bg, bold, italic = None, None, False, False
        i = 0
        while i < len(line):
            m = re.match(r"\x1b\[([0-9;]*)m", line[i:])
            if m:
                codes = [int(c) if c else 0 for c in m.group(1).split(";")]
                j = 0
                while j < len(codes):
                    c = codes[j]
                    if c == 0:
                        fg, bg, bold, italic = None, None, False, False
                    elif c == 1:
                        bold = True
                    elif c == 3:
                        italic = True
                    elif c == 22:
                        bold = False
                    elif c == 23:
                        italic = False
                    elif c in (38, 48) and j + 1 < len(codes):
                        if codes[j + 1] == 2:
                            col = tuple(codes[j + 2 : j + 5])
                            j += 4
                        else:
                            col = xterm256(codes[j + 2])
                            j += 2
                        if c == 38:
                            fg = col
                        else:
                            bg = col
                    elif c == 39:
                        fg = None
                    elif c == 49:
                        bg = None
                    elif 30 <= c <= 37:
                        fg = PALETTE16[c - 30]
                    elif 90 <= c <= 97:
                        fg = PALETTE16[c - 90 + 8]
                    elif 40 <= c <= 47:
                        bg = PALETTE16[c - 40]
                    j += 1
                i += len(m.group(0))
                continue
            cells.append((line[i], fg, bg, bold, italic))
            i += 1
        rows.append(cells)
    while rows and not rows[-1]:
        rows.pop()
    return rows


def main():
    src, out = sys.argv[1], sys.argv[2]
    font_path = sys.argv[3] if len(sys.argv) > 3 else "DejaVuSansMono.ttf"
    size = 16
    font = ImageFont.truetype(font_path, size)
    bold_font = ImageFont.truetype(font_path.replace(".ttf", "-Bold.ttf"), size) if "DejaVu" in font_path else font
    cw = round(font.getlength("M"))
    ch = round(size * 1.22)
    rows = parse(open(src, encoding="utf-8").read())
    w = max(len(r) for r in rows)
    img = Image.new("RGB", (w * cw + 24, len(rows) * ch + 24), BG)
    d = ImageDraw.Draw(img)
    for y, row in enumerate(rows):
        for x, (c, fg, bg, bold, _it) in enumerate(row):
            px, py = 12 + x * cw, 12 + y * ch
            if bg:
                d.rectangle([px, py, px + cw - 1, py + ch - 1], fill=bg)
            col = fg or FG
            o = ord(c)
            if c == " ":
                continue
            if c == "█":
                d.rectangle([px, py, px + cw - 1, py + ch - 1], fill=col)
            elif 0x2589 <= o <= 0x258F:  # left n/8 blocks
                frac = (0x2590 - o) / 8
                d.rectangle([px, py, px + max(1, round(cw * frac)) - 1, py + ch - 1], fill=col)
            elif 0x2581 <= o <= 0x2587:  # lower n/8 blocks
                frac = (o - 0x2580) / 8
                top = py + ch - max(1, round(ch * frac))
                d.rectangle([px, top, px + cw - 1, py + ch - 1], fill=col)
            elif 0x2800 <= o <= 0x28FF:  # braille: draw dots for a crisp look
                bits = o - 0x2800
                dots = [(0, 0, 0x01), (0, 1, 0x02), (0, 2, 0x04), (1, 0, 0x08), (1, 1, 0x10), (1, 2, 0x20), (0, 3, 0x40), (1, 3, 0x80)]
                r = max(1.6, cw / 5.2)
                for dx, dy, b in dots:
                    if bits & b:
                        cx = px + cw * (0.3 + 0.4 * dx)
                        cy = py + ch * (0.14 + 0.24 * dy)
                        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=col)
            else:
                d.text((px, py + 1), c, font=bold_font if bold else font, fill=col)
    img.save(out)


if __name__ == "__main__":
    main()
