#!/usr/bin/env python3
"""Records the README demo and screenshots from the real hopback binary.

    cargo build --release
    python3 docs/demo/record.py            # everything
    python3 docs/demo/record.py stills     # only the screenshots
    python3 docs/demo/record.py gif        # only docs/demo.gif

hopback runs in a private tmux server on the made-up sessions from
docs/demo/setup.sh, in a throwaway HOME under /tmp/hopback-demo (or
HB_DEMO_DIR), with stand-in `claude` and `codex` commands. Each screen is read
back with `tmux capture-pane` and drawn here, so nothing from the real home
directory or terminal is shown.

With HB_DEMO_CLAUDE_SESSION set to the id of a real Claude Code session that
was run in /tmp/hopback-demo/home/code/shop-api, that session takes the place
of the first made-up one and resuming it opens the real Claude Code, so the
film shows where hopback takes you. Look at the frames before publishing them:
they show Claude Code as it is set up on the recording machine. Without the
variable, the stand-in is used.

Needs tmux and Pillow (`pip install pillow`). HB_DEMO_FONT_DIR picks the
folder holding the JetBrains Mono files; Menlo is used when they are missing.
"""

import functools
import math
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

REPO = Path(__file__).resolve().parents[2]
DOCS = REPO / "docs"
COLS, ROWS = 100, 28

# ── Look ────────────────────────────────────────────────────────────────────

BG = (13, 17, 23)
FG = (214, 220, 228)
TITLE_BG = (22, 27, 34)
TITLE_FG = (128, 138, 152)
BORDER = (48, 54, 61)
CURSOR = (224, 150, 70)
LIGHTS = [(255, 95, 87), (254, 188, 46), (40, 200, 64)]
ANSI = [
    (33, 38, 45), (240, 100, 100), (126, 211, 133), (240, 180, 80),
    (110, 170, 255), (200, 150, 250), (118, 214, 222), (201, 209, 217),
    (110, 118, 129), (255, 130, 130), (150, 230, 160), (250, 210, 120),
    (140, 190, 255), (220, 180, 255), (150, 230, 235), (255, 255, 255),
]

FONT_PX = 20  # at 1x: a 12 by 26 cell
CELL_W, CELL_H = 12, 26
PAD = 18
TITLE_H = 40
RADIUS = 12
SHADOW = 44  # room around the window for its shadow, stills only


@functools.lru_cache(maxsize=None)
def _fonts(px):
    folder = Path(os.environ.get("HB_DEMO_FONT_DIR", Path.home() / "Library/Fonts"))
    names = ["Regular", "Bold", "Italic", "BoldItalic"]
    files = [folder / f"JetBrainsMonoNLNerdFontMono-{n}.ttf" for n in names]
    menlo = "/System/Library/Fonts/Menlo.ttc"
    if all(f.exists() for f in files):
        fonts = [ImageFont.truetype(str(f), px) for f in files]
    else:
        fonts = [ImageFont.truetype(menlo, px, index=i) for i in (0, 1, 2, 3)]
    # Tried in turn for a character the main font does not have (★, ↳, ⏵).
    spare = []
    for path in (
        menlo,
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
    ):
        if Path(path).exists():
            spare.append(ImageFont.truetype(path, px))
    ui = None
    for name in ("Inter-Medium.ttf", "Inter-Regular.ttf"):
        if (folder / name).exists():
            ui = ImageFont.truetype(str(folder / name), round(px * 0.68))
            break
    return fonts, spare, ui or fonts[0].font_variant(size=round(px * 0.66))


@functools.lru_cache(maxsize=None)
def _has(font, ch):
    missing = font.getmask("\U0010ffff")
    glyph = font.getmask(ch)
    return glyph.size != missing.size or bytes(glyph) != bytes(missing)


def font_for(ch, bold, italic, px):
    fonts, spare, _ = _fonts(px)
    font = fonts[(1 if bold else 0) + (2 if italic else 0)]
    if ch.isascii() or _has(font, ch):
        return font
    return next((f for f in spare if _has(f, ch)), font)


# ── Reading the screen ──────────────────────────────────────────────────────

SGR = re.compile(r"\x1b\[([0-9;]*)m")
OTHER_ESC = re.compile(r"\x1b(?:\[[0-9;?]*[A-Za-ln-z]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[()][A-Z0-9])")


class Style:
    __slots__ = ("fg", "bg", "bold", "italic", "dim", "underline", "reverse")

    def __init__(self):
        self.fg = self.bg = None
        self.bold = self.italic = self.dim = self.underline = self.reverse = False

    def key(self):
        return (self.fg, self.bg, self.bold, self.italic, self.dim, self.underline, self.reverse)

    def apply(self, params):
        codes = [int(p) if p else 0 for p in params.split(";")] if params else [0]
        i = 0
        while i < len(codes):
            c = codes[i]
            if c == 0:
                self.__init__()
            elif c == 1:
                self.bold = True
            elif c == 2:
                self.dim = True
            elif c == 3:
                self.italic = True
            elif c == 4:
                self.underline = True
            elif c == 7:
                self.reverse = True
            elif c == 22:
                self.bold = self.dim = False
            elif c == 23:
                self.italic = False
            elif c == 24:
                self.underline = False
            elif c == 27:
                self.reverse = False
            elif 30 <= c <= 37:
                self.fg = ANSI[c - 30]
            elif 90 <= c <= 97:
                self.fg = ANSI[c - 90 + 8]
            elif 40 <= c <= 47:
                self.bg = ANSI[c - 40]
            elif 100 <= c <= 107:
                self.bg = ANSI[c - 100 + 8]
            elif c == 39:
                self.fg = None
            elif c == 49:
                self.bg = None
            elif c in (38, 48):
                color, used = None, 0
                if i + 2 < len(codes) and codes[i + 1] == 5:
                    color, used = xterm256(codes[i + 2]), 2
                elif i + 4 < len(codes) and codes[i + 1] == 2:
                    color, used = tuple(codes[i + 2 : i + 5]), 4
                if c == 38:
                    self.fg = color
                else:
                    self.bg = color
                i += used
            i += 1


def xterm256(n):
    if n < 16:
        return ANSI[n]
    if n < 232:
        n -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return (steps[n // 36], steps[n // 6 % 6], steps[n % 6])
    gray = 8 + (n - 232) * 10
    return (gray, gray, gray)


def parse(text):
    """`capture-pane -e` output as rows of (char, style key)."""
    rows, style = [], Style()
    for line in text.split("\n")[:ROWS]:
        line = OTHER_ESC.sub("", line)
        row, pos = [], 0
        for m in SGR.finditer(line):
            row += [(ch, style.key()) for ch in line[pos : m.start()]]
            style.apply(m.group(1))
            pos = m.end()
        row += [(ch, style.key()) for ch in line[pos:]]
        rows.append(row[:COLS])
    return rows


# ── Drawing ─────────────────────────────────────────────────────────────────

# Box-drawing characters are drawn as lines so they always join: for each, the
# sides of the cell the line reaches (left, right, up, down).
BOX = {
    "─": "lr", "│": "ud", "┌": "rd", "┐": "ld", "└": "ru", "┘": "lu",
    "├": "udr", "┤": "udl", "┬": "lrd", "┴": "lru", "┼": "lrud",
    "╭": "rd", "╮": "ld", "╰": "ru", "╯": "lu",
}
ROUND = "╭╮╰╯"


def blend(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def draw_box(d, ch, x, y, s, color):
    w, h = CELL_W * s, CELL_H * s
    t = max(1, round(s * 1.25))
    cx, cy = x + w // 2, y + h // 2
    half = t // 2
    sides = BOX[ch]
    if ch in ROUND:
        r = w // 2
        dx = 1 if "r" in sides else -1
        dy = 1 if "d" in sides else -1
        ox, oy = cx + dx * r, cy + dy * r
        start = {(1, 1): 180, (-1, 1): 270, (1, -1): 90, (-1, -1): 0}[(dx, dy)]
        d.arc([ox - r - half, oy - r - half, ox + r + half, oy + r + half], start, start + 90, fill=color, width=t)
        if dy > 0:
            d.rectangle([cx - half, oy, cx - half + t - 1, y + h - 1], fill=color)
        else:
            d.rectangle([cx - half, y, cx - half + t - 1, oy], fill=color)
        return
    if "l" in sides:
        d.rectangle([x, cy - half, cx + half, cy - half + t - 1], fill=color)
    if "r" in sides:
        d.rectangle([cx - half, cy - half, x + w - 1, cy - half + t - 1], fill=color)
    if "u" in sides:
        d.rectangle([cx - half, y, cx - half + t - 1, cy + half], fill=color)
    if "d" in sides:
        d.rectangle([cx - half, cy - half, cx - half + t - 1, y + h - 1], fill=color)


def render(screen, title, scale, shadow):
    """One screen as an RGBA image, `scale` times the 1x size. Drawn larger and scaled down."""
    rows, cursor = screen
    s = 2 * math.ceil(scale)
    shrink = lambda image: image.resize(
        (round(image.width * scale / s), round(image.height * scale / s)), Image.LANCZOS
    )
    ui = _fonts(FONT_PX * s)[2]
    grid_w, grid_h = COLS * CELL_W * s, ROWS * CELL_H * s
    win_w, win_h = grid_w + 2 * PAD * s, grid_h + (TITLE_H + PAD + PAD // 2) * s
    win = Image.new("RGBA", (win_w, win_h), BG + (255,))
    d = ImageDraw.Draw(win)

    d.rectangle([0, 0, win_w, TITLE_H * s], fill=TITLE_BG)
    d.line([0, TITLE_H * s, win_w, TITLE_H * s], fill=BORDER, width=max(1, s // 2))
    for i, color in enumerate(LIGHTS):
        cx, cy, r = (22 + i * 21) * s, TITLE_H * s // 2, 6 * s
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=color)
    d.text((win_w // 2, TITLE_H * s // 2), title, font=ui, fill=TITLE_FG, anchor="mm")

    ox, oy = PAD * s, (TITLE_H + PAD // 2) * s
    ascent = round(FONT_PX * s * 0.98)
    for r, row in enumerate(rows):
        y = oy + r * CELL_H * s
        for c, (ch, key) in enumerate(row):
            fg, bg, bold, italic, dim, underline, reverse = key
            fg, bg = fg or FG, bg
            if reverse:
                fg, bg = bg or BG, fg
            x = ox + c * CELL_W * s
            if bg:
                d.rectangle([x, y, x + CELL_W * s - 1, y + CELL_H * s - 1], fill=bg)
            if dim:
                fg = blend(fg, bg or BG, 0.4)
            if ch == " ":
                continue
            if ch in BOX:
                draw_box(d, ch, x, y, s, fg)
                continue
            font = font_for(ch, bold, italic, FONT_PX * s)
            d.text((x, y + ascent), ch, font=font, fill=fg, anchor="ls")
            if underline:
                uy = y + ascent + 3 * s
                d.rectangle([x, uy, x + CELL_W * s - 1, uy + s - 1], fill=fg)
    if cursor:
        x, y = ox + cursor[0] * CELL_W * s, oy + cursor[1] * CELL_H * s
        d.rectangle([x, y + 3 * s, x + 2 * s - 1, y + CELL_H * s - 3 * s], fill=CURSOR)

    if not shadow:
        return shrink(win)

    mask = Image.new("L", (win_w, win_h), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, win_w - 1, win_h - 1], RADIUS * s, fill=255)
    edge = Image.new("RGBA", (win_w, win_h), (0, 0, 0, 0))
    ImageDraw.Draw(edge).rounded_rectangle(
        [0, 0, win_w - 1, win_h - 1], RADIUS * s, outline=BORDER + (255,), width=max(1, s // 2)
    )
    win = Image.alpha_composite(win, edge)
    m = SHADOW * s
    canvas = Image.new("RGBA", (win_w + 2 * m, win_h + 2 * m), (0, 0, 0, 0))
    shade = Image.new("L", canvas.size, 0)
    shade.paste(mask.point(lambda v: v * 150 // 255), (m, m + 10 * s))
    shade = shade.filter(ImageFilter.GaussianBlur(14 * s))
    canvas.putalpha(shade)
    canvas.paste(win, (m, m), mask)
    return shrink(canvas)


# ── Driving hopback in tmux ─────────────────────────────────────────────────


class Terminal:
    def __init__(self, demo):
        self.demo = demo
        self.tmux = [shutil.which("tmux") or sys.exit("tmux is not installed"), "-L", f"hopback-demo-{os.getpid()}", "-f", "/dev/null"]
        self.frames = []  # (screen, title, milliseconds)
        self.title = "~/code/shop-api"
        env = {
            "HOME": f"{demo}/home",
            "XDG_CACHE_HOME": f"{demo}/cache",
            "PATH": f"{demo}/bin:{REPO}/target/release:/usr/bin:/bin",
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "LANG": "en_US.UTF-8",
        }
        prompt = r"\[\e[1;38;2;224;150;70m\]~/code/shop-api\[\e[0m\] \[\e[38;2;128;138;152m\]\$\[\e[0m\] "
        shell = f"PS1='{prompt}' bash --norc --noprofile"
        subprocess.run(
            ["env", "-i", *[f"{k}={v}" for k, v in env.items()], *self.tmux, "new-session", "-d",
             "-x", str(COLS), "-y", str(ROWS), "-c", f"{demo}/home/code/shop-api", shell],
            check=True,
        )
        for option in (["status", "off"], ["escape-time", "0"]):
            self.run("set", "-g", *option)
        self.run("set", "-ga", "terminal-features", ",*:RGB")
        self.send("clear", "Enter")
        self.settle()

    def run(self, *args):
        return subprocess.run([*self.tmux, *args], check=True, capture_output=True, text=True).stdout

    def close(self):
        subprocess.run([*self.tmux, "kill-server"], capture_output=True)

    def send(self, *keys, literal=False):
        self.run("send-keys", *(["-l"] if literal else []), *keys)

    def read(self):
        text = self.run("capture-pane", "-p", "-e")
        x, y, shown = self.run("display", "-p", "#{cursor_x} #{cursor_y} #{cursor_flag}").split()
        return parse(text), ((int(x), int(y)) if shown == "1" else None)

    def settle(self, timeout=2.0):
        """Wait until two reads in a row are the same."""
        last, end = self.read(), time.time() + timeout
        while time.time() < end:
            time.sleep(0.06)
            now = self.read()
            if now == last:
                return now
            last = now
        return last

    def wait_for(self, text, timeout=5.0):
        end = time.time() + timeout
        while time.time() < end:
            rows, _ = self.read()
            if any(text in "".join(ch for ch, _ in row) for row in rows):
                return
            time.sleep(0.05)
        sys.exit(f"the screen never showed {text!r}")

    def wait_gone(self, text, timeout=10.0):
        end = time.time() + timeout
        while time.time() < end:
            rows, _ = self.read()
            if not any(text in "".join(ch for ch, _ in row) for row in rows):
                return
            time.sleep(0.05)
        sys.exit(f"the screen still shows {text!r}")

    def shot(self, ms):
        """Add the current screen to the recording, shown for `ms`."""
        screen = self.settle()
        if self.frames and self.frames[-1][:2] == (screen, self.title):
            self.frames[-1] = (screen, self.title, self.frames[-1][2] + ms)
        else:
            self.frames.append((screen, self.title, ms))
        return screen

    def key(self, *keys, ms=700):
        self.send(*keys)
        return self.shot(ms)

    def type(self, text, ms=85, then=900):
        for ch in text:
            self.send(ch, literal=True)
            self.shot(ms)
        self.frames[-1] = (*self.frames[-1][:2], self.frames[-1][2] + then)


MADE_UP_HERO = "5f0c2a9e-1111-4aaa-8bbb-000000000001"
PINNED = "claude:5f0c2a9e-2222-4aaa-8bbb-000000000002"


def demo_home():
    """Builds the demo home. Returns its path and the session the film follows."""
    # A short fixed path, because a translated fork prints the file it wrote.
    demo = os.path.realpath(os.environ.get("HB_DEMO_DIR", "/tmp/hopback-demo"))
    subprocess.run(["sh", str(DOCS / "demo/setup.sh"), demo], check=True, capture_output=True)
    if not (REPO / "target/release/hb").exists():
        sys.exit("target/release/hb is missing: run `cargo build --release` first")
    real = os.environ.get("HB_DEMO_CLAUDE_SESSION")
    if not real:
        return demo, f"claude:{MADE_UP_HERO}"

    # The real session is copied into the demo home for hopback to list, and
    # `claude` there becomes the real program run with the real home, where
    # the session and the sign-in are.
    home = Path.home()
    found = sorted(home.glob(f".claude/projects/*/{real}.jsonl"))
    claude = shutil.which("claude")
    if not found or not claude:
        sys.exit(f"no Claude Code session {real}, or no claude command")
    projects = Path(demo) / "home/.claude/projects"
    for made_up in projects.glob(f"*/{MADE_UP_HERO}.jsonl"):
        made_up.unlink()
    target = projects / found[0].parent.name
    target.mkdir(parents=True, exist_ok=True)
    shutil.copy(found[0], target / found[0].name)
    path = f"{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    wrapper = Path(demo) / "bin/claude"
    wrapper.write_text(
        f"#!/bin/sh\nHOME='{home}' USER='{home.name}' PATH='{path}' exec '{claude}' \"$@\"\n"
    )
    wrapper.chmod(0o755)
    return demo, f"claude:{real}"


def real_claude():
    return bool(os.environ.get("HB_DEMO_CLAUDE_SESSION"))


def record_stills():
    demo, hero = demo_home()
    term = Terminal(demo)
    hb = lambda *args: subprocess.run(
        [REPO / "target/release/hb", *args], check=True, capture_output=True,
        env=dict(os.environ, HOME=f"{demo}/home", XDG_CACHE_HOME=f"{demo}/cache"),
    )
    try:
        # A pin, and a fork made beforehand, so the list shows both marks.
        hb("pin", PINNED)
        hb("fork", "--json", "--to", "codex", hero)

        def save(name, screen, title="hb"):
            image = render(screen, title, scale=1.5, shadow=True)
            image.save(DOCS / name, optimize=True)
            print(f"wrote docs/{name}  {image.width}x{image.height}")

        term.send("hb", "Enter")
        term.settle()
        term.send("Down", "Down")
        save("screenshot.png", term.settle())

        term.send("C-f")
        term.settle()
        term.send("Down")
        save("screen-fork.png", term.settle())
        term.send("Escape")

        term.send("C-o")
        save("screen-actions.png", term.settle())
        term.send("Escape")

        term.send("discount date:week", literal=True)
        save("screen-search.png", term.settle())

        if real_claude():
            # Enter on the Claude Code session: the agent itself, in its folder.
            term.send("C-u")
            term.send("discount agent:claude", literal=True)
            term.settle()
            term.send("Enter")
            term.wait_gone("Sessions")
            time.sleep(6)
            save("screen-resume.png", term.settle(timeout=6), "claude")
            term.send("C-c")
            time.sleep(0.4)
            term.send("C-c")
            time.sleep(1.5)
    finally:
        term.close()
        shutil.rmtree(demo, ignore_errors=True)


def record_gif():
    demo, _ = demo_home()
    term = Terminal(demo)
    try:
        # 1. Every agent's sessions in one list.
        term.shot(700)
        term.type("hb", ms=140, then=350)
        term.title = "hb"
        term.key("Enter", ms=2200)

        # 2. The preview follows the selection.
        term.key("Down", ms=700)
        term.key("Down", ms=900)

        # 3. Tab steps through the agents by writing the filter.
        term.key("Tab", ms=900)
        term.key("Tab", ms=900)
        term.key("Tab", ms=1000)
        term.key("Escape", ms=500)

        # 4. Search as you type, and narrow it with a filter.
        term.type("discount", then=1200)
        term.type(" agent:cl", then=700)
        term.key("Tab", ms=1500)

        # 5. Enter resumes the session with its own agent, in its folder.
        if real_claude():
            term.title = "claude"
            term.send("Enter")
            term.wait_gone("Sessions")
            time.sleep(6)
            term.settle(timeout=6)
            term.shot(5000)
            # Leave Claude Code and clear the screen, off camera.
            term.send("C-c")
            time.sleep(0.4)
            term.send("C-c")
            time.sleep(1.5)
            term.send("clear", "Enter")
            term.title = "~/code/shop-api"
            term.shot(600)
            term.type("hb discount", then=350)
            term.title = "hb"
            term.key("Enter", ms=1500)

        # 6. Fork that session from Claude Code into Codex.
        term.key("C-f", ms=1400)
        term.key("Down", ms=1300)
        term.title = "~/code/shop-api"
        term.send("Enter")
        term.wait_for("stand-in")
        term.shot(3000)

        # 7. The fork is a Codex session now, marked as one.
        term.type("hb agent:codex", ms=75, then=350)
        term.title = "hb"
        term.key("Enter", ms=3500)
    finally:
        term.close()
        shutil.rmtree(demo, ignore_errors=True)

    images = [render(screen, title, scale=1, shadow=False).convert("RGB") for screen, title, _ in term.frames]
    durations = [ms for _, _, ms in term.frames]

    # One palette for the whole film, taken from a sample of its frames.
    sample = images[:: max(1, len(images) // 12)]
    sheet = Image.new("RGB", (images[0].width, images[0].height * len(sample) + 40), BG)
    for i, image in enumerate(sample):
        sheet.paste(image, (0, i * image.height))
    # The window buttons are too small to earn palette entries on their own.
    for i, color in enumerate(LIGHTS):
        sheet.paste(color, (i * 400, sheet.height - 40, (i + 1) * 400, sheet.height))
    palette = sheet.quantize(colors=255, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
    quantized = [image.quantize(palette=palette, dither=Image.Dither.NONE) for image in images]
    out = DOCS / "demo.gif"
    quantized[0].save(
        out, save_all=True, append_images=quantized[1:], duration=durations, loop=0, optimize=False, disposal=1
    )
    total = sum(durations) / 1000
    print(f"wrote docs/demo.gif  {len(images)} frames, {total:.1f} s, {out.stat().st_size // 1024} KB")


if __name__ == "__main__":
    what = sys.argv[1] if len(sys.argv) > 1 else "all"
    if what in ("stills", "all"):
        record_stills()
    if what in ("gif", "all"):
        record_gif()
