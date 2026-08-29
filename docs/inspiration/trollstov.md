# trollstov

<https://github.com/hikikones/trollstov> — local-library music TUI, Rust workspace, ratatui.

The most philosophically opposite project here, and the most useful for it. Its pitch is **"your music is the database —
your files and their metadata are all you need"**, and its non-goals are stated as plainly: _"Supporting most audio
formats. Metadata editing, except for the rating. Gapless playback."_

That last list is worth reading twice. It is a project that decided the opposite of us on two questions we have already
settled, and the same as us on a third — which makes it a good check on whether our reasons were real.

## Ratings in the file — the counter-example to take seriously

_"Portable rating — the rating is part of the metadata."_ Backing up the music folder backs up the ratings, because they
are in the files.

This is a **working counter-argument to the recommendation in #14**, which leans towards keeping ratings in our own
durable store. trollstov shows the other choice made deliberately, with the trade stated: it forbids metadata editing in
general and then carves out the rating as the single exception, precisely because portability is the point of a rating.

That is the exact shape of amendment §1 would need. It does not settle the question — our §1 exists for reasons theirs
does not have to weigh — but it is evidence that the carve-out is a coherent position rather than a slippery slope, and
#14 should be read with it in mind.

`skip_tracks_with_rating` is also a nice minimum: a rating-driven playback filter as a _setting_, without needing a
query language for it. Most of what #15 would be used for, at a fraction of the cost.

## Light and dark terminal detection

`Settings::default()` calls `terminal_colorsaurus::theme_mode()` and picks a different palette for a light terminal than
a dark one.

We do not do this. Our ANSI-name defaults adapt to whatever the terminal's palette is, which is most of the way there,
but the shipped Frappé theme is dark and there is no light equivalent. Detecting the mode is how a default stops being a
guess — and `theme.rs` already has the two-level palette/role split that would make swapping the underlying palette
cheap.

## Pages, and two we do not have

Five pages: `playing`, `tracks`, `search`, `settings`, `logs`.

- **A `logs` page.** In-app diagnostics as a view. Ours go to a file behind `--log`, which is right for a long listening
  test and useless for "why did that track not play". An underrun count and the last few errors, visible without leaving
  the app, is a real gap.
- **`search` as a page, not a prompt.** Ours is a `/` prompt over the current view. A dedicated page is the other
  design: results as a list you navigate, rather than a cursor jump. Worth considering when M8's filters land, since a
  filter result _is_ a page of results — and view 6 is already reserved for exactly that.

No playlist or queue pages, consistent with the README. We have both.

## Smaller things

- **`keep_selected_track_on_sort`** — the cursor stays on the same track when the sort order changes, rather than the
  same row. A small, correct detail we do not handle and would eventually be annoyed by.
- **Three colour roles** — `primary`, `secondary`, `neutral` — against our twenty-odd `Role`s. Theirs is easier to theme
  and cannot express "the playing track is green even under the cursor", which took us three attempts to get right. Ours
  is the right trade for this project; theirs is the reminder that role count is a cost.
- **Settings are TOML with a `VERSION: u8`** for deserialisation compatibility. Same idea as our cache's
  `FORMAT_VERSION`, applied to config — which is a thing we do not version at all.
- **Workspace split** — `database`, `jukebox`, `utils`, `widgets` as separate crates. We are a single binary; §8's
  module layout does the same job without the Cargo ceremony.

## What was not read

Its keybindings and navigation — the folder's main remit — are not documented in the README, and I did not read
`events.rs`. The notes above come from the README, the page list, and `settings.rs`. Treat the TUI-behaviour half of
this file as unexplored rather than uninteresting.

## Deliberately not taken

- **Gapless as a non-goal.** We shipped it in M7; it was the hardest item in §11 and worth it.
- **Album art.** §1, and out of scope regardless of how it is rendered.
