"""Convert a truecolor ANSI terminal frame (stdin) to an SVG (stdout).

Box-drawing characters are drawn as lines so borders join up whatever
font the viewer has. Used by render.sh for the README screenshots.
"""
import html
import re
import sys

CW, CH, PAD = 8.4, 17.0, 12
FONT = "ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace"
DEFAULT_FG, DEFAULT_BG = "#d0d0d0", "#1b1d23"
BASIC = ["#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5"]
BRIGHT = ["#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff"]


def color256(n):
    if n < 8:
        return BASIC[n]
    if n < 16:
        return BRIGHT[n - 8]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[n // 36], steps[n // 6 % 6], steps[n % 6])
    v = 8 + (n - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


def apply_sgr(params, st):
    i = 0
    while i < len(params):
        p = params[i]
        if p == 0:
            st.update(fg=None, bg=None, bold=False, rev=False, ul=False, dim=False, it=False, strike=False)
        elif p == 1:
            st["bold"] = True
        elif p == 2:
            st["dim"] = True
        elif p == 3:
            st["it"] = True
        elif p == 4:
            st["ul"] = True
        elif p == 7:
            st["rev"] = True
        elif p == 9:
            st["strike"] = True
        elif p == 22:
            st["bold"] = st["dim"] = False
        elif p == 23:
            st["it"] = False
        elif p == 24:
            st["ul"] = False
        elif p == 27:
            st["rev"] = False
        elif p == 29:
            st["strike"] = False
        elif p in (38, 48):
            key = "fg" if p == 38 else "bg"
            if params[i + 1] == 2:
                st[key] = "#%02x%02x%02x" % tuple(params[i + 2:i + 5])
                i += 4
            else:
                st[key] = color256(params[i + 2])
                i += 2
        elif p == 39:
            st["fg"] = None
        elif p == 49:
            st["bg"] = None
        elif 30 <= p <= 37:
            st["fg"] = BASIC[p - 30]
        elif 40 <= p <= 47:
            st["bg"] = BASIC[p - 40]
        elif 90 <= p <= 97:
            st["fg"] = BRIGHT[p - 90]
        elif 100 <= p <= 107:
            st["bg"] = BRIGHT[p - 100]
        i += 1


# Box-drawing characters: which of (up, right, down, left) each one connects.
BOX = {"─": "0101", "│": "1010", "┌": "0110", "┐": "0011", "└": "1100", "┘": "1001",
       "├": "1110", "┤": "1011", "┬": "0111", "┴": "1101", "┼": "1111"}


def box_lines(ch, x0, y0, color):
    cx, cy = x0 + CW / 2, y0 + CH / 2
    ends = [(cx, y0), (x0 + CW, cy), (cx, y0 + CH), (x0, cy)]
    segs = [f'M{cx:.1f} {cy:.1f}L{ex:.1f} {ey:.1f}' for on, (ex, ey) in zip(BOX[ch], ends) if on == "1"]
    return f'<path d="{"".join(segs)}" stroke="{color}" stroke-width="1.2" fill="none" stroke-linecap="butt"/>'


def cells(text):
    rows = []
    st = dict(fg=None, bg=None, bold=False, rev=False, ul=False, dim=False, it=False, strike=False)
    for line in text.split("\n"):
        row = []
        for tok in re.split(r"(\x1b\[[0-9;]*m)", line):
            if tok.startswith("\x1b["):
                apply_sgr([int(x) if x else 0 for x in tok[2:-1].split(";")], st)
                continue
            for ch in tok:
                if ch == "\r":
                    continue
                row.append((ch, dict(st)))
        rows.append(row)
    while rows and not rows[-1]:
        rows.pop()
    return rows


def main():
    rows = cells(sys.stdin.read())
    cols = max(len(r) for r in rows)
    w, h = cols * CW + 2 * PAD, len(rows) * CH + 2 * PAD
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0f}" height="{h:.0f}" viewBox="0 0 {w:.0f} {h:.0f}">',
           f'<rect width="100%" height="100%" rx="8" fill="{DEFAULT_BG}"/>',
           f'<g font-family="{FONT}" font-size="14px" xml:space="preserve">']
    for y, row in enumerate(rows):
        top = PAD + y * CH
        for x, (ch, st) in enumerate(row):
            fg, bg = st["fg"] or DEFAULT_FG, st["bg"]
            if st["rev"]:
                fg, bg = (bg or DEFAULT_BG), fg
            if bg:
                out.append(f'<rect x="{PAD + x * CW:.1f}" y="{top:.1f}" width="{CW + 0.3:.1f}" height="{CH + 0.3:.1f}" fill="{bg}"/>')
    for y, row in enumerate(rows):
        top = PAD + y * CH
        x = 0
        while x < len(row):
            ch, st = row[x]
            if ch in BOX:
                fg = (st["bg"] or DEFAULT_BG) if st["rev"] else (st["fg"] or DEFAULT_FG)
                out.append(box_lines(ch, PAD + x * CW, top, fg))
                x += 1
                continue
            start, chars = x, []
            while x < len(row) and row[x][1] == st and row[x][0] not in BOX:
                chars.append(row[x][0])
                x += 1
            text = "".join(chars)
            if not text.strip():
                continue
            fg = (st["bg"] or DEFAULT_BG) if st["rev"] else (st["fg"] or DEFAULT_FG)
            attrs = f'fill="{fg}"'
            if st["bold"]:
                attrs += ' font-weight="bold"'
            if st["dim"]:
                attrs += ' opacity="0.6"'
            if st["it"]:
                attrs += ' font-style="italic"'
            decorations = [d for d, on in (("underline", st["ul"]), ("line-through", st["strike"])) if on]
            if decorations:
                attrs += f' text-decoration="{" ".join(decorations)}"'
            out.append(f'<text x="{PAD + start * CW:.1f}" y="{top + CH * 0.78:.1f}" {attrs} textLength="{len(text) * CW:.1f}" lengthAdjust="spacingAndGlyphs">{html.escape(text)}</text>')
    out.append("</g></svg>")
    print("\n".join(out))


main()
