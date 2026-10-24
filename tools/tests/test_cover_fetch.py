"""Album art from the Cover Art Archive — #352.

Nothing here touches the network: `lookup`'s three answers are stood in for at
`bronze.get`, and the download is a function handed in. What is real is the
disk, because the claims that matter are about what lands in a master folder
and what is kept — and an absence ("nothing written for an unmarked row") is
only shown by a marked row beside it that *was* written.
"""

from __future__ import annotations

import csv
from typing import TYPE_CHECKING, Any

import requests

from gmuse_tools import cover_fetch
from gmuse_tools.cover_fetch import Album, Offer, albums, apply, front, lookup, survey, wants_art
from gmuse_tools.covers import dimensions
from tests.test_covers import jpeg

if TYPE_CHECKING:
    from pathlib import Path

    import pytest

RELEASE = "d30d02cf-ec86-4c1a-9514-9daa09cf3139"
GROUP = "218c0181-a18f-5d99-b2c9-57eaa255d3c5"


def listing(*, front_image: bool = True) -> dict[str, Any]:
    """An archive listing, shaped as coverartarchive.org returns one."""
    return {
        "images": [
            {"front": False, "image": "https://x/back.jpg", "thumbnails": {"1200": "https://x/back-1200.jpg"}},
            {
                "front": front_image,
                "image": "https://x/original.jpg",
                "thumbnails": {"250": "https://x/250.jpg", "500": "https://x/500.jpg", "1200": "https://x/1200.jpg"},
            },
        ]
    }


def album(tmp_path: Path, cover: bytes | None, name: str = "cover.jpg") -> Album:
    """A master album folder, with a cover if given one."""
    folder = tmp_path / "MusicLibrary" / "Orbital" / "Diversions"
    folder.mkdir(parents=True, exist_ok=True)
    at = None
    if cover is not None:
        at = folder / name
        at.write_bytes(cover)
    return Album(folder, RELEASE, at)


def read(review: Path) -> list[dict[str, str]]:
    """The review file's rows."""
    with review.open(encoding="utf-8") as handle:
        return list(csv.DictReader(handle, delimiter="\t"))


def mark(review: Path, *, action: str | None = None) -> None:
    """Mark rows `y`, as a person would — all of them, or one action's."""
    rows = read(review)
    for r in rows:
        if action is None or r["action"] == action:
            r["apply"] = "y"
    with review.open("w", encoding="utf-8", newline="") as handle:
        w = csv.DictWriter(handle, fieldnames=list(rows[0]), delimiter="\t")
        w.writeheader()
        w.writerows(rows)


def test_an_album_wants_art_when_it_has_none_or_a_thumbnail(tmp_path: Path) -> None:
    """No cover is an add; under 500 px an upgrade; a real cover is left alone."""
    assert wants_art(album(tmp_path / "a", None)) == "add"
    assert wants_art(album(tmp_path / "b", jpeg(300, 300))) == "upgrade"
    assert wants_art(album(tmp_path / "c", jpeg(1200, 1200))) is None


def test_the_front_is_the_1200_thumbnail_of_the_front_image() -> None:
    """Not the back, and not the original scan."""
    assert front(listing()) == "https://x/1200.jpg"
    assert front(listing(front_image=False)) is None
    assert front({"images": []}) is None
    assert front(None) is None


def stand_in(monkeypatch: pytest.MonkeyPatch, answers: dict[str, tuple[int, object]]) -> list[str]:
    """Answer `bronze.get` from `answers` by URL prefix, and record what was asked."""
    asked: list[str] = []

    def get(_con: object, url: str, _params: object = None, **_: object) -> tuple[int, object]:
        asked.append(url)
        return next((v for k, v in answers.items() if url.startswith(k)), (404, None))

    monkeypatch.setattr(cover_fetch.bronze, "get", get)
    return asked


def test_a_release_with_art_is_offered_its_own(monkeypatch: pytest.MonkeyPatch) -> None:
    """The pressing first."""
    asked = stand_in(monkeypatch, {f"{cover_fetch.CAA}/release/{RELEASE}": (200, listing())})
    assert lookup(None, RELEASE) == Offer("https://x/1200.jpg", "release")  # type: ignore[arg-type]
    assert len(asked) == 1, "and nothing more is asked"


def test_a_release_without_art_falls_back_to_its_group(monkeypatch: pytest.MonkeyPatch) -> None:
    """A reissue often has none of its own; its group's front is the album's."""
    asked = stand_in(
        monkeypatch,
        {
            cover_fetch.MB_RELEASE.format(RELEASE): (200, {"release-group": {"id": GROUP}}),
            f"{cover_fetch.CAA}/release-group/{GROUP}": (200, listing()),
        },
    )
    assert lookup(None, RELEASE) == Offer("https://x/1200.jpg", "release-group")  # type: ignore[arg-type]
    assert asked[0].startswith(f"{cover_fetch.CAA}/release/"), "instrument: the pressing was asked first"


def test_a_release_and_group_without_art_offer_nothing(monkeypatch: pytest.MonkeyPatch) -> None:
    """Nothing is an answer, reported in the review."""
    stand_in(monkeypatch, {cover_fetch.MB_RELEASE.format(RELEASE): (200, {"release-group": {"id": GROUP}})})
    assert lookup(None, RELEASE) is None  # type: ignore[arg-type]


def offering(art: bytes, via: str = "release") -> dict[str, Any]:
    """`look` and `fetch` stand-ins that offer one image."""
    return {"look": lambda _: Offer("https://x/1200.jpg", via), "fetch": lambda _: art}


def test_the_survey_stages_offers_and_marks_nothing(tmp_path: Path) -> None:
    """An offer is downloaded for a person to look at; the library is not touched and nothing is marked."""
    a = album(tmp_path, None)
    out, staged = tmp_path / "out.tsv", tmp_path / "staged"
    survey([a], out, staged, **offering(jpeg(1200, 1200)))
    (row,) = read(out)
    assert row["action"] == "add"
    assert row["offered"] == "1200x1200"
    assert row["apply"] == "", "a person marks it"
    assert list(a.folder.iterdir()) == [], "no master changed"
    assert (staged / f"{RELEASE}.jpg").is_file()
    assert "1200x1200" in (staged / "index.html").read_text(), "the contact sheet shows it"


def test_an_archive_image_no_larger_than_the_cover_is_not_offered(tmp_path: Path) -> None:
    """An upgrade that is not one."""
    a = album(tmp_path, jpeg(400, 400))
    out = tmp_path / "out.tsv"
    survey([a], out, tmp_path / "staged", **offering(jpeg(250, 250)))
    (row,) = read(out)
    assert row["action"] == "upgrade", "instrument: it did want an upgrade"
    assert "no larger" in row["why"]
    assert row["image"] == ""


def test_a_failed_download_or_a_non_image_is_a_reason_not_a_crash(tmp_path: Path) -> None:
    """Both are rows saying why, and the survey goes on."""

    def broken(_: str) -> bytes:
        raise requests.ConnectionError

    out = tmp_path / "a.tsv"
    survey([album(tmp_path / "1", None)], out, tmp_path / "s", look=lambda _: Offer("u", "release"), fetch=broken)
    assert "download failed" in read(out)[0]["why"]
    survey([album(tmp_path / "2", None)], out, tmp_path / "s", **offering(b"<html>not an image</html>"))
    assert "not a JPEG or PNG" in read(out)[0]["why"]


def test_a_survey_that_finds_nothing_still_writes_the_header(tmp_path: Path) -> None:
    """So last run's rows are not left standing as if current."""
    out = tmp_path / "out.tsv"
    out.write_text("path\tapply\nstale\ty\n")
    survey([album(tmp_path, jpeg(1200, 1200))], out, tmp_path / "staged", **offering(jpeg(1200, 1200)))
    assert read(out) == []
    assert out.read_text().startswith("path\taction\tapply")


def surveyed(tmp_path: Path, cover: bytes | None, offered: bytes) -> tuple[Album, Path]:
    """An album surveyed against an offer, and the review."""
    a = album(tmp_path, cover)
    out = tmp_path / "out.tsv"
    survey([a], out, tmp_path / "staged", **offering(offered))
    return a, out


def test_apply_writes_only_marked_rows(tmp_path: Path) -> None:
    """An unmarked row is nothing written; a marked one beside it is the instrument."""
    a, out = surveyed(tmp_path / "unmarked", None, jpeg(1200, 1200))
    assert apply(out, write=True).total() == 0
    assert not (a.folder / "cover.jpg").exists()

    b, out = surveyed(tmp_path / "marked", None, jpeg(1200, 1200))
    mark(out)
    apply(out, write=True)
    assert (b.folder / "cover.jpg").read_bytes() == jpeg(1200, 1200)


def test_a_dry_run_writes_nothing(tmp_path: Path) -> None:
    """`fetch-apply` without `--write` says what it would do."""
    a, out = surveyed(tmp_path, None, jpeg(1200, 1200))
    mark(out)
    tally = apply(out, write=False)
    assert tally == {"would add cover.jpg": 1}
    assert not (a.folder / "cover.jpg").exists()


def test_an_add_never_overwrites_a_cover_that_appeared_since(tmp_path: Path) -> None:
    """Somebody put art there after the survey; it is theirs."""
    a, out = surveyed(tmp_path, None, jpeg(1200, 1200))
    mark(out)
    (a.folder / "cover.jpg").write_bytes(b"put here by hand")
    tally = apply(out, write=True)
    assert "appeared since" in next(iter(tally))
    assert (a.folder / "cover.jpg").read_bytes() == b"put here by hand"


def test_an_upgrade_keeps_the_old_cover_and_does_not_repeat(tmp_path: Path) -> None:
    """The thumbnail becomes `cover-small.jpg`; a second apply changes nothing."""
    small = jpeg(300, 300)
    a, out = surveyed(tmp_path, small, jpeg(1200, 1200))
    mark(out)
    apply(out, write=True)
    assert (a.folder / "cover-small.jpg").read_bytes() == small
    assert (a.folder / "cover.jpg").read_bytes() == jpeg(1200, 1200)

    again = apply(out, write=True)
    assert "already there" in next(iter(again))
    assert (a.folder / "cover-small.jpg").read_bytes() == small, "the kept cover is never overwritten"


def test_an_upgraded_png_keeps_its_own_name(tmp_path: Path) -> None:
    """`cover.png` becomes `cover-small.png`, and the new cover is a `cover.jpg`: one cover remains."""
    from tests.test_covers import png  # noqa: PLC0415

    a = album(tmp_path, png(300, 300), name="cover.png")
    out = tmp_path / "out.tsv"
    survey([a], out, tmp_path / "staged", **offering(jpeg(1200, 1200)))
    mark(out)
    apply(out, write=True)
    names = sorted(p.name for p in a.folder.iterdir())
    assert names == ["cover-small.png", "cover.jpg"]

    # The CAKE *Comfort Eagle* case: a second apply found no `cover-small.jpg`
    # and kept the new `cover.jpg` as one.
    again = apply(out, write=True)
    assert "cover-small.png is already there" in next(iter(again))
    assert sorted(p.name for p in a.folder.iterdir()) == names


class Lib:
    """What `albums` asks of the library."""

    def __init__(self, rows: list[dict[str, Any]], said: dict[str, dict[str, Any]]) -> None:
        """Rows as `query(every_copy=True)` returns them, and each encoding's statement."""
        self.rows = rows
        self.said = said

    def query(self, _spec: str | None, *, every_copy: bool) -> list[dict[str, Any]]:
        """Every copy."""
        assert every_copy
        return self.rows

    def statement(self, encodings: list[str]) -> list[dict[str, Any]]:
        """What the library says about each."""
        return [self.said[e] for e in encodings]


def test_an_album_is_the_release_most_of_its_copies_are_on(tmp_path: Path) -> None:
    """Two pressings in one folder: the majority is the album's; a loose single is no album."""
    root = tmp_path / "MusicLibrary"
    album_dir = root / "Orbital" / "Diversions"
    album_dir.mkdir(parents=True)
    rows = [{"encoding": f"e{i}", "locator": str(album_dir / f"0{i} t.flac")} for i in range(3)] + [
        {"encoding": "single", "locator": str(root / "Jeremie Albino" / "Rollin'.mp3")}
    ]
    said = {"e0": {"musicbrainz_release": "other"}, "e1": {"musicbrainz_release": RELEASE}}
    said |= {"e2": {"musicbrainz_release": RELEASE}, "single": {"musicbrainz_release": "x"}}
    found = albums(Lib(rows, said), root)  # type: ignore[arg-type]
    assert found == [Album(album_dir, RELEASE, None)]


def test_a_second_survey_downloads_nothing_again(tmp_path: Path) -> None:
    """The image already staged is the answer; only a missing one is fetched."""
    calls: list[str] = []

    def fetch(url: str) -> bytes:
        calls.append(url)
        return jpeg(1200, 1200)

    a = album(tmp_path, None)
    look = lambda _: Offer("https://x/1200.jpg", "release")  # noqa: E731
    survey([a], tmp_path / "out.tsv", tmp_path / "staged", look=look, fetch=fetch)
    assert len(calls) == 1, "instrument: the first survey downloaded it"
    survey([a], tmp_path / "out.tsv", tmp_path / "staged", look=look, fetch=fetch)
    assert len(calls) == 1
    assert read(tmp_path / "out.tsv")[0]["offered"] == "1200x1200", "and still offers it"


def test_an_upgrade_replaces_only_the_cover_that_was_surveyed(tmp_path: Path) -> None:
    """A cover changed since the survey is not the thumbnail a person looked at."""
    a, out = surveyed(tmp_path, jpeg(300, 300), jpeg(1200, 1200))
    mark(out)
    assert a.cover is not None
    a.cover.write_bytes(jpeg(450, 450))
    tally = apply(out, write=True)
    assert "not the one surveyed" in next(iter(tally))
    assert dimensions(a.cover.read_bytes()) == (450, 450)
    assert not (a.folder / "cover-small.jpg").exists()
