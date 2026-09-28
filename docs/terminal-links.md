# Terminal links (OSC 8 + plain URLs)

## What it does

Links in a terminal pane are clickable.

- Hover a link: it gets a thin underline.
- Hold **Ctrl**: the pointer turns into a hand.
- **Ctrl+Click**: the link opens in your default browser.
- Hover an **OSC 8** link: a tooltip shows where it really goes (the
  visible text can say anything).

Two kinds of link are found:

1. **OSC 8 hyperlinks.** A program prints `ESC ] 8 ; ; URI ESC \ text ESC ] 8 ; ; ESC \`.
   alacritty stores the link on each cell. The link is the run of cells
   carrying the same link.
2. **Plain URLs.** Text starting `http://`, `https://`, `ftp://` or `mailto:`.
   Detection follows soft wraps (alacritty's `WRAPLINE` flag), so a long URL
   the terminal wrapped is one link. Trailing `.`, `,`, `)` etc. are trimmed the
   way prose expects: `(see https://x.io/a_(b)).` keeps `https://x.io/a_(b)`.

## Why it exists

Agents print URLs all the time (PRs, docs, dashboards). Copy-selecting them
by hand was the gap flagged in the competitor review.

## Gotchas

- **Only http/https/ftp/mailto ever open.** `file:` is refused on purpose: the
  Windows shell *runs* a `file:///…/x.exe` link, and OSC 8 lets any program
  hide that behind innocent text. Unopenable links are not underlined, so an
  underline always means "Ctrl+Click will work".
- **Ctrl+Click beats app mouse mode.** Inside a mouse-reporting TUI (vim with
  `mouse=a`, htop…) Ctrl+Click on a link opens it instead of being sent to
  the app. Ctrl+Click *off* a link still goes to the app as before.
- **The press owns the button until release.** After opening, local
  selection is suppressed through the release frame, so the click can't also
  clear or start a selection. Keyed on the physical button, so a release
  missed while unfocused can't leave it stuck.
- **A URL an app wraps itself is two links.** Only terminal soft-wraps join
  rows. TUIs that hard-wrap text inside a box (newline + padding) break the
  URL; nothing in the grid says those rows belong together.
- **Box-drawing glyphs end a URL**, so `│https://x.io│` in a TUI frame works.
- Hover lookup is memoized on (content_gen, scroll offset, grid size, cell) — a
  still pointer over a quiet pane does no work.

## Key files

- `src/hyperlink.rs` — pure detection: `link_at`, `find_urls`, `openable`,
  `viewport_spans`. All tests are `Term<VoidListener>` fixtures.
- `src/terminal.rs` — `Session::link_under` (memoized lookup),
  Ctrl+Click in `handle_mouse`, hover underline/cursor/tooltip in `show`.
  Test: `ctrl_click_on_a_url_opens_it_and_plain_click_does_not`.
- Opening goes through `egui::Context::open_url` → eframe → `webbrowser`.
