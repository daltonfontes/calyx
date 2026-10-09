"""Records media/check.gif and media/check_dark.gif: `calyx check` refusing
an unsafe refund workflow, the fix, and the check passing.

The outputs are the real ones: the script runs `target/release/calyx` on
examples/readme/ and draws what it prints. Needs Pillow and a built binary:

    cargo build --release
    python3 media/make_check_gif.py
"""

import subprocess
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
DIR = ROOT / "examples" / "readme"
CALYX = ROOT / "target" / "release" / "calyx"
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
FONT_BOLD = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"

COLS, ROWS = 84, 35
SIZE = 15
PAD, BAR = 18, 30

THEMES = {
    "check.gif": {
        "bg": "#ffffff", "bar": "#e9e9ec", "fg": "#24292f", "dim": "#6e7781",
        "prompt": "#0969da", "err": "#cf222e", "warn": "#9a6700", "ok": "#1a7f37",
        "add": "#1a7f37", "del": "#cf222e", "hunk": "#8250df", "frame": "#d0d7de",
    },
    "check_dark.gif": {
        "bg": "#0d1117", "bar": "#161b22", "fg": "#e6edf3", "dim": "#8b949e",
        "prompt": "#58a6ff", "err": "#ff7b72", "warn": "#d29922", "ok": "#3fb950",
        "add": "#3fb950", "del": "#ff7b72", "hunk": "#d2a8ff", "frame": "#30363d",
    },
}


def run(*args):
    p = subprocess.run(args, cwd=DIR, capture_output=True, text=True)
    return (p.stdout + p.stderr).rstrip("\n").split("\n")


def style(line):
    """The color and weight of one output line."""
    if line.startswith("error["):
        return "err", True
    if line.startswith("warning["):
        return "warn", True
    if line.startswith(("- expected", "- observed", "Location")):
        return "dim", False
    if line.startswith("checked in"):
        return "ok", True
    if line.startswith(("+++", "---")):
        return "fg", True
    if line.startswith("+"):
        return "add", False
    if line.startswith("-"):
        return "del", False
    if line.startswith("@@"):
        return "hunk", False
    return "fg", False


class Term:
    def __init__(self):
        self.lines = []          # [(text, color, bold)], a line is a list of spans
        self.frames = []         # (snapshot, cursor, ms)

    def snap(self, ms, cursor=True):
        self.frames.append(([list(l) for l in self.lines[-ROWS:]], cursor, ms))

    def type(self, cmd, comment=False):
        color = "dim" if comment else "fg"
        self.lines.append([("$ ", "prompt", True), ("", color, False)])
        for i in range(0, len(cmd), 2):
            self.lines[-1][1] = (cmd[: i + 2], color, False)
            self.snap(45)
        self.snap(450)

    def out(self, lines, ms=35):
        for line in lines:
            color, bold = style(line)
            # Long lines wrap, as in a terminal.
            for i in range(0, max(len(line), 1), COLS):
                self.lines.append([(line[i : i + COLS], color, bold)])
            self.snap(ms, cursor=False)

    def wait(self, ms):
        # A blinking cursor on a fresh prompt.
        self.lines.append([("$ ", "prompt", True)])
        for i in range(max(1, ms // 500)):
            self.snap(500, cursor=i % 2 == 0)
        self.lines.pop()

    def clear(self):
        self.type("clear")
        self.lines = []
        self.snap(250)


def script():
    t = Term()
    t.snap(500)
    t.type("# a refund workflow, as an agent might write it", comment=True)
    t.type("calyx check refund_unsafe.clyx")
    t.out(run(CALYX, "check", "refund_unsafe.clyx"))
    t.wait(5000)
    t.clear()
    t.type("diff -u refund_unsafe.clyx refund.clyx")
    diff = run("diff", "-u", "refund_unsafe.clyx", "refund.clyx")
    # The header without the files' timestamps.
    diff[0], diff[1] = "--- refund_unsafe.clyx", "+++ refund.clyx"
    t.out(diff, ms=60)
    t.wait(4500)
    t.type("calyx check refund.clyx --time")
    t.out(run(CALYX, "check", "refund.clyx", "--time"))
    t.wait(4000)
    return t.frames


def render(frames, theme, out):
    font = ImageFont.truetype(FONT, SIZE)
    bold = ImageFont.truetype(FONT_BOLD, SIZE)
    cw = font.getbbox("M")[2]
    ch = SIZE + 5
    w, h = PAD * 2 + COLS * cw, BAR + PAD * 2 + ROWS * ch
    images, durations = [], []
    for lines, cursor, ms in frames:
        im = Image.new("RGB", (w, h), theme["bg"])
        d = ImageDraw.Draw(im)
        d.rectangle([0, 0, w, BAR], fill=theme["bar"])
        for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"]):
            d.ellipse([14 + i * 20, 9, 26 + i * 20, 21], fill=c)
        d.text((w // 2, BAR // 2), "calyx check", fill=theme["dim"], font=font, anchor="mm")
        d.rectangle([0, 0, w - 1, h - 1], outline=theme["frame"])
        y = BAR + PAD
        for n, spans in enumerate(lines):
            x = PAD
            for text, color, b in spans:
                text = text[: COLS - (x - PAD) // cw]
                d.text((x, y), text, fill=theme[color], font=bold if b else font)
                x += len(text) * cw
            if cursor and n == len(lines) - 1:
                d.rectangle([x, y + 1, x + cw - 1, y + ch - 2], fill=theme["fg"])
            y += ch
        if images and im.tobytes() == images[-1].tobytes():
            durations[-1] += ms
            continue
        images.append(im)
        durations.append(ms)
    images[0].save(out, save_all=True, append_images=images[1:], duration=durations,
                   loop=0, optimize=True, disposal=1)
    print(f"{out.relative_to(ROOT)}: {len(images)} frames, {sum(durations) / 1000:.1f} s, "
          f"{out.stat().st_size // 1024} KB")


if __name__ == "__main__":
    frames = script()
    for name, theme in THEMES.items():
        render(frames, theme, ROOT / "media" / name)
