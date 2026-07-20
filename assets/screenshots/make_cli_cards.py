#!/usr/bin/env python3
"""Render real tazamun CLI output as branded SVG cards for the README.

SVG rather than PNG on purpose: these are text, so they stay crisp at any zoom
and on any display scale, they weigh a few kilobytes instead of a few hundred,
and a reader can select the commands out of them. The palette is the app's own
(`src/gui_native/theme.rs`), so the cards and the screenshots read as one thing.

The input is captured verbatim from a real run — nothing here is mocked up.
"""

from html import escape

BG = "#0a0f1e"      # theme::BG0
CARD = "#0d1426"    # theme::BG1
EDGE = "#1b2440"
INK = "#e9ecf8"     # theme::INK
DIM = "#8e97b3"     # theme::DIM
FAINT = "#5a6480"   # theme::FAINT
GOLD = "#c8a24b"    # theme::GOLD
GOOD = "#3fbf7f"    # theme::GOOD
BAD = "#e56060"     # theme::BAD
LAPIS = "#5e8bd6"   # theme::LAPIS

FS = 15             # font size
CW = FS * 0.601     # monospace advance width
LH = FS * 1.62      # line height
PAD = 22
TOP = 52            # room for the title strip

MONO = "ui-monospace,SFMono-Regular,Menlo,DejaVu Sans Mono,Consolas,monospace"


def colour(line: str) -> str:
    """The one rule: colour by what the line *means*, never decoratively."""
    s = line.lstrip()
    if s.startswith("$"):
        return GOLD
    if s.startswith("✔"):
        return GOOD
    if s.startswith("✗") or s.startswith("error:"):
        return BAD
    if s.startswith("●"):
        return GOOD
    if s.startswith("•") or s.startswith("○"):
        return DIM
    if s.startswith("#"):
        return FAINT
    return INK


def card(lines: list[str], title: str, out: str) -> None:
    width = max(len(l) for l in lines + [title]) * CW + PAD * 2
    width = max(width, 560)
    height = TOP + len(lines) * LH + PAD

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0f}" '
        f'height="{height:.0f}" viewBox="0 0 {width:.0f} {height:.0f}" '
        f'font-family="{MONO}" font-size="{FS}">',
        f'<rect width="{width:.0f}" height="{height:.0f}" rx="12" fill="{BG}"/>',
        f'<rect x="1" y="1" width="{width - 2:.0f}" height="{height - 2:.0f}" rx="11" '
        f'fill="{CARD}" stroke="{EDGE}"/>',
        # Title strip: a gold rule and the command, in the app's own idiom.
        f'<line x1="{PAD}" y1="36" x2="{width - PAD:.0f}" y2="36" stroke="{EDGE}"/>',
        f'<rect x="{PAD}" y="18" width="3" height="14" rx="1.5" fill="{GOLD}"/>',
        f'<text x="{PAD + 12}" y="30" fill="{DIM}" font-size="{FS - 2}">'
        f"{escape(title)}</text>",
    ]

    for i, line in enumerate(lines):
        y = TOP + i * LH + FS
        if not line.strip():
            continue
        indent = len(line) - len(line.lstrip())
        x = PAD + indent * CW
        parts.append(
            f'<text x="{x:.1f}" y="{y:.1f}" fill="{colour(line)}" '
            f'xml:space="preserve">{escape(line.strip())}</text>'
        )

    parts.append("</svg>")
    with open(out, "w", encoding="utf-8") as fh:
        fh.write("\n".join(parts) + "\n")
    print(out)


REFUSED = """$ tazamun lock components/button.tsx

✗ could not lock components/button.tsx: lease is held by ac133e7591…
  blocked precondition : LEASE
  what to do           : wait for the current holder to unlock or its
                         TTL to expire, or pass --wait
  peers consulted      : ac133e7591 (Good, Direct)
  (use -v for the full per-peer table)""".splitlines()

STATUS = """$ tazamun status

peer id : ac133e7591fce384d60dde89966c7b71ddb0b7f6f07780bb256105e70…
folder  : ~/work/design-system
files   : 5 (540176 bytes)

members (1):
  ● Good   b960b62f03 Direct  0±0ms        Δ5 via LAN

active leases (1):
  components/button.tsx  held by you  expires in 77s

recent events:
  • peer b960b62f03 connected (Direct, rtt 0ms)
  • peer b960b62f03 connected (Relayed, rtt 647ms)""".splitlines()

QUICKSTART = """# on the first machine
$ tazamun init
✔ session created — hand this ticket to a collaborator
  tzm1akqhrtm5lfr2tnri67jm6qmg2jb7euzymomokrxo3slv3vj3urwj4xr4o2…

$ tazamun start
# on the second machine
$ tazamun join tzm1akqhrtm5lfr2tnri67jm6qmg2jb7euzymomokrxo3slv3…
✔ joined — 5 files arriving

$ tazamun lock notes.md
✔ notes.md is now writable (lease TTL 90s, auto-renewed)

$ tazamun unlock notes.md
✔ published 1 change — notes.md is read-only again""".splitlines()

if __name__ == "__main__":
    here = __file__.rsplit("/", 1)[0]
    card(REFUSED, "a refusal that tells you why", f"{here}/cli-refused.svg")
    card(STATUS, "tazamun status", f"{here}/cli-status.svg")
    card(QUICKSTART, "two machines, one folder", f"{here}/cli-quickstart.svg")
