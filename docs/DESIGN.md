# Neria design system: Studio + Play

Chosen 2026-11-10 from the mockups in `design/download-directions.html` (direction D). This is
the target for the UI foundation work; new screens follow it from now on.

## The one rule

**Calm where you read and watch, playful where you touch.**

- **Touch = playful.** Anything pressable (buttons, chips, toggles, row actions, the logo) gets
  a 2px outline; the main action on a screen also gets a hard offset shadow and sticker yellow.
  It should be obvious at a glance what can be clicked.
- **Read = calm.** Queue rows, progress, stats, timelines, settings text: hairline dividers, no
  cards, muted colours, monospace numbers. Easy to watch for an hour.
- **Status = soft tags.** Saved / Gone / Check match are small tinted labels, never outlines or
  filled buttons, so green, amber and red keep their meaning.

If something is both (a queue row you can click), the row stays calm and its actions are the
playful part.

## What we stopped doing (the "vibecoded" tells)

- Purple-on-blue-grey defaults. Neria's colour is sticker yellow, used sparingly.
- A rounded bordered card around every block. Lists are rows with hairlines; a card only when
  something really floats (dialogs, the drawer, the toast).
- One font at one weight. Three faces with jobs (below).
- Icons in tinted circles, pill chips, a giant centred "Nothing here yet".

## Colour tokens

| Token | Dark | Light | Use |
| --- | --- | --- | --- |
| `bg` | `#141416` | `#f4f0e6` | Window |
| `panel` | `#1b1b1e` | `#fffdf8` | Inputs, drawer, dialogs |
| `line` | `#26262a` | `#e2dccd` | Hairlines, meter track |
| `line-strong` | `#34343a` | `#cfc7b4` | Control outlines |
| `ink` | `#f1efe9` | `#151412` | Text, selected chip fill |
| `muted` | `#8e8c88` | `#6d685e` | Secondary text |
| `dim` | `#5f5d5a` | `#9a9486` | Hints, inactive nav |
| `pop` | `#ffd23f` | `#ffd23f` | Main action, progress, counts |
| `on-pop` | `#1a1405` | `#1a1405` | Text on `pop` |
| `pop-shadow` | `#9a6f00` | `#151412` | Hard shadow under the main action (deep ochre: same family as the yellow) |
| `hard` | `#000000` | `#151412` | Outline of `pop` controls |
| `ok` / `ok-bg` | `#7fd6a4` / `#173326` | `#1d7a4b` / `#d5f2e0` | Saved, done |
| `warn` / `warn-bg` | `#f5b14a` / `#3a2a12` | `#9a5b00` / `#fbe7c4` | Check match, retryable problems |
| `bad` / `bad-bg` | `#ff7b72` / `#3a1a1a` | `#c2372e` / `#fde0dc` | Failed, can't download |

Yellow is for the one main action per screen and for "things are moving" (progress, counts).
Never for decoration.

## Type

| Face | Job | Sizes |
| --- | --- | --- |
| Space Grotesk 700 | Page headlines, buttons, tags, section titles | 30 / 16 / 14 / 11 px |
| Inter Tight 400–600 | Everything else | 17 (preview title) / 14 / 13 / 12 px |
| JetBrains Mono 400–500 | Numbers: sizes, speeds, timecodes, versions, shortcuts | 11.5 px |

All three are SIL OFL; bundle the `.ttf` files and register them with Slint (`import
"fonts/….ttf";`) so nothing depends on what's installed. Headlines are short and friendly
("Grab anything."), sentence case, no trailing period except on that kind of one-liner.

## Shape and spacing

- Radius: 9–12px on controls and thumbnails, 6px on tags and small thumbnails. No pills.
- Outlines: 2px on controls, 1px hairlines for structure.
- Hard shadow: 3px 3px, no blur, only on the main action (and the logo mark).
- Spacing scale: 4, 8, 12, 16, 22, 26px. Page padding 22–26px. Queue rows 60px.
- Thumbnails lead: link previews at 184×104, queue rows 64×36.

## Components

- **Primary button**: `pop` fill, 2px `hard` outline, `pop-shadow` offset, Space Grotesk 14.
  Presses down 2px (shadow shrinks) on click. One per screen.
- **Secondary / row action**: 1.5–2px `line-strong` outline, no fill, `muted` text.
- **Chip**: 2px `line-strong` outline, 9px radius; selected = `ink` fill with `bg` text.
- **Input**: `panel` fill, 2px `line-strong` outline, 50px tall; the primary button can sit inside.
- **Queue row**: no card; 1px `line` under it, thumbnail, title (Inter Tight 14/500), a status
  line (tag + mono stats), a 6px rounded `pop` meter while active, actions on the right.
- **Tag**: tinted `*-bg` with `*` text, Space Grotesk 11/600, 6px radius.
- **Count badge**: `pop` fill, mono 11px ("1 going").
- **Keyboard hint**: mono 11px, 1px outline with a 2px bottom edge.
- **Nav**: 80px rail, icon with a small label (Slint has no tooltips, so no bare icons);
  active item gets a `line-strong` outline on `panel`.
- **Quality row**: chips under the preview title when the window is wide enough, otherwise in
  their own row; ends with the format button ("mp4 · H.264") that opens Settings.
- **Text link**: quiet secondary actions (Clear finished, Open folder): muted text, ink on hover.
- **Lists**: rows keep a 16px lane on the right for the scrollbar; dividers stop before it.
- **Narrow windows**: secondary row actions shrink to icons; preview thumbnail shrinks on short
  windows; long option lists scroll while the main action stays pinned.
- **Toast / dialog / drawer**: the only floating cards: `panel`, 1px `line-strong`, 12px radius.
  Dialogs use the shared `Dialog` (scrim, Space Grotesk 20 title, optional muted line, ×);
  lists inside are calm rows with hairlines, never cards per row.
- **Tick**: 20px box, 2px `line-strong` outline, 6px radius; ticked = `ink` fill with a `bg`
  check. The label is part of the target.
- **Banner** (new version and the like): a calm strip with a tag, one line of text, ghost
  actions and a hairline under it, so the page keeps its one yellow action.

## Themes

Dark and light share every rule; only the tokens change. Default: follow Windows' app theme,
with a manual override in Settings.
