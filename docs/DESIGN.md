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
| `muted` | `#8e8c88` | `#6b665c` | Secondary text, hints, placeholders (4.5:1+ everywhere) |
| `dim` | `#6e6c68` | `#878276` | Icons, inactive nav, disabled labels (3:1+; never body text) |
| `edge` | `#6d6c6e` | `#888376` | An outline that alone shows a control (empty tick box), 3:1+ |
| `focus` | ink | ink | Keyboard focus ring |
| `pop` | `#ffd23f` | `#ffd23f` | Main action, progress, counts |
| `on-pop` | `#1a1405` | `#1a1405` | Text on `pop` |
| `pop-shadow` | `#9a6f00` | `#151412` | Hard shadow under the main action (deep ochre: same family as the yellow) |
| `hard` | `#000000` | `#151412` | Outline of `pop` controls |
| `ok` / `ok-bg` | `#7fd6a4` / `#173326` | `#1c7649` / `#d5f2e0` | Saved, done |
| `warn` / `warn-bg` | `#f5b14a` / `#3a2a12` | `#975900` / `#fbe7c4` | Unsure match, retryable problems |
| `bad` / `bad-bg` | `#ff7b72` / `#3a1a1a` | `#ba352c` / `#fde0dc` | Failed, sounds different, can't download |

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
  `GhostButton` at its regular size is exactly as tall as the primary button (a 41px face
  at the top of the same 44px slot, where the yellow one keeps its shadow), so a pair like
  Cancel + Download lines up. `compact` (30px) is for rows and strips, never beside a
  primary button.
- **Chip**: 2px `line-strong` outline, 9px radius; selected = `ink` fill with `bg` text.
- **Input**: `panel` fill, 2px `line-strong` outline, 50px tall; the primary button can sit inside.
- **Queue row**: no card; 1px `line` under it, thumbnail, title (Inter Tight 14/500), a status
  line (tag + mono stats), a 6px rounded `pop` meter while active, actions on the right.
  Paused rows keep their meter in `muted`.
- **Group card** (playlist/post/album): a queue row with a stacked thumbnail, title at 650, a
  summary ("3 of 12 saved · 1 failed · now: …"), whole-group actions and a chevron; open by
  default. Its rows hang under it, indented 28px off a 2px `line` guide.
- **Tag**: tinted `*-bg` with `*` text, Space Grotesk 11/600, 6px radius. Flagged songs:
  "Unsure" in `warn`, "Sounds different" in `bad`.
- **Filter tabs** (queue): calm words with a mono count, `ink` with a 2px underline when
  picked; the "Needs a look" count turns `warn` when it isn't 0. In the header row on wide
  windows, on a line of their own below it. Not chips: they change the view, not a setting.
- **Count badge**: `pop` fill, mono 11px ("1 going").
- **Keyboard hint**: mono 11px, 1px outline with a 2px bottom edge.
- **Nav**: 80px rail (bottom tabs on compact windows, see below), icon with a small label
  (Slint has no tooltips, so no bare icons);
  active item gets a `line-strong` outline on `panel`.
- **Quality row**: chips under the preview title when the window is wide enough, otherwise in
  their own row; ends with the format button ("mp4 · H.264") that opens Settings.
- **Text link**: quiet secondary actions (Clear finished, Open folder): muted text, ink on hover.
- **Lists**: rows keep a 24px lane on the right (`Theme.lane`: the 14px scrollbar plus a 10px
  gap) so the scrollbar never touches a button; dividers stop before it.
- **Window widths** (`Screen` in `theme.slint`; mocks in `design/narrow-layouts.html`):
  - *Wide* (1000px+): everything side by side; quality chips under the preview title.
  - *Medium* (720–999): rail stays; chips get their own row; row actions Clip / Compress / Pause
    shrink to icons; the Settings drawer leaves a strip of the page showing.
  - *Compact* (under 720, down to the 400 × 600 minimum; also the starting point for Android):
    bottom tab bar (Download, Clip, Compress, Settings) and the logo next to the headline;
    page padding 16px; queue rows keep one main action (Pause / Resume / Retry / Pick match /
    Show / Cancel) and put the rest in a ⋯ menu (`Menu` / `MenuItem`); Settings is a full
    sheet with the sections as words along the top; Clip stacks (In / Out on their own row, no
    shortcut line, Export across the bottom); Compress puts each label above its chips and
    Compress across the bottom; header buttons go icon-only; dialogs go nearly edge to edge.
    The link box keeps its whole width for the link: Paste becomes the box's icon and
    Download (whose label grows: "Download 13 songs") gets its own full-width row under it.
    Any row of a text field plus buttons does the same: the text it holds comes first.
  - Chip rows (`ChoiceChips`) wrap onto more lines instead of clipping, at every width.
- **Narrow windows**: secondary row actions shrink to icons; preview thumbnail shrinks on short
  windows; long option lists scroll while the main action stays pinned.
- **Toast / dialog / drawer**: the only floating cards: `panel`, 1px `line-strong`, 12px radius.
  Dialogs use the shared `Dialog` (scrim, Space Grotesk 20 title, optional muted line, ×);
  lists inside are calm rows with hairlines, never cards per row.
- **Keyboard focus**: every pressable control uses `KeyPress` (Tab reaches it, Enter/Space
  presses it): a 2px `focus` (ink) ring, 4px outside the control, only while focused from the
  keyboard. Dialogs and the drawer close on Esc.
- **Tick**: 20px box, 2px `line-strong` outline, 6px radius; ticked = `ink` fill with a `bg`
  check. The label is part of the target.
- **Banner** (new version and the like): a calm strip with a tag, one line of text, ghost
  actions and a hairline under it, so the page keeps its one yellow action.

## Adding a screen or a control

So the next feature doesn't undo the foundation. Most of this is automatic if you build from
the shared pieces; the rest is checked.

1. **Build from `widgets.slint`.** Buttons, chips, ticks, tabs, menu rows already handle the
   keyboard (`KeyPress`), screen readers (role and label) and both themes. A new kind of
   control goes into `widgets.slint` with the same three, not into a page.
2. **Colours only from `theme.slint`**, with a dark and a light value. Text uses `ink` or
   `muted`; `dim` is for icons and disabled labels.
3. **Widths: let layouts size things.** Read `Screen.compact` / `Screen.wide` for what changes
   per width (see "Window widths"); never a fixed width over 360px; rows of chips use
   `ChoiceChips` (they wrap). Wrapping text must sit in layouts only: a `Rectangle` between
   it and its layout stops it from measuring its height.
4. **Icon-only buttons get a `label`** (what a screen reader says).
5. **Check it:** `cargo test -p neria --test ui_rules` (colours, keyboard, labels, widths,
   contrast; fails with what to do), then `scripts/ui-check.ps1` for screenshots of every page
   and dialog at 1280 / 900 / 720 / 400 wide. Look at every shot for what the user has to
   read or press, at 400 first: a field squeezed by its buttons, a label cut off, a tab off
   the edge. Add the new screen to the script's list (a dev
   hook in `main.rs` that opens it, like `NERIA_DIALOG`).

## Themes

Dark and light share every rule; only the tokens change. Default: follow Windows' app theme,
with a manual override in Settings.
