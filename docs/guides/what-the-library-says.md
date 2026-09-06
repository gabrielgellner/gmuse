# What the library says

    K          look up the row under the cursor
    :info      the same thing, typed
    escape     back to where you were

`K` is vim's "look up the thing under the cursor", and that is what this is. It opens view 8 on the **recording** the
row holds — the performance, not the file — and everything else is one hop from there.

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

## A count instead of a list

An artist with four hundred recordings does not get four hundred rows. It gets the number and the query that finds them:

    credited on   things   412   artist="Stephan Bodzin"

The library view is what is good at showing four hundred things. The panel points at it rather than competing with it.

## A row with nothing behind it

Some files have no recording the library can name — that is ordinary, not an error. `K` on one says so rather than
opening a page with nothing on it, which would read as "nothing is known" when the truth is "there is nothing here to
know it about".
