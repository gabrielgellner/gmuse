# What the library says

    K          look up the row under the cursor
    :info      the same thing, typed
    enter      follow the link under the cursor
    ^o         back
    escape     leave the panel

`K` is vim's "look up the thing under the cursor", and that is what this is. It opens view 8 on the **recording** the
row holds — the performance, not the file — and everything else is one hop from there.

## Reaching things that are not rows

Half of what the panel shows is not a track: an artist, a release group, a genre, a crate. `:info` names one, in the
same grammar the rest of the `:` line uses:

    :info artist="Bodzin"      an artist
    :info in="Dance"           a crate
    :info genre="Techno"       a genre, on its axis
    :info version="live"       any other axis, the same way

Part of a name is enough. If several answer to it, **it offers the choice rather than picking one**:

    ┌ artist ──────────────────────────────────────────────┐
    │  > █                                                 │
    │                                                      │
    │  > Stephan Bodzin                            artist  │
    │    Stephan Bodzin & Marc Romboy              artist  │
    │    Stephan Bodzin, Luna Semara               artist  │
    └──────────────────────────────────────────────────────┘

Keep typing to narrow it, `enter` to look at one. This is the same picker `<leader>ff` opens, over the things already
found rather than over the library — so there is no filter half here, and everything you type is the name.

If nothing answers, it says what it was looking for:

    no artist called "Ozric Tentacles"

`saved="…"` is not something to look up. A saved view is a **definition**; what it names has a page, the name does not.

## It shows claims, not values

Every other player prints `genre: Techno, Minimal`. This one keeps who said each, because the library keeps who said
each:

    genre         Techno         tag · confirmed
    genre         Techno         discogs · matched
    genre         Minimal        discogs · matched
    genre         Ambient        ruled out · manual

Four lines where a tag has one. Two sources agreeing about Techno are **two claims**, not one fact stated twice, and the
last line is a genre somebody ruled out — struck rather than absent, because "I decided against this" and "nobody ever
said it" are different answers and only a visible row can tell you which you are looking at.

That is the whole reason this panel exists. The tags in your files are a serialization; the library is what it says.

## The sections

| section       | what it is                                             |
| ------------- | ------------------------------------------------------ |
| what it is    | the identity — what an authority named it, if anything |
| said about it | the claims, with who made each                         |
| credited to   | the artists, by role                                   |
| where it sits | the release, the group, the track number               |
| copies        | every file holding it, here or on another machine      |
| history       | plays and rating                                       |
| filed in      | the crates it is in                                    |

A section with nothing in it is not drawn. Empty headings would say the library knows less than it does.

## Following a link

Rows ending in `→` go somewhere. `enter` follows one; `^o` comes back. The trail along the bottom is where you have
been:

    Callisto ▸ Luna ▸ Stephan Bodzin

`^o` rather than `h`, because this is **a jump list and not a tree**. You can arrive at an artist from a recording, from
a release group or from a concept, and there is no parent to climb to — `h` would promise one. vim spells the same
distinction the same way.

`^o` past the beginning leaves the panel: the way in is the way out.

`enter` on a section header folds it instead, and a fold is remembered by name — so hiding the parts you never read
stays hidden as you walk around, rather than resetting on every page.

Going in a circle is fine. A recording leads to a genre, which leads back to the recording; nothing prevents it, because
there is nothing wrong with it.

## A count instead of a list

An artist with four hundred recordings does not get four hundred rows. It gets the number and the query that finds them:

    credited on   things   412   artist="Stephan Bodzin"

The library view is what is good at showing four hundred things. The panel points at it rather than competing with it.

## The facts behind it

The last section is the log itself — folded away, because it is long and it is rarely what you came for. `enter` on the
heading opens it:

    ▾ facts (4)
      today       7405e4cd   CollectionEntryAdded    collection=General ord=a0 recording=I'll Be Waiting
      yesterday   7405e4cd   RecordingIdentified     basis=musicbrainz dd2d2073…
      3d ago      7405e4cd   ConceptAsserted         concept=Techno basis=discogs
      3d ago      7405e4cd   ⊘ ConceptRetracted      concept=Ambient basis=manual

A claim's basis says **who**. This says **when**, and **on which machine** — the column after the date is the device.
Something you decided on the laptop and something a tool decided last week read differently, and only the log can tell
you which.

`⊘` marks a withdrawal: somebody took something back rather than saying it.

This does not replace grepping the log — it is a plain file on disk for exactly that reason. What it adds is the same
records already filtered to one thing, which grep can only do if you already know the id, and the id is what you do not
have when you are looking at a row on a screen.

## A row with nothing behind it

Some files have no recording the library can name — that is ordinary, not an error. `K` on one says so rather than
opening a page with nothing on it, which would read as "nothing is known" when the truth is "there is nothing here to
know it about".
