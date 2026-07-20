# tazamun — logo concepts

Three distinct directions for cc1a2b to choose from. **Nothing downstream (icon
embedding, `.ico`/`.icns`, VERSIONINFO, signing) is built until a direction is
picked** — the mark is the owner's aesthetic call.

## Design thesis

tazamun's identity is its **Arabic name** (تزامُن = *synchrony*). The most
ownable, least generic mark is **typographic** — the letterforms themselves,
drawn as vector geometry, encoding what the tool does. A sovereign, Arabic-first
tool should read that way instantly; a generic Western utility could not have
made it. The recurring motif across all three: the **two dots of ت (tā)** become
the **two peer nodes** of the P2P pair.

How the form carries the meaning:

- **P2P** — two nodes (the dots of ت), or two vessels (concept C).
- **sync** — the connective stroke: the Arabic *kashida* (the calligraphic
  elongation that literally means "extension/connection") or the interlock.
- **strict checkout** — an *implicit* lock: a filled disc vs. an open ring
  (exactly one peer holds the lease), or two vessels clasping so only one holds
  at a time. **No literal padlock, no sync-arrows-in-a-circle.**

### Palette (justified, restrained)

One confident primary + one accent + mono:

- **Navy `#0E2A47`** (primary/structure) — sovereign depth and trust; it stands
  for the privacy promise ("no server ever reads your files"). It is the house
  theme's navy.
- **Gold `#C8A24B`** (accent, used sparingly) — the illuminated-manuscript
  tradition of Arabic/Islamic art (gold leaf), and the "value / the lease"
  highlight. Never more than a couple of small gold elements.
- **Mono** — any single ink via `currentColor`; every concept is drawn to
  survive as a flat 1-color silhouette.

No rainbow, no default purple-blue gradient, no default utility-tool look. Flat
color only (a single subtle two-stop navy gradient is the *most* a final could
use, and only if justified).

### Anti-"AI-look" measures applied

- Hand-authored bézier geometry, intentional; asymmetry used where it serves the
  letterform (concept B's lifting vessel; A's bookend balance; C's 180°
  **rotational** — not mirror — symmetry).
- All letterforms are **paths**, no font dependency, no live text.
- Negative space does work (C's interlock/clasp; the counter of the ت vessel).
- Proven legible at **16 px and 128 px** (renders in `previews/`).

> These masters are hand-authored geometric interpretations of the letterforms
> (the source of truth SVGs). A native calligrapher or a proper Arabic type tool
> can refine the curves further — noted per direction below.

## Direction A — calligraphic wordmark (bridge ت → ن)

`concept-a.svg` · previews `previews/concept-a-{16,128}.png`

A compact horizontal wordmark. Arabic reads right-to-left, so the word's
**bookend letters** are featured, large and balanced — ت (tā, right) and ن
(nūn, left) — joined by a short **kashida**.

- **Form → meaning:** the two gold dots of ت are the two peers; the kashida
  between the letters is the sync link / the shared file carried across; the
  deep bowl of ن is the single converged session, its one navy dot the unified
  result. Two peers, bridged, resolving to one — the whole sync story in a line.
- **Ownable because:** it is unmistakably Arabic letterforms treated as a
  wordmark, not a Latin logo with Arabic bolted on.
- **Use:** a horizontal lockup (README header, docs); its square-icon companion
  is the ت monogram (direction B).
- **Iterate option:** render the *full* five-letter word تزامُن (best with a
  broad-nib / Arabic type tool for authentic joins) — this is the roughest of
  the three as pure hand-geometry.

## Direction B — ت monogram (app icon)

`concept-b.svg` · previews `previews/concept-b-{16,128}.png`

The single letter **ت** as a self-contained icon-mark.

- **Form → meaning:** a deep broad-nib **vessel** (a boat, not a shallow smile)
  is the shared folder the peers sync into; its right tip **lifts higher** than
  the left for direction (not mirror-symmetry). The two dots sit on a slight
  diagonal so they read as **nodes, not eyes** — a filled gold **disc** holds
  the lease, an open **ring** waits: the strict exclusive checkout, exactly one
  at a time, drawn without a padlock.
- **Ownable because:** a single Arabic letter as an app icon is instantly
  sovereign and hard to confuse with a generic utility glyph.
- **Use:** the app/taskbar/favicon icon; reads at 16 px as a navy vessel + gold
  node.

## Direction C — the handoff / interlock

`concept-c.svg` · previews `previews/concept-c-{16,128}.png`

Two identical vessel-glyphs (from the نـ / ت bowl) in **180° rotational**
symmetry — equal peers facing opposite ways — clasping in the centre.

- **Form → meaning:** the two hooks **interlock in the negative space** = the
  exclusive lease **handoff** (only one holds at a time); each vessel carries one
  node dot (navy / gold) — the two peers — and the colours cross so neither peer
  "owns" the mark. The loop of sync is *implied* by the interlock, not drawn as a
  circle or arrows.
- **Ownable because:** rotational (not reflective) symmetry reads as motion and
  exchange; it is specific and dynamic, the opposite of a static generic badge.
- **Use:** a strong standalone icon; the boldest, most conceptual of the three.

## Rendering method

No `rsvg-convert`/`inkscape`/`resvg` in this environment, so previews are
rasterised with **cairosvg** (present at `~/.local/bin/cairosvg`); mono 16 px
silhouettes are the same SVG with the gold swapped to navy. When a direction is
chosen, the full PNG ladder (16→512) will be rendered the same way.
