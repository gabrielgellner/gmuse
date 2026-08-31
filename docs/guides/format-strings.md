# Writing the lines yourself

Two lines are yours to write: the now-playing description in the status bar, and how a track reads in the library tree.

    [format]
    now_playing = "{artist}[ — {album}] — {title:.40}"
    track = "{track:2}  {display_title}  {duration}"

## Fields

    artist  albumartist  file_under  album  title  display_title
    genre   label        path        filename
    track   disc         year        duration   bitrate  samplerate  channels

`display_title` falls back to the file name, so it is never empty. `file_under` is what the tree files a track under —
the album artist, or the artist, or "Unknown Artist".

The same names work in filters and in `sort`. One vocabulary, three uses.

## Sizing

    {track:02}      pad to two with zeroes
    {artist:20}     pad to twenty
    {duration:>7}   right-align in seven
    {title:.40}     truncate at forty
    {title:8.8}     pad to eight, then cut at eight

The value is padded first and cut second, so the maximum always wins.

## Groups that disappear

Square brackets vanish entirely when a field inside them is missing:

    "{artist}[ — {album}] — {title}"

A single with no album renders `Cat Clyde — All The Black`, with no stranded dash. That is the whole reason the groups
exist — otherwise every optional tag needs a separate template.

Write a literal bracket or brace with a backslash: `\[`, `\{`.
