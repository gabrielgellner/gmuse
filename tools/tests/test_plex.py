"""Plays and ratings read back from Plex — #338.

A fake server answers each endpoint from fixtures, so nothing here touches the
network. The claims are mostly about what is **not** proposed — an echo, a
half star, a file no rendering made — so each test that shows an absence also
shows the proposal beside it that was made.
"""

from __future__ import annotations

import json
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import TYPE_CHECKING, Any

from gmuse_tools.plex import GMUSE_SUMMARY, PAGE, Audio, Copy, Keeping, every, playlists, read, resolver, stamp

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

SERVER_ID = "0c1eda1ba823bfe1fcce17edfd592f48560c435d"
DEVICE = f"plex:{SERVER_ID}"
ROOT = "/share/Media/PlexMusic"


def element(tag: str, children: list[dict[str, str]] | None = None, **attrs: str) -> ET.Element:
    """An XML element with children, each a dict of attributes."""
    e = ET.Element(tag, attrs)
    for child in children or []:
        ET.SubElement(e, child.pop("tag", "Track"), child)
    return e


class FakeServer:
    """Answers `get(path, params, start=)` from what a test puts in it, paged like Plex."""

    def __init__(self) -> None:
        """An empty music server."""
        self.history: list[dict[str, str]] = []
        self.rated: list[dict[str, str]] = []
        self.files: dict[str, str] = {}
        self.asked: list[str] = []
        #: Each a playlist's attributes plus `files`, one per entry.
        self.lists: list[dict[str, Any]] = []

    def get(self, path: str, _params: Mapping[str, str] | None = None, *, start: int = 0) -> ET.Element:
        """One page of the answer to one endpoint."""
        self.asked.append(path)
        if path == "/identity":
            return element("MediaContainer", machineIdentifier=SERVER_ID)
        if path == "/library/sections":
            return element("MediaContainer", [{"tag": "Directory", "key": "3", "type": "artist"}])
        if path == "/status/sessions/history/all":
            return page(self.history, start)
        if path.startswith("/library/sections/"):
            return page(self.rated, start)
        if path.startswith("/playlists"):
            return self.playlist(path, start)
        if path.startswith("/library/metadata/"):
            keys = path.rsplit("/", 1)[-1].split(",")
            out = ET.Element("MediaContainer")
            for k in keys:
                if k in self.files:
                    track = ET.SubElement(out, "Track", {"ratingKey": k})
                    media = ET.SubElement(track, "Media")
                    ET.SubElement(media, "Part", {"file": self.files[k]})
            return out
        raise AssertionError(path)

    def playlist(self, path: str, start: int) -> ET.Element:
        """The playlists, or one playlist's entries, each with a `playlistItemID`."""
        if path == "/playlists":
            out = ET.Element("MediaContainer")
            for p in self.lists:
                ET.SubElement(out, "Playlist", {k: str(v) for k, v in p.items() if k != "files"})
            return out
        key = path.split("/")[2]
        files = next(p["files"] for p in self.lists if p["ratingKey"] == key)
        out = ET.Element("MediaContainer", {"totalSize": str(len(files))})
        for n, file in enumerate(files[start : start + PAGE], start):
            track = ET.SubElement(out, "Track", {"playlistItemID": str(1000 + n)})
            ET.SubElement(ET.SubElement(track, "Media"), "Part", {"file": file})
        return out


def page(items: list[dict[str, str]], start: int) -> ET.Element:
    """A page of a list, as Plex sends one."""
    return element("MediaContainer", [dict(i) for i in items[start : start + PAGE]], totalSize=str(len(items)))


MANIFEST = {"Orbital/Diversions/05 Lush.mp3": {"encoding": "enc-lush"}}
COPIES = {"enc-lush": Copy("enc-lush", "rec-lush", None)}


def library(stars: int | None = None) -> Callable[[str], Copy | None]:
    """The resolver for a library holding Lush, rated `stars`."""
    return resolver(MANIFEST, {"enc-lush": Copy("enc-lush", "rec-lush", stars)}, ROOT)


def lush(server: FakeServer, key: str = "100") -> None:
    """Make item `key` the rendered Lush."""
    server.files[key] = f"{ROOT}/Orbital/Diversions/05 Lush.mp3"


def play(key: str, history_id: int, viewed: int) -> dict[str, str]:
    """A history entry."""
    return {"ratingKey": key, "historyKey": f"/status/sessions/history/{history_id}", "viewedAt": str(viewed)}


def test_a_play_is_proposed_with_plexs_own_stamp() -> None:
    """Plex's time, the history entry's id, the server — from Plex's data alone."""
    server = FakeServer()
    lush(server)
    server.history = [play("100", 4242, 1_785_341_390)]
    found = read(server.get, library())
    assert found.proposals == [
        {
            "fact": "played",
            "recording": "rec-lush",
            "encoding": "enc-lush",
            "elsewhere": {"device": DEVICE, "at_ms": 1_785_341_390_000, "seq": 4242},
        }
    ]


def test_reading_twice_proposes_the_same_records() -> None:
    """Nothing remembered between runs: the same history makes the same stamps."""
    server = FakeServer()
    lush(server)
    server.history = [play("100", 1, 1000), play("100", 2, 2000)]
    first = read(server.get, library()).proposals
    assert len(first) == 2, "instrument: two plays, told apart by their ids"
    assert read(server.get, library()).proposals == first


def test_every_page_of_history_is_read() -> None:
    """A history longer than a page is read to its end."""
    server = FakeServer()
    lush(server)
    server.history = [play("100", i, 1000 + i) for i in range(PAGE * 2 + 7)]
    assert read(server.get, library()).plays == PAGE * 2 + 7


def test_a_file_no_rendering_made_is_left_and_counted() -> None:
    """An old library's play is #339's; it is never matched by name."""
    server = FakeServer()
    lush(server)
    server.files["200"] = "/share/Media/Music/General/Orbital/Diversions/05 Lush.mp3"
    server.history = [play("100", 1, 1000), play("200", 2, 2000)]
    found = read(server.get, library())
    assert found.plays == 1, "instrument: the rendered file's play was proposed"
    assert found.left["a play: a file no rendering made"] == 1


def rated(key: str, rating: str, at: int = 1_785_000_000) -> dict[str, str]:
    """A rated track."""
    return {"ratingKey": key, "userRating": rating, "lastRatedAt": str(at)}


def test_a_rating_is_halved_and_stamped_by_the_item() -> None:
    """Ten in Plex is five here, stamped with when it was rated and the item's id."""
    server = FakeServer()
    lush(server)
    server.rated = [rated("100", "8.0")]
    (proposal,) = read(server.get, library(stars=None)).proposals
    assert proposal == {
        "fact": "rated",
        "recording": "rec-lush",
        "stars": 4,
        "elsewhere": {"device": DEVICE, "at_ms": 1_785_000_000_000, "seq": 100},
    }


def test_a_rating_the_library_already_holds_does_not_echo() -> None:
    """A pushed rating comes back re-stamped; it is not a new one."""
    server = FakeServer()
    lush(server)
    server.rated = [rated("100", "10.0")]
    assert read(server.get, library(stars=4)).ratings == 1, "instrument: a different rating is proposed"
    found = read(server.get, library(stars=5))
    assert found.ratings == 0
    assert found.left["a rating the library already holds"] == 1


def test_a_half_star_is_reported_not_rounded() -> None:
    """Nine of ten is four and a half, which the library cannot hold."""
    server = FakeServer()
    lush(server)
    server.rated = [rated("100", "9.0")]
    found = read(server.get, library())
    assert found.ratings == 0
    assert found.left["a half star, which the library cannot hold"] == 1


def test_a_stamp_needs_a_time_and_an_id_that_fits() -> None:
    """No time, no id, or an id past 32 bits is no stamp."""
    assert stamp(DEVICE, "1000", "7") == {"device": DEVICE, "at_ms": 1_000_000, "seq": 7}
    assert stamp(DEVICE, None, "7") is None
    assert stamp(DEVICE, "1000", None) is None
    assert stamp(DEVICE, "1000", str(2**32)) is None


def test_the_token_is_never_in_what_is_written() -> None:
    """The proposals carry the server's id, not the credential used to reach it."""
    server = FakeServer()
    lush(server)
    server.history = [play("100", 1, 1000)]
    server.rated = [rated("100", "6.0")]
    text = json.dumps(read(server.get, library()).proposals)
    assert SERVER_ID in text, "instrument: the server is named"
    assert "token" not in text.lower()


def test_every_asks_until_the_total_is_reached() -> None:
    """Pages are asked for by offset, not by hoping one is enough."""
    server = FakeServer()
    server.history = [play("1", i, i) for i in range(PAGE + 1)]
    got = every(server.get, "/status/sessions/history/all")
    assert len(got) == PAGE + 1
    assert server.asked.count("/status/sessions/history/all") == 2


# --- The old libraries, placed by their audio — #339 --------------------------


def by_audio(answers: dict[str, str]) -> Callable[[str], str | None]:
    """A stand-in for `Audio`: a file's encoding, or `!why`."""
    return lambda file: answers.get(file, "!a file whose audio could not be read")


OLD = "/share/Media/Music/General/Orbital/Diversions/05 Lush.mp3"


def test_an_old_library_file_is_placed_by_its_audio() -> None:
    """A file no rendering made, whose bytes are a copy the library holds."""
    resolve = resolver(MANIFEST, COPIES, ROOT, by_audio({OLD: "enc-lush"}))
    assert resolve(OLD) == COPIES["enc-lush"]
    assert resolve(f"{ROOT}/Orbital/Diversions/05 Lush.mp3") == COPIES["enc-lush"], "the manifest still comes first"


def test_what_cannot_be_placed_says_why() -> None:
    """Each reason is its own, so the review can be read."""
    resolve = resolver(MANIFEST, COPIES, ROOT, by_audio({OLD: "enc-someone-else"}))
    assert resolve(OLD) == "audio the library does not hold"
    assert resolve("/share/Media/Music/x.mp3") == "a file whose audio could not be read"
    assert resolve("") == "an item Plex no longer has"
    assert resolve(f"{ROOT}/Not/In/The Manifest.mp3") == "a file in the rendering its manifest does not name"
    assert resolver(MANIFEST, COPIES, ROOT)(OLD) == "a file no rendering made", "without by_audio, as #338 was"


def test_the_review_keeps_each_files_history() -> None:
    """For a file the library does not hold, its plays and rating are its only record."""
    server = FakeServer()
    server.files["200"] = OLD
    server.history = [play("200", 1, 1000), play("200", 2, 2000), {"historyKey": "/x/3", "viewedAt": "3000"}]
    server.rated = [rated("200", "10.0")]
    found = read(server.get, resolver(MANIFEST, COPIES, ROOT, by_audio({OLD: "enc-someone-else"})))
    assert found.plays == 0
    assert found.unplaced[OLD] == ["audio the library does not hold", 2, "10.0"]
    assert found.unplaced["(no file)"][0] == "an item Plex no longer has"


class Lib:
    """`encoding_at`, counting how often it is asked."""

    def __init__(self) -> None:
        """Asked nothing yet."""
        self.asked: list[str] = []

    def encoding_at(self, path: object) -> str | None:
        """The bytes' encoding: their content, here."""
        self.asked.append(str(path))
        return "enc-" + Path(str(path)).read_text()


def test_audio_is_hashed_once_across_reads_and_again_when_it_changes(tmp_path: Path) -> None:
    """Remembered by path, size and mtime: a second read hashes nothing."""
    mount = tmp_path / "Media"
    track = mount / "Music" / "a.mp3"
    track.parent.mkdir(parents=True)
    track.write_text("lush")
    lib, cache = Lib(), tmp_path / "cache.json"

    first = Audio(lib, str(mount), cache)  # type: ignore[arg-type]
    assert first("/share/Media/Music/a.mp3") == "enc-lush"
    first.save()
    assert len(lib.asked) == 1, "instrument: the first read hashed it"

    again = Audio(lib, str(mount), cache)  # type: ignore[arg-type]
    assert again("/share/Media/Music/a.mp3") == "enc-lush"
    assert len(lib.asked) == 1

    track.write_text("a different take")
    assert again("/share/Media/Music/a.mp3") == "enc-a different take"
    assert len(lib.asked) == 2


def test_a_file_the_share_will_not_open_is_not_gone(tmp_path: Path) -> None:
    """The three DanceMusic files list and will not open: a reading failure, said as one."""
    audio = Audio(Lib(), str(tmp_path), tmp_path / "cache.json")  # type: ignore[arg-type]
    assert audio("/share/Media/DanceMusic/Lvis Mejía - In Praise Of Shadows.mp3") == (
        "!a file that could not be read through the share"
    )


# --- Playlists: the mirror — #349 -------------------------------------------

RENDERED = f"{ROOT}/Orbital/Diversions/05 Lush.mp3"


def plexamp(key: str, title: str, files: list[str], **attrs: str) -> dict[str, Any]:
    """A hand-made Plex playlist, made at 1000 and last edited at 2000."""
    return {
        "ratingKey": key,
        "title": title,
        "smart": "0",
        "addedAt": "1000",
        "updatedAt": "2000",
        "files": files,
        **attrs,
    }


def nothing_held(_name: str, desired: list[tuple[int, str]]) -> tuple[list[tuple[int, str]], list[tuple[str, str]]]:
    """A library holding none of a playlist yet: everything is an add."""
    return desired, []


def test_a_plexamp_playlist_is_mirrored_as_plexs() -> None:
    """Kept by Plex, its entries at Plex's positions — a gap where one cannot be placed — and Plex's stamps."""
    server = FakeServer()
    server.lists = [plexamp("5", "made in plexamp", [RENDERED, "/share/Media/Music/elsewhere.mp3", RENDERED])]
    found = playlists(server.get, library(), Keeping(frozenset(), nothing_held))
    kept, *entries = found.proposals
    assert kept == {
        "fact": "kept",
        "collection": "made in plexamp",
        "device": DEVICE,
        "elsewhere": {"device": DEVICE, "at_ms": 1_000_000, "seq": 5},
    }
    assert [(e["position"], e["elsewhere"]["seq"]) for e in entries] == [(0, 1000), (2, 1002)], "a gap at 1"
    assert all(e["elsewhere"]["at_ms"] == 2_000_000 for e in entries), "stamped with the playlist's last edit"
    assert found.left["an entry of 'made in plexamp': a file no rendering made"] == 1


def test_a_playlist_gmuse_keeps_or_made_is_not_mirrored() -> None:
    """Smart ones are rules; marked ones `push` made; a name the library keeps is its own."""
    server = FakeServer()
    server.lists = [
        plexamp("1", "\u2764\ufe0f Tracks", [RENDERED], smart="1"),
        plexamp("2", "Did someone say Pop?", [RENDERED], summary=GMUSE_SUMMARY),
        plexamp("3", "adopted", [RENDERED]),
        plexamp("4", "theirs", [RENDERED]),
    ]
    found = playlists(server.get, library(), Keeping(frozenset({"adopted"}), nothing_held))
    assert {p["collection"] for p in found.proposals} == {"theirs"}, "instrument: one is mirrored"


def test_a_playlist_unchanged_proposes_only_its_keeper() -> None:
    """Nothing to add or remove is nothing proposed — and the keeper's record is the same every read."""
    server = FakeServer()
    server.lists = [plexamp("5", "made in plexamp", [RENDERED])]
    found = playlists(server.get, library(), Keeping(frozenset(), lambda _n, _d: ([], [])))
    assert [p["fact"] for p in found.proposals] == ["kept"]


def test_an_entry_plex_dropped_is_taken_out_by_its_key() -> None:
    """A removal names the entry's key, and its stamp is the same on every read of that state."""
    server = FakeServer()
    server.lists = [plexamp("5", "made in plexamp", [])]

    def drop(_name: str, _desired: list[tuple[int, str]]) -> tuple[list[tuple[int, str]], list[tuple[str, str]]]:
        return [], [("a1", "rec-gone")]

    first = playlists(server.get, library(), Keeping(frozenset(), drop)).proposals
    (removed,) = [p for p in first if p["fact"] == "uncollected"]
    assert (removed["ord"], removed["recording"]) == ("a1", "rec-gone")
    assert removed["elsewhere"]["at_ms"] == 2_000_000
    assert playlists(server.get, library(), Keeping(frozenset(), drop)).proposals == first


def test_a_long_playlist_is_read_to_its_end() -> None:
    """Every page of its items."""
    server = FakeServer()
    server.lists = [plexamp("2", "long", [RENDERED] * (PAGE + 3))]
    found = playlists(server.get, library(), Keeping(frozenset(), nothing_held))
    entries = [p for p in found.proposals if p["fact"] == "collected_into"]
    assert len(entries) == PAGE + 3
    assert entries[-1]["position"] == PAGE + 2
