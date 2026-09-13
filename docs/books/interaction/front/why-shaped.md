# About this part

The design behind the guides — and, for two chapters, design that has no guide yet because it is not built.

The through-line is that **structure should be usable as structure**. A file explorer cannot offer "collapse to
artists", because "three directories deep" is not a category. This tree can, because every row at a level is the same
kind of thing: `zr` puts every artist's albums on screen and `zm` walks back out. The motions follow from the same fact
— `}` steps to the next row at the level you are on, `]]` always means artist, `-` goes up to what a row is filed under.

**Two of these five are proposals**, and they are the interesting ones to argue with:

- **The transport vocabulary** is where the player's four verbs run out. Issues #225–#229 carry it — Play, Pause and
  Toggle as three verbs rather than one toggle resolved by the sender; one parameterised Jump instead of a dozen; a
  visible mode that gives a count its unit; momentary verbs on the terminals that cannot report key release.
- **The info panel** turns on one rule: **a claim is never a value.** The library holds "Discogs said Techno,
  confidently, in March" rather than "genre: Techno", and a surface that showed only the second would be throwing away
  the disagreement that makes the first worth having.
