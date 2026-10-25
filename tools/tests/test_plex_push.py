"""Plex reading the rendering: checked, and given the library's ratings and playlists — #340.

No network: `get` is a fake that answers from fixtures, and the one write
test stands in for `requests.request`. What these hold to is mostly what is
*not* done — a rating already right is not re-sent, a compilation's copy is
not what a playlist points at — so each shows the thing that *is* done beside.
"""

from __future__ import annotations

import xml.etree.ElementTree as ET
from typing import TYPE_CHECKING, Any

import pytest

from gmuse_tools import plex_push
from gmuse_tools.plex_push import (
    Held,
    Placed,
    PushError,
    Track,
    Writer,
    keys_by_recording,
    locked,
    placed_of,
    plan_playlists,
    ratings_to_push,
    section_of,
    tracks_of,
    verify,
)

if TYPE_CHECKING:
    from collections.abc import Mapping

ROOT = "/share/Media/PlexMusic"


def xml(text: str) -> ET.Element:
    """An answer, as Plex sends one."""
    return ET.fromstring(text)  # noqa: S314 — a fixture


def test_the_rendering_library_is_found_by_where_it_reads() -> None:
    """Not by name: a library is renamed in step 7 and its location is not."""
    answer = xml(
        '<MediaContainer><Directory key="3" title="Music"><Location path="/share/Media/Music/General"/></Directory>'
        '<Directory key="10" title="Music (gmuse)"><Location path="/share/Media/PlexMusic"/></Directory>'
        "</MediaContainer>"
    )
    assert section_of(lambda *_a, **_k: answer, ROOT) == "10"
    with pytest.raises(PushError):
        section_of(lambda *_a, **_k: answer, "/share/Media/Elsewhere")


def page(*tracks: str) -> ET.Element:
    """One page of tracks."""
    return xml(f'<MediaContainer totalSize="{len(tracks)}">{"".join(tracks)}</MediaContainer>')


def track(key: str, title: str, file: str, rating: str = "") -> str:
    """A track element with its file."""
    rated = f' userRating="{rating}"' if rating else ""
    return f'<Track ratingKey="{key}" title="{title}"{rated}><Media><Part file="{file}"/></Media></Track>'


def test_a_tracks_path_is_relative_to_the_rendering_and_its_stars_halved() -> None:
    """A file outside the rendering has no path; ten in Plex is five here."""
    answer = page(
        track("1", "Lush", f"{ROOT}/Orbital/Diversions/05 Lush.mp3", "10.0"), track("2", "x", "/elsewhere/x.mp3")
    )
    got = tracks_of(lambda *_a, **_k: answer, "10", ROOT)
    assert got == [
        Track("1", "Lush", 5, "Orbital/Diversions/05 Lush.mp3"),
        Track("2", "x", None, None),
    ]


def test_verify_sorts_tracks_into_agree_differ_and_stray() -> None:
    """A title Plex changed is named with both spellings."""
    titled = {"a.mp3": "Lush 3–4", "b.mp3": "The Bells Sketch"}
    found = verify(
        [
            Track("1", "Lush 3–4", None, "a.mp3"),
            Track("2", "Give a Man a Rod", None, "b.mp3"),
            Track("3", "?", None, None),
        ],
        titled,
    )
    assert (found.tracks, found.agree, found.stray) == (3, 1, 1)
    assert found.differ == [("b.mp3", "Give a Man a Rod", "The Bells Sketch")]


def placed(recording: str, stars: int | None = None, *, compilation: bool = False) -> Placed:
    """What the library says of a rendered path."""
    return Placed(recording, stars, compilation)


def test_only_a_rating_that_differs_is_pushed() -> None:
    """Already right is nothing to send; a rating the library cleared is sent as cleared."""
    tracks = [Track("1", "", 5, "a"), Track("2", "", None, "b"), Track("3", "", 4, "c"), Track("4", "", None, "x")]
    by_path = {"a": placed("r1", 5), "b": placed("r2", 3), "c": placed("r3", None)}
    assert ratings_to_push(tracks, by_path) == [("2", 3), ("3", None)]


def test_a_playlist_points_at_the_albums_copy_not_the_compilations() -> None:
    """One recording on an album and on a sampler is two Plex items; the song is the album's."""
    tracks = [
        Track("sampler", "", None, "Various/Hits/03 Lush.mp3"),
        Track("album", "", None, "Orbital/Diversions/05 Lush.mp3"),
    ]
    by_path = {
        "Various/Hits/03 Lush.mp3": placed("lush", compilation=True),
        "Orbital/Diversions/05 Lush.mp3": placed("lush"),
    }
    assert keys_by_recording(tracks, by_path) == {"lush": "album"}


def test_locked_fields_are_counted_by_type() -> None:
    """A lock is reported by what it is on and which field."""

    def get(path: str, params: Mapping[str, str] | None = None, *, start: int = 0) -> ET.Element:
        _ = path, start
        kind = (params or {}).get("type")
        if kind == "10":
            return page('<Track ratingKey="1"><Field locked="1" name="title"/></Track>', '<Track ratingKey="2"/>')
        if kind == "9":
            return page(
                '<Directory ratingKey="3"><Field locked="1" name="thumb"/><Field locked="0" name="x"/></Directory>'
            )
        return page()

    assert locked(get, "10") == {"track title": 1, "album thumb": 1}


class Sent:
    """Stands in for `requests.request`, keeping what was asked."""

    def __init__(self, status: int = 200, text: str = "<MediaContainer/>") -> None:
        """Answer every request with this."""
        self.calls: list[tuple[str, str, dict[str, Any]]] = []
        self.status, self.text = status, text

    def __call__(self, method: str, url: str, **kw: Any) -> Any:  # noqa: ANN401 — `requests`' own signature
        """Record the request and answer it."""
        self.calls.append((method, url, kw["params"]))
        return type("Answer", (), {"ok": self.status < 400, "status_code": self.status, "text": self.text})()


def test_a_rating_is_doubled_and_a_cleared_one_is_minus_one(monkeypatch: pytest.MonkeyPatch) -> None:
    """Plex's scale and Plex's word for "no rating"."""
    sent = Sent()
    monkeypatch.setattr(plex_push.requests, "request", sent)
    w = Writer("http://plex", "secret-token")
    w.rate("7", 4)
    w.rate("8", None)
    assert [c[2]["rating"] for c in sent.calls] == ["8", "-1"]


def test_a_refused_write_never_names_the_token(monkeypatch: pytest.MonkeyPatch) -> None:
    """The error says what failed; the credential is in the headers, and stays there."""
    monkeypatch.setattr(plex_push.requests, "request", Sent(status=401))
    with pytest.raises(PushError) as e:
        Writer("http://plex", "secret-token").rate("7", 4)
    assert "/:/rate" in str(e.value), "instrument: the error names the request"
    assert "secret-token" not in str(e.value)


class Lib:
    """What `placed_of` asks of the library: every copy, and what it says of each."""

    def query(self, _spec: str | None, *, every_copy: bool) -> list[dict[str, Any]]:
        """Two copies: one named, one nothing has identified (#160)."""
        assert every_copy
        return [{"encoding": "e1", "recording": "r1"}, {"encoding": "e2"}]

    def statement(self, encodings: list[str]) -> list[dict[str, Any]]:
        """Stars for each."""
        return [{"stars": 4} for _ in encodings]


def test_a_copy_nothing_has_named_is_left_out_not_a_crash() -> None:
    """The library lists unidentified copies on purpose; none is something to rate."""
    manifest = {"a.mp3": {"encoding": "e1"}, "b.mp3": {"encoding": "e2"}, "Album/cover.jpg": {"cover": "1-2"}}
    assert placed_of(Lib(), manifest) == {"a.mp3": Placed("r1", 4, compilation=False)}  # type: ignore[arg-type]


def test_a_playlist_is_created_kept_resynced_or_left_by_whose_it_is() -> None:
    """Gmuse's own copy follows the library; a Plexamp playlist of the same name is not touched."""
    wanted = {"new": ["1"], "same": ["1", "2"], "drifted": ["1", "2", "3"], "reordered": ["2", "1"], "theirs": ["1"]}
    held = {
        "same": Held("10", ours=True, items=["1", "2"]),
        "drifted": Held("11", ours=True, items=["1", "3"]),
        "reordered": Held("13", ours=True, items=["1", "2"]),
        "theirs": Held("12", ours=False, items=["9"]),
    }
    assert plan_playlists(wanted, held) == [
        ("create", "new", ["1"]),
        ("same", "same", ["1", "2"]),
        ("resync", "drifted", ["1", "2", "3"]),
        ("resync", "reordered", ["2", "1"]),
        ("leave", "theirs", ["1"]),
    ]


def test_a_resync_clears_then_adds_in_order(monkeypatch: pytest.MonkeyPatch) -> None:
    """The playlist keeps its identity; its items become the library's, in order."""
    sent = Sent()
    monkeypatch.setattr(plex_push.requests, "request", sent)
    Writer("http://plex", "t").resync("machine", "11", ["3", "1", "2"])
    assert [(m, u.rsplit("plex", 1)[-1]) for m, u, _ in sent.calls] == [
        ("DELETE", "/playlists/11/items"),
        ("PUT", "/playlists/11/items"),
    ]
    assert sent.calls[1][2]["uri"].endswith("/library/metadata/3,1,2")
