# About this part

The seven guides the program ships, verbatim.

**These are not a copy.** `gmuse` reads these exact files at compile time with `include_str!` and draws them as **view
7** — so the guide you read here and the guide you read by pressing `7` cannot disagree. That property is most of why
this project uses mdBook at all.

They answer "how do I do the thing", which is a different question from "what does this key do". The keymap answers the
second, and it is _generated_ from the binding table at read time rather than written down — so a `:bind` at runtime
shows up in it and there is no second copy of the keymap to fall out of step. That listing is not published here,
because a table of what is true in _your_ running program is not a thing a book can hold.

Start with **Moving around** if you have never opened it. **The library tree** is the one worth reading even if you
think you know it, because the single-buffer idea underneath it is what makes the motions work.
