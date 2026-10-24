"""The renderer's bookkeeping: what it writes, what it deletes, what it refuses — #332.

**The claims here are mostly absences** — nothing rewritten, nothing adopted,
nothing written inside the library — so each test carries a second assertion
computed some other way, proving the run it inspects did something at all.
"""

from __future__ import annotations

import json
import math
import shutil
import struct
import wave
from pathlib import Path
from typing import Any

import pytest

from gmuse_tools.render import ENCODES, MANIFEST, Encode, Refused, Transcoding, check_length, ffmpeg, probe_ms, render


class FakeLibrary:
    """What `render` asks of `gmuse_core.Library`, over files on disk.

    `held` maps encoding → (source file, rendered path). Each encoding is its
    own recording, `rec-<encoding>`, unless `recording` says otherwise — which
    is how a test holds one recording twice. A query is a set of encodings, or
    everything.

    `choose` keeps the first of each group in `held`'s order. The ranking is
    `canonical_key`'s and is tested in Rust; here `chosen` overrides the answer
    for a group, keyed by its encodings, so a test can make it anything.
    """

    def __init__(self, roots: list[Path], held: dict[str, tuple[Path, str]]) -> None:
        """Hold `held`, under library `roots`."""
        self.roots = roots
        self.held = held
        self.recording: dict[str, str] = {}
        self.chosen: dict[frozenset[str], dict[str, Any]] = {}

    def rec(self, enc: str) -> str:
        """The recording a copy is of."""
        return self.recording.get(enc, f"rec-{enc}")

    def query(self, spec: str | None = None, *, every_copy: bool = False) -> list[dict[str, Any]]:
        """Every copy the spec names, as `query(every_copy=True)` returns them."""
        assert every_copy, "a rendering chooses among every copy, so it must ask for every copy"
        wanted = set(spec.split(",")) if spec else set(self.held)
        return [
            {"recording": self.rec(e), "encoding": e, "locator": str(src)}
            for e, (src, _) in self.held.items()
            if e in wanted
        ]

    def layout(self, encodings: list[str]) -> list[dict[str, Any]]:
        """Each copy's rendered path, uncontested."""
        return [{"encoding": e, "path": self.held[e][1]} for e in encodings]

    def choose(self, groups: list[list[str]]) -> list[dict[str, Any]]:
        """The first of each group, or what `chosen` says."""
        out = []
        for group in groups:
            first = min(group, key=list(self.held).index)
            default = {
                "encoding": first,
                "locator": str(self.held[first][0]),
                "tied": [],
                # The fixture's word for lossless is the rendered extension; the
                # library's is the format it read from the stream.
                "lossless": self.held[first][1].endswith(".flac"),
                "length_ms": 1000,
            }
            out.append(self.chosen.get(frozenset(group), default))
        return out


@pytest.fixture
def lib(tmp_path: Path) -> FakeLibrary:
    """Two masters on disk: a FLAC on one album and an MP3 on another."""
    root = tmp_path / "MusicLibrary"
    held = {}
    for enc, rel in [("e1", "Orbital/Diversions/05 Lush.flac"), ("e2", "Beirut/Lon Gisland/01 Elephant Gun.mp3")]:
        src = root / "masters" / f"{enc}.bin"
        src.parent.mkdir(parents=True, exist_ok=True)
        src.write_bytes(f"audio of {enc}".encode())
        held[enc] = (src, rel)
    return FakeLibrary([root], held)


def files_in(target: Path) -> set[str]:
    """What is on disk under a target, the manifest aside.

    Counted from the filesystem, not from anything the renderer reports.
    """
    return {str(p.relative_to(target)) for p in target.rglob("*") if p.is_file() and p.name != MANIFEST}


def test_a_second_render_of_an_unchanged_library_writes_nothing(lib: FakeLibrary, tmp_path: Path) -> None:
    """The manifest is what makes a rendering incremental."""
    target = tmp_path / "PlexMusic"
    first = render(lib, target, None, write=True)
    assert len(files_in(target)) == 2, "instrument: the first render put both files on disk"
    assert first.written == 2

    second = render(lib, target, None, write=True)
    assert second.written == 0, "nothing changed, so nothing is copied again"


def test_a_dry_run_writes_nothing(lib: FakeLibrary, tmp_path: Path) -> None:
    """Without `--write`, the target is not even created."""
    target = tmp_path / "PlexMusic"
    done = render(lib, target, None, write=False)
    assert done.written == 2, "instrument: the dry run did plan the writes"
    assert not target.exists(), "and made nothing, not even the directory"


def test_a_recording_leaving_the_query_is_named_by_the_dry_run_then_deleted(
    lib: FakeLibrary, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """A narrowed query deletes, and the dry run says so first."""
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    gone = target / lib.held["e2"][1]
    assert gone.is_file(), "instrument: the file to be deleted is there"
    capsys.readouterr()

    render(lib, target, "e1", write=False)
    assert f"delete  {lib.held['e2'][1]}" in capsys.readouterr().out
    assert gone.is_file(), "a dry run names the delete and does not do it"

    render(lib, target, "e1", write=True)
    assert not gone.exists()
    assert not gone.parent.exists(), "the album folder it left empty goes too"
    assert (target / lib.held["e1"][1]).is_file(), "and the recording still wanted stays"


def test_a_target_remembers_its_query(lib: FakeLibrary, tmp_path: Path) -> None:
    """No query given means the target's own, not the whole library."""
    target = tmp_path / "laptop"
    render(lib, target, "e1", write=True)
    assert files_in(target) == {lib.held["e1"][1]}, "instrument: the subset was rendered"

    again = render(lib, target, None, write=True)
    assert again.deleted == 0, "no query given means the target's own, not everything"
    assert again.written == 0


def test_a_directory_with_files_and_no_manifest_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    """Somebody else's music is never adopted as a rendering."""
    fresh = tmp_path / "fresh"
    fresh.mkdir()
    render(lib, fresh, None, write=False)  # instrument: an empty directory is a new rendering

    theirs = tmp_path / "Media" / "Music"
    theirs.mkdir(parents=True)
    (theirs / "something.mp3").write_bytes(b"somebody's music")
    with pytest.raises(Refused, match="not a rendering"):
        render(lib, theirs, None, write=True)
    assert files_in(theirs) == {"something.mp3"}


def test_a_target_inside_the_library_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    """A rendering under a library root would be scanned and read back."""
    beside = tmp_path / "PlexMusic"
    render(lib, beside, None, write=False)  # instrument: a sibling of the root is fine

    inside = lib.roots[0] / "Renderings" / "Plex"
    with pytest.raises(Refused, match="never scanned"):
        render(lib, inside, None, write=True)
    with pytest.raises(Refused, match="never scanned"):
        render(lib, tmp_path, None, write=True)  # a target holding the root is no better
    assert not inside.exists()


def test_a_file_the_manifest_does_not_name_is_never_overwritten(lib: FakeLibrary, tmp_path: Path) -> None:
    """Only the renderer's own files are ever replaced."""
    target = tmp_path / "PlexMusic"
    render(lib, target, "e1", write=True)
    stranger = target / lib.held["e2"][1]
    stranger.parent.mkdir(parents=True)
    stranger.write_bytes(b"put here by hand")

    done = render(lib, target, "e1,e2", write=True)
    assert stranger.read_bytes() == b"put here by hand"
    assert done.written == 0
    manifest = json.loads((target / MANIFEST).read_text())
    assert lib.held["e1"][1] in manifest["files"], "instrument: the manifest was read and kept"
    assert lib.held["e2"][1] not in manifest["files"]


def test_a_path_now_made_from_a_different_copy_is_rewritten(lib: FakeLibrary, tmp_path: Path) -> None:
    """The manifest row is compared whole, so a new source is a rewrite."""
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    at = target / lib.held["e1"][1]
    before = at.read_bytes()

    src, rel = lib.held.pop("e1")
    better = src.with_name("e3.bin")
    better.write_bytes(b"audio of a better copy")
    lib.held["e3"] = (better, rel)

    done = render(lib, target, None, write=True)
    assert done.written == 1
    assert at.read_bytes() != before
    assert at.read_bytes() == b"audio of a better copy"


def test_a_file_deleted_from_the_target_by_hand_is_written_again(lib: FakeLibrary, tmp_path: Path) -> None:
    """A manifest row whose file is gone is not trusted."""
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    at = target / lib.held["e1"][1]
    at.unlink()
    assert len(files_in(target)) == 1, "instrument: one of the two is really gone"

    done = render(lib, target, None, write=True)
    assert done.written == 1
    assert at.is_file()


def test_a_layout_that_disowns_copies_the_query_located_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    """A layout disowning copies the query located is refused.

    The first `layout` forgot which device it was on and called all 83 of one
    artist's local copies held elsewhere. A `--write` on that answer would
    delete a full rendering.
    """
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    assert len(files_in(target)) == 2, "instrument: a full rendering exists to be lost"

    def disowning(encodings: list[str]) -> list[dict[str, Any]]:
        return [{"encoding": e, "elsewhere": True} for e in encodings]

    lib.layout = disowning  # type: ignore[method-assign]
    with pytest.raises(Refused, match="and then that they are not"):
        render(lib, target, None, write=True)
    assert len(files_in(target)) == 2, "and nothing was deleted on its word"


def hold_twice(lib: FakeLibrary, enc: str, rel: str, body: bytes, *, of: str) -> Path:
    """A second copy of `of`'s recording, at `rel` when rendered."""
    src = lib.roots[0] / "masters" / f"{enc}.bin"
    src.write_bytes(body)
    lib.held[enc] = (src, rel)
    lib.recording[enc] = lib.rec(of)
    return src


def test_two_copies_of_one_track_render_as_one(lib: FakeLibrary, tmp_path: Path) -> None:
    """A FLAC and an MP3 of one track in one album are one appearance."""
    flac_at = lib.held["e1"][1]
    mp3_at = flac_at.removesuffix(".flac") + ".mp3"
    hold_twice(lib, "e1-mp3", mp3_at, b"the mp3", of="e1")

    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    on_disk = files_in(target)
    assert flac_at in on_disk, "instrument: the album rendered"
    assert mp3_at not in on_disk, "the other copy of that place is not rendered beside it"


def test_one_recording_on_two_albums_renders_on_both(lib: FakeLibrary, tmp_path: Path) -> None:
    """An album and a compilation are two appearances.

    Plex shows both whole, and choosing one copy per *recording* would leave a
    hole in one.
    """
    album = lib.held["e1"][1]
    sampler = "Various Artists/Diversions Sampler/03 Lush.mp3"
    hold_twice(lib, "e1-sampler", sampler, b"the sampler's copy", of="e1")

    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    assert {album, sampler} <= files_in(target)


def test_the_file_comes_from_the_copy_chosen_here(lib: FakeLibrary, tmp_path: Path) -> None:
    """The renderer copies what `choose` named, whichever copy that is."""
    flac_at = lib.held["e1"][1]
    mp3_at = flac_at.removesuffix(".flac") + ".mp3"
    src = hold_twice(lib, "e1-mp3", mp3_at, b"the copy chosen", of="e1")
    assert src.read_bytes() != lib.held["e1"][0].read_bytes(), "instrument: the two copies differ"
    lib.chosen[frozenset({"e1", "e1-mp3"})] = {"encoding": "e1-mp3", "locator": str(src), "tied": []}

    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    assert (target / mp3_at).read_bytes() == b"the copy chosen"
    assert flac_at not in files_in(target)


def test_a_tie_is_reported_and_still_rendered(
    lib: FakeLibrary, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """An id-only pick is said, and the rendering goes ahead with it."""
    src = str(lib.held["e1"][0])
    lib.chosen[frozenset({"e1"})] = {"encoding": "e1", "locator": src, "tied": ["e9"], "tied_at": ["/m/e9.mp3"]}

    target = tmp_path / "PlexMusic"
    done = render(lib, target, None, write=False)
    out = capsys.readouterr().out
    assert done.written == 2, "instrument: the tie did not stop the rendering"
    assert f"tie  {lib.held['e1'][1]}\n         took  {src}\n         over  /m/e9.mp3" in out
    assert "1 chosen by id alone" in out


def test_a_copy_the_query_located_and_choose_disowned_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    """`choose` saying a located copy is not here stops the run."""
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    assert len(files_in(target)) == 2, "instrument: a full rendering exists to be lost"

    lib.chosen[frozenset({"e2"})] = {}
    with pytest.raises(Refused, match="and then that they are not"):
        render(lib, target, None, write=True)
    assert len(files_in(target)) == 2


def test_copies_of_one_recording_contesting_a_path_are_a_choice(lib: FakeLibrary, tmp_path: Path) -> None:
    """Two copies of one recording contesting a path are one place.

    The layout refuses a path two copies want, which is right for moving files
    and wrong here: the better copy is rendered.
    """
    path = lib.held["e1"][1]
    hold_twice(lib, "e1-again", path, b"another copy", of="e1")
    contested = lib.layout

    def contesting(encodings: list[str]) -> list[dict[str, Any]]:
        rows = contested(encodings)
        for r in rows:
            if r["encoding"] in {"e1", "e1-again"}:
                r.update(refused=f"two copies want {path}", contested=True)
        return rows

    lib.layout = contesting  # type: ignore[method-assign]
    target = tmp_path / "PlexMusic"
    done = render(lib, target, None, write=True)
    assert path in files_in(target)
    assert done.written == 2, "instrument: both places rendered, the contested one once"


def test_two_recordings_contesting_a_path_stay_refused(
    lib: FakeLibrary, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """Two recordings wanting one path is a collision, not a choice."""
    path = lib.held["e1"][1]
    src = lib.roots[0] / "masters" / "other.bin"
    src.write_bytes(b"a different recording")
    lib.held["other"] = (src, path)
    contested = lib.layout

    def contesting(encodings: list[str]) -> list[dict[str, Any]]:
        rows = contested(encodings)
        for r in rows:
            if r["encoding"] in {"e1", "other"}:
                r.update(refused=f"two copies want {path}", contested=True)
        return rows

    lib.layout = contesting  # type: ignore[method-assign]
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    assert files_in(target) == {lib.held["e2"][1]}, "instrument: the uncontested file rendered"
    assert "two recordings want" in capsys.readouterr().out


def test_one_recording_twice_on_one_album_renders_twice(lib: FakeLibrary, tmp_path: Path) -> None:
    """A disc really can play one recording twice.

    Grouping by folder kept one of `Furious Angels`' two slots for the same
    instrumental.
    """
    first = "Rob Dougan/Furious Angels/1-08 Instrumental.mp3"
    second = "Rob Dougan/Furious Angels/2-06 Instrumental.mp3"
    src, _ = lib.held["e1"]
    lib.held["e1"] = (src, first)
    hold_twice(lib, "e1-disc2", second, b"disc two", of="e1")

    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    assert {first, second} <= files_in(target)


class Encoder:
    """A transcoder that writes a marker and counts its calls.

    The instrument for "encoded once", which no file on disk could say.
    """

    def __init__(self, *, fail: bool = False) -> None:
        """Count calls; with `fail`, raise as a short encode would."""
        self.calls: list[Path] = []
        self.fail = fail

    def __call__(self, source: Path, dest: Path, encode: Encode, length_ms: int | None) -> None:  # noqa: ARG002 — the transcoder's signature
        """Encode, or fail as a short encode does."""
        self.calls.append(source)
        if self.fail:
            dest.write_bytes(b"half an encode")
            msg = "a short encode"
            raise ValueError(msg)
        dest.write_bytes(b"mp3 of " + source.read_bytes() + encode.key.encode())


def mp3_of(path: str) -> str:
    """Where a FLAC's encode lands."""
    return path.removesuffix(".flac") + ".mp3"


def test_lossless_renders_as_its_encode_and_is_encoded_once(lib: FakeLibrary, tmp_path: Path) -> None:
    """A FLAC becomes an MP3, encoded once across runs and targets."""
    enc = Encoder()
    t = Transcoding(tmp_path / "cache", enc, workers=2)
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True, encode="mp3-320", transcoding=t)
    flac_at = lib.held["e1"][1]
    assert enc.calls == [lib.held["e1"][0]], "instrument: the FLAC went through the encoder, and only it"
    assert mp3_of(flac_at) in files_in(target)
    assert flac_at not in files_in(target)
    assert (target / mp3_of(flac_at)).read_bytes().startswith(b"mp3 of ")

    render(lib, target, None, write=True, transcoding=t)
    assert len(enc.calls) == 1, "a second render reuses the encode"
    assert mp3_of(flac_at) in files_in(target), "and remembers the policy rather than copying the FLAC"
    assert flac_at not in files_in(target)
    elsewhere = tmp_path / "laptop"
    render(lib, elsewhere, None, write=True, encode="mp3-320", transcoding=t)
    assert len(enc.calls) == 1, "a second target shares the cache"
    assert mp3_of(flac_at) in files_in(elsewhere)


def test_a_lossy_master_is_never_given_to_the_encoder(lib: FakeLibrary, tmp_path: Path) -> None:
    """Lossy to lossy loses twice for nothing, so a lossy master is copied."""
    enc = Encoder()
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True, encode="mp3-320", transcoding=Transcoding(tmp_path / "c", enc))
    mp3_master = lib.held["e2"]
    assert len(enc.calls) == 1, "instrument: the encoder ran for the lossless one"
    assert mp3_master[0] not in enc.calls
    assert (target / mp3_master[1]).read_bytes() == mp3_master[0].read_bytes(), "copied as it is"


def test_changing_the_policy_re_encodes_and_rewrites(
    lib: FakeLibrary, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """The encoder key is in the row and the cache key, so a new policy re-encodes."""
    enc = Encoder()
    t = Transcoding(tmp_path / "cache", enc)
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True, encode="mp3-320", transcoding=t)
    at = target / mp3_of(lib.held["e1"][1])
    before = at.read_bytes()

    done = render(lib, target, None, write=True, encode="mp3-v0", transcoding=t)
    assert "encode changes: mp3-320 → mp3-v0" in capsys.readouterr().out
    assert len(enc.calls) == 2
    assert done.written == 1, "the transcode is rewritten; the copied MP3 is not"
    assert at.read_bytes() != before


def test_a_failed_encode_writes_nothing_for_that_file(lib: FakeLibrary, tmp_path: Path) -> None:
    """A failed encode leaves no file, no row and no partial cache entry."""
    t = Transcoding(tmp_path / "cache", Encoder(fail=True))
    target = tmp_path / "PlexMusic"
    done = render(lib, target, None, write=True, encode="mp3-320", transcoding=t)
    assert files_in(target) == {lib.held["e2"][1]}, "instrument: the rest of the rendering still landed"
    assert [p for p, _ in done.failed] == [mp3_of(lib.held["e1"][1])]
    assert not any((tmp_path / "cache").rglob("*.mp3")), "and no partial encode is left to be mistaken for one"
    manifest = json.loads((target / MANIFEST).read_text())
    assert mp3_of(lib.held["e1"][1]) not in manifest["files"]


def test_an_encode_landing_on_another_recordings_file_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    """An encode landing on another recording's path is refused.

    The layout compared `05 Lush.flac` with nothing; its encode is
    `05 Lush.mp3`, which another recording already wants.
    """
    src = lib.roots[0] / "masters" / "other.bin"
    src.write_bytes(b"another recording")
    lib.held["other"] = (src, mp3_of(lib.held["e1"][1]))

    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True, encode="mp3-320", transcoding=Transcoding(tmp_path / "c", Encoder()))
    assert files_in(target) == {lib.held["e2"][1]}, "instrument: the uncontested file rendered"


def test_without_a_policy_nothing_is_transcoded(lib: FakeLibrary, tmp_path: Path) -> None:
    """A target that names no policy copies, as before #334."""
    enc = Encoder()
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True, transcoding=Transcoding(tmp_path / "c", enc))
    assert lib.held["e1"][1] in files_in(target), "instrument: the FLAC rendered, as itself"
    assert enc.calls == []


def test_an_unknown_policy_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    """A policy name that does not exist is refused, not ignored."""
    with pytest.raises(Refused, match="no encode policy"):
        render(lib, tmp_path / "PlexMusic", None, write=False, encode="mp3-1000")


def test_an_encode_the_wrong_length_is_refused(tmp_path: Path) -> None:
    """A short encode is a broken file, not a smaller one."""
    check_length(tmp_path / "x.mp3", 200_000, 200_400)  # instrument: a frame of padding passes
    with pytest.raises(ValueError, match="plays 150000 ms where its source plays 200000"):
        check_length(tmp_path / "x.mp3", 200_000, 150_000)


@pytest.mark.skipif(shutil.which("ffmpeg") is None, reason="needs ffmpeg, which the renderer itself needs")
def test_ffmpeg_makes_an_mp3_as_long_as_its_source(tmp_path: Path) -> None:
    """The one test of the real encoder: a two-second WAV in, an MP3 out."""
    src = tmp_path / "tone.wav"
    rate = 44_100
    with wave.open(str(src), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(b"".join(struct.pack("<h", int(8000 * math.sin(i / 20))) for i in range(rate * 2)))
    dest = tmp_path / "tone.mp3"
    ffmpeg(src, dest, ENCODES["mp3-320"], 2000)
    assert dest.stat().st_size > 10_000, "a real encode, not an empty file"
    assert abs(probe_ms(dest) - 2000) < 100
    with pytest.raises(ValueError, match="plays"):
        ffmpeg(src, tmp_path / "again.mp3", ENCODES["mp3-320"], 9000)



def test_a_choice_that_does_not_say_lossless_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    """A stale `gmuse_core` omits `lossless`, and absent must not read as lossy."""
    lib.chosen[frozenset({"e1"})] = {"encoding": "e1", "locator": str(lib.held["e1"][0]), "tied": []}
    render(lib, tmp_path / "copying", None, write=False)  # instrument: without a policy it does not matter
    with pytest.raises(Refused, match="which copies are lossless"):
        render(lib, tmp_path / "PlexMusic", None, write=False, encode="mp3-320")
