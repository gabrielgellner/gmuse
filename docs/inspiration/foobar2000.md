# foobar2000

<https://www.foobar2000.org> — Windows desktop player, closed source, freeware.

The odd entry: not a TUI, not Rust, not something to read the code of. It is here for one reason — **it has thought
harder about power-user query and formatting languages than anything else in this folder**, and both of gmuse's
mini-languages (§7) are in its territory.

Nothing about its UI, its component system or its engine is relevant. Read it for the languages, and for what twenty-odd
years of power users have asked it for.

## Query syntax — compared 2026-08-30

|                       | foobar2000                              | gmuse                                  |
| --------------------- | --------------------------------------- | -------------------------------------- |
| booleans and grouping | `AND OR NOT ()`                         | `& \| ! ()` — parity                   |
| exact / substring     | `IS` / `HAS` (word set)                 | `="x"` / `="*x*"`                      |
| prefix, suffix        | —                                       | `"x*"`, `"*x"` — **ahead**             |
| case                  | always insensitive                      | smart-case — **ahead**                 |
| presence              | `PRESENT` / `MISSING`                   | `field?` / `!field?` — taken from here |
| dates                 | `BEFORE AFTER SINCE DURING LAST n DAYS` | absent — needs #13                     |
| sorting               | `SORT BY`, `SORT DESCENDING BY`         | absent — belongs to #15                |
| field vs field        | `%last_played% AFTER %first_played%`    | absent                                 |

`field?` came directly from `PRESENT`/`MISSING`. Its `HAS` word-set matching — `artist HAS sierra ferrell` matching
"Ferrell, Sierra" in any order — is the one string behaviour not copied; here that is
`artist="*sierra*"&artist="*ferrell*"`, which composes instead of being a special operator.

## Title formatting — and a convergence

**Its conditional sections are `[...]`, exactly ours**, with the same rule: the section disappears if a field inside it
is undefined. `%artist% - [%album% / ]%title%` is our `{artist} - [{album} / ]{title}`. That was arrived at here
independently, from the failure mode recorded in `spotatui.md`, and finding the same answer in the language that has had
the most use is about as good a signal as design gets.

Beyond that it is a **full expression language** and ours is a template:

|                  | foobar2000                           | gmuse                                |
| ---------------- | ------------------------------------ | ------------------------------------ |
| field            | `%artist%`                           | `{artist}`                           |
| conditional      | `[...]`                              | `[...]` — **identical**              |
| literal escape   | `'...'`, `''` for an apostrophe      | `\{` `\[` `\\`                       |
| padding          | `$pad(X,Y)`, `$num(X,Y)`             | `{track:02}`, `{duration:>7}`        |
| **truncation**   | `$cut`, `$left`, `$right`, `$padcut` | **absent**                           |
| **fallback**     | `$if2(%albumartist%,%artist%)`       | **hardcoded as `file_under`**        |
| conditionals     | `$if`, `$ifequal`, `$ifgreater`      | absent                               |
| case             | `$upper`, `$lower`                   | absent                               |
| arithmetic       | `$add $sub $mul $div $mod $min $max` | absent                               |
| variables        | `$puts` / `$get`                     | absent                               |
| sort-name prefix | `$swapprefix`                        | **hardcoded as `smart_artist_sort`** |
| multi-value      | `$meta_sep(artist,', ',' and ')`     | absent                               |

### The observation worth keeping

**Three things we hardcoded are functions there.** `file_under` is `$if2(%albumartist%,%artist%)`. `display_title` is
`$if2(%title%,%filename%)`. `smart_artist_sort` is `$swapprefix`. Because our format language cannot express a fallback,
each convenience had to become a field or a setting — which means each new one needs code.

That is not obviously the wrong trade. `{file_under}` is friendlier than `$if2(%albumartist%,%artist%)` and cannot be
got wrong. But it is worth knowing _why_ the field list keeps growing, and a fallback operator would let those three be
expressed rather than built.

### What to take, and what not to

**Truncation is the real gap.** The status line is one line; a long `{artist} — {album} — {title}` simply gets clipped
by the terminal. `$padcut` is how foobar solves it, and `{title:.30}` would be the natural spelling here.

**Not the rest.** `$puts`/`$get`, arithmetic and nested `$if` add up to a programming language living in a config file,
and foobar's power-user configurations are notoriously unreadable for exactly that reason. §1 names scope creep as this
project's failure mode; a format string that needs debugging is what that looks like here.

## Why it is worth checking back on

It is the deepest well of power-user ideas among local-library players, and it reliably has a considered answer to
questions this project has not reached yet — multi-value tag handling, sort orders, per-view column formatting, playback
statistics. When one of those comes up, look here first for the shape of the problem, then decide separately whether it
belongs.
