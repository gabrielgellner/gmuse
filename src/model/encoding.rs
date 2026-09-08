//! Encodings, copies, and which one represents — issue #75,
//! `domain-model.md` §2.
//!
//! An [`Encoding`] is a **serialization**; a [`Holding`] is a copy of it on a
//! device. DCAT names the same concept `Distribution` — "a specific
//! representation or serialization of a dataset, *not* a physical copy" —
//! and carries a checksum, which here is `audio_id`. The copy is FRBR's
//! `Item`. Nothing in the state of the art agrees on the noun; see
//! `domain-model.md` §7 for the five standards and their five words.
//!
//! Keeping them apart is what lets spectral cutoff and ReplayGain be measured
//! **once** rather than once per machine per file, and what lets "is there a
//! good copy of this anywhere" be answered on a laptop holding 2% of the
//! library.
//!
//! ## Three questions, one stored answer
//!
//! ```text
//! canonical(rec)   the encoding that represents this recording   derived, same everywhere
//! for_playback     which copy this machine opens right now       derived, differs per device
//! rec.pinned       a human overruling the first                  stored, empty almost always
//! ```
//!
//! The default is computed and never stored. Persisting it would be caching a
//! fold as a synced field: stale the moment a better encode arrives, rewritten
//! by every device, and syncing derived data.

use std::collections::BTreeMap;

use crate::audio_id::AudioId;
use crate::model::entity::{Device, Recording};
use crate::model::id::{Id, Ref};

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the analyser version a re-measure pass would compare against, and there is no pass"
)]
/// Which analyser produced a measurement.
///
/// Bumped whenever the FFT parameters or the ReplayGain implementation change.
/// Without it, "measured once, travels forever" quietly becomes "measured once,
/// with the old code" — and there is no way to find the rows that need doing
/// again short of redoing all of them.
pub const ANALYSER: u16 = 1;

/// A measurement, and what measured it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Measured<T> {
    pub value: T,
    pub analyser: u16,
}

impl<T> Measured<T> {
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — Measured carries when a measurement was taken; nothing re-measures yet"
    )]
    pub const fn now(value: T) -> Self {
        Measured {
            value,
            analyser: ANALYSER,
        }
    }

    /// Whether this was produced by something older than the current analyser.
    pub const fn is_stale(&self) -> bool {
        self.analyser < ANALYSER
    }
}

/// ReplayGain, in hundredths of a decibel.
///
/// Integers for the reason tempo is: two runs of an analyser differ in the last
/// float digit and are the same loudness, and a level that compares
/// approximately is a level that cannot be a map key or an `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Gain {
    pub track_cdb: i32,
    pub peak_cdb: i32,
}

/// ReplayGain **as a tagger wrote it**, in integers.
///
/// Distinct from [`Gain`], which is what an *analyser* measured. This is a
/// claim in the file's tags, and every one of the 6,500 files here carries
/// both a track and an album value.
///
/// Integers for the reason `Gain` and `Bpm` are: a level that compares
/// approximately cannot be `Eq`, and a `Fact` must be — records are
/// deduplicated by wire form and two devices reading one file have to agree
/// exactly.
///
/// **Gain is hundredths of a decibel; peak is ten-thousandths of unity.** The
/// units differ because the quantities do — a gain is logarithmic and a peak
/// is a fraction of full scale — and 1e-4 of full scale is far below anything
/// the clipping guard can act on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Levels {
    pub track_cdb: Option<i32>,
    pub track_peak_e4: Option<u32>,
    pub album_cdb: Option<i32>,
    pub album_peak_e4: Option<u32>,
}

impl Levels {
    pub fn is_empty(&self) -> bool {
        *self == Levels::default()
    }

    /// Back into the floats a fader wants.
    pub fn track_db(&self) -> Option<f32> {
        self.track_cdb.map(|c| c as f32 / 100.0)
    }
    pub fn album_db(&self) -> Option<f32> {
        self.album_cdb.map(|c| c as f32 / 100.0)
    }
    pub fn track_peak(&self) -> Option<f32> {
        self.track_peak_e4.map(|p| p as f32 / 10_000.0)
    }
    pub fn album_peak(&self) -> Option<f32> {
        self.album_peak_e4.map(|p| p as f32 / 10_000.0)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Format {
    Flac,
    Alac,
    Wav,
    Mp3,
    Aac,
    Vorbis,
    Opus,
}

impl Format {
    /// The extension a file of this format carries — #132.
    ///
    /// **The one the tools already expect**, since the point of keeping an
    /// extension at all is that everything which opens files by name still
    /// works. `m4a` for both AAC and ALAC, which is what they are in.
    pub const fn extension(self) -> &'static str {
        match self {
            Format::Flac => "flac",
            Format::Alac | Format::Aac => "m4a",
            Format::Wav => "wav",
            Format::Mp3 => "mp3",
            Format::Vorbis => "ogg",
            Format::Opus => "opus",
        }
    }

    pub const fn is_lossless(self) -> bool {
        matches!(self, Format::Flac | Format::Alac | Format::Wav)
    }

    /// A fixed global ordering, lossless first.
    ///
    /// **Not** the `playback_prefers` setting. They sound alike and are
    /// opposite in intent: this says what best *represents* the recording
    /// everywhere, the setting says what this laptop would rather *open*.
    const fn rank(self) -> u8 {
        match self {
            Format::Flac => 0,
            Format::Alac => 1,
            Format::Wav => 2,
            Format::Mp3 => 3,
            Format::Aac => 4,
            Format::Vorbis => 5,
            Format::Opus => 6,
        }
    }
}

/// What the numbers say about an encoding, as against what it claims.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Verdict {
    /// The declared bitrate is not supported by the audio — a lossy file
    /// re-encoded upward, which the library has real examples of.
    Suspect,
    /// Nothing has been measured yet.
    Unmeasured,
    Good,
}

/// An encoding of a recording.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Encoding {
    pub id: Id<Encoding>,
    pub audio: AudioId,
    pub recording: Option<Ref<Recording>>,
    pub format: Format,
    pub bitrate: u32,
    /// Samples per second, and how many channels.
    ///
    /// Facts about the bytes, alongside the bitrate. Here because a display
    /// row asks for both and a librarian that cannot say them cannot replace
    /// the scanner as what a row is drawn from (#86).
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    /// How long these bytes play.
    ///
    /// **Per encoding, and not reconciled with any other.** An edit, a radio
    /// cut and a truncated rip are one recording and three durations, and
    /// picking between them would be answering a question nobody asked: what
    /// the player wants to know is how long *the thing it opened* is.
    pub length: std::time::Duration,
    /// **A mastering tool re-encoded this, and the path said so** — #143.
    ///
    /// Kept beside the measurement because it answers what the measurement
    /// cannot: a competent 320-to-320 pass leaves no wall for the cutoff test
    /// to find, and 130 of this collection's 144 marked files measure clean.
    ///
    /// It does not move [`Encoding::verdict`], deliberately. Measured here,
    /// 9.7% of marked files fail the cutoff test against 6.2% of the rest — a
    /// complete record of provenance and a poor predictor of damage, and those
    /// are different jobs. What it is for is being *askable*.
    pub processed: bool,
    pub cutoff_hz: Option<Measured<u32>>,
    pub gain: Option<Measured<Gain>>,
    /// ReplayGain as the tags claim it — what a fader applies. Not
    /// [`Encoding::gain`], which is what an analyser found.
    pub levels: Levels,
    /// Encoder delay and padding, for correcting a cue point across encodings
    /// of one source. The canonical timeline is the decoded signal with this
    /// removed.
    pub encoder_delay: Option<u32>,
}

impl Encoding {
    /// Whether the file is as good as it says it is.
    ///
    /// The worked example in §9 is a real file here: *320k declared, 15.7 kHz*
    /// — a lossy source re-encoded upward, which measures as a low cutoff under
    /// a high claimed bitrate. Lossless is exempt because a lossless file's
    /// cutoff is a property of the source, not of the encoder.
    pub fn verdict(&self) -> Verdict {
        let Some(cutoff) = self.cutoff_hz else {
            return Verdict::Unmeasured;
        };
        if self.format.is_lossless() {
            return Verdict::Good;
        }
        // A real 320k MP3 keeps content to about 20 kHz; a 128k source
        // re-encoded to 320k still stops around 16.
        let expected = match self.bitrate {
            0..=159 => 16_000,
            160..=255 => 18_000,
            _ => 19_000,
        };
        if cutoff.value + 1_000 < expected {
            Verdict::Suspect
        } else {
            Verdict::Good
        }
    }
}

/// A copy of an encoding, on one device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Holding {
    pub encoding: Ref<Encoding>,
    pub device: Ref<Device>,
    /// Interpreted by the device's kind — a path on a disk, a rating key on a
    /// server. Opaque here on purpose.
    pub locator: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeviceKind {
    LocalDisk,
    Share,
    PlexServer,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceRecord {
    pub id: Id<Device>,
    pub name: String,
    pub kind: DeviceKind,
}

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the canonical-ordering policy version, read by the pin and by nothing else"
)]
/// Bumped when the ordering below changes.
///
/// Canonical choices feed the re-buy list and the quality tables, so a changed
/// ordering should read as an explainable diff rather than as a mystery.
///
/// 2 — measurement stopped outranking the format (#110).
pub const POLICY: u16 = 2;

/// The ordering that decides which encoding represents a recording.
///
/// Free-standing so that totality can be checked directly rather than through a
/// collection that happens to be sorted already — an ordering that is only
/// deterministic because of how it was reached is not total, it is lucky.
///
/// **The signature is the guarantee.** It takes an `&Encoding` and nothing
/// else, so there is no holding and no setting in scope to read: every replica
/// computes the same answer without coordinating, and a later edit *cannot*
/// quietly make it machine-dependent without changing this line.
pub fn canonical_key(
    e: &Encoding,
) -> (bool, std::cmp::Reverse<u8>, u32, u32, Verdict, Id<Encoding>) {
    (
        // **"We know this is bad" and "we have not looked" are different**,
        // and the ordering used to collapse them: `Verdict` sorted first, so a
        // measured 320k MP3 outranked an unmeasured FLAC. Measurement status
        // is a fact about our knowledge rather than about the audio, and
        // sorting on it meant running the quality pass over part of the
        // library *changed which copy represents a recording* — in the wrong
        // direction (#110).
        //
        // Suspect still sinks to the bottom outright. A suspect FLAC is a
        // lossy file upsampled to lossless, this library has real examples,
        // and it must not beat a good MP3.
        e.verdict() != Verdict::Suspect,
        std::cmp::Reverse(e.format.rank()),
        e.bitrate,
        // Below the format, so measuring only ever breaks a tie between copies
        // that are otherwise equal.
        e.cutoff_hz.map(|c| c.value).unwrap_or(0),
        e.verdict(),
        e.id,
    )
}

/// The encodings and copies, held flat.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Encodings {
    pub encodings: BTreeMap<Id<Encoding>, Encoding>,
    pub holdings: Vec<Holding>,
    pub devices: BTreeMap<Id<Device>, DeviceRecord>,
}

impl Encodings {
    /// Every encoding of a recording.
    /// Every encoding, grouped by the recording it holds.
    ///
    /// **Built once, because [`Self::of`] is a linear scan** (#123). One
    /// question about one recording is fine; `draw` asks about every row, and
    /// on 6,500 encodings that is 6,500 full passes — and each one allocated a
    /// `Vec` and sorted it to answer "is this empty".
    pub fn by_recording(&self) -> std::collections::BTreeMap<Id<Recording>, Vec<&Encoding>> {
        let mut out: std::collections::BTreeMap<Id<Recording>, Vec<&Encoding>> = Default::default();
        for encoding in self.encodings.values() {
            if let Some(recording) = encoding.recording.map(|r| r.id()) {
                out.entry(recording).or_default().push(encoding);
            }
        }
        for found in out.values_mut() {
            found.sort_by_key(|e| e.id);
        }
        out
    }

    pub fn of(&self, recording: Id<Recording>) -> Vec<&Encoding> {
        let mut found: Vec<_> = self
            .encodings
            .values()
            .filter(|e| e.recording.map(|r| r.id()) == Some(recording))
            .collect();
        found.sort_by_key(|e| e.id);
        found
    }

    /// The encoding that represents this recording.
    ///
    /// **Total, and blind to the machine it runs on.** The trailing id makes
    /// the winner deterministic between near-identical candidates rather than
    /// whichever the iterator reached first, and reading no holding and no
    /// setting means every replica computes the same answer without
    /// coordinating — agreement through determinism, the same trick as
    /// content-derived ids.
    pub fn canonical(&self, recording: Id<Recording>) -> Option<&Encoding> {
        self.of(recording)
            .into_iter()
            .max_by_key(|e| canonical_key(e))
    }

    /// The best copy this machine can open right now.
    ///
    /// Reachability is a hard filter, not a preference — you cannot play what
    /// is not here. Everything after it is the per-device setting, which is
    /// per-device because a laptop wanting the transcode and a desktop wanting
    /// the master is a preference about the machine, not about the music.
    ///
    /// `pinned` overrules the lot, when the pinned encoding is actually
    /// reachable.
    pub fn for_playback(
        &self,
        recording: Id<Recording>,
        pinned: Option<Id<Encoding>>,
        reachable: &[Id<Device>],
        prefers: &[Format],
    ) -> Option<&Encoding> {
        let here = |e: &Encoding| {
            self.holdings
                .iter()
                .any(|h| h.encoding.id() == e.id && reachable.contains(&h.device.id()))
        };
        let candidates: Vec<_> = self.of(recording).into_iter().filter(|e| here(e)).collect();

        if let Some(p) = pinned
            && let Some(e) = candidates.iter().find(|e| e.id == p)
        {
            return Some(e);
        }
        candidates.into_iter().max_by_key(|e| {
            let wanted = prefers
                .iter()
                .position(|f| *f == e.format)
                .map(|i| prefers.len() - i)
                .unwrap_or(0);
            (wanted, e.bitrate, e.id)
        })
    }

    /// Every device claiming to hold this encoding, and where.
    pub fn holders(&self, encoding: Id<Encoding>) -> Vec<&Holding> {
        self.holdings
            .iter()
            .filter(|h| h.encoding.id() == encoding)
            .collect()
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — targets a re-measure pass at stale rows, and there is no pass"
    )]
    /// Measurements taken by an older analyser, so a re-measure pass can target
    /// them instead of the whole library.
    pub fn stale_measurements(&self) -> Vec<Id<Encoding>> {
        self.encodings
            .values()
            .filter(|e| {
                e.cutoff_hz.is_some_and(|c| c.is_stale()) || e.gain.is_some_and(|g| g.is_stale())
            })
            .map(|e| e.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(hash: u128) -> AudioId {
        AudioId { scheme: 1, hash }
    }

    fn encoding(rec: Id<Recording>, format: Format, bitrate: u32, cutoff: Option<u32>) -> Encoding {
        let a =
            audio(bitrate as u128 * 1_000 + cutoff.unwrap_or(0) as u128 + format.rank() as u128);
        Encoding {
            id: Id::of_audio(&a),
            audio: a,
            recording: Some(Ref::to(rec)),
            format,
            bitrate,
            sample_rate: Some(44_100),
            channels: Some(2),
            length: std::time::Duration::from_secs(200),
            processed: false,
            cutoff_hz: cutoff.map(Measured::now),
            gain: None,
            levels: Levels::default(),
            encoder_delay: None,
        }
    }

    /// Give the *loser* the higher id, so the tiebreak pulls against the term
    /// under test.
    ///
    /// Without this a two-candidate test passes about half the time by luck of
    /// hashing, and quietly stops testing the thing it is named after.
    fn stack_the_ids_against(winner: &mut Encoding, loser: &mut Encoding) {
        if winner.id > loser.id {
            std::mem::swap(&mut winner.id, &mut loser.id);
            std::mem::swap(&mut winner.audio, &mut loser.audio);
        }
        assert!(loser.id > winner.id, "the id now favours the wrong file");
    }

    fn holding(e: &Encoding, device: Id<Device>, at: &str) -> Holding {
        Holding {
            encoding: Ref::to(e.id),
            device: Ref::to(device),
            locator: at.into(),
        }
    }

    /// **The re-buy list's real question.** A 320k file that stops at 15.7 kHz
    /// is a lossy source re-encoded upward, and the declared bitrate is a lie.
    #[test]
    fn a_transcode_is_caught_by_its_shape_not_its_claim() {
        let rec = Id::<Recording>::derived(b"isnt-love");
        let lying = encoding(rec, Format::Mp3, 320, Some(15_700));
        let honest = encoding(rec, Format::Mp3, 320, Some(19_800));

        assert_eq!(lying.verdict(), Verdict::Suspect);
        assert_eq!(honest.verdict(), Verdict::Good);
        assert_eq!(
            encoding(rec, Format::Mp3, 128, Some(15_700)).verdict(),
            Verdict::Good,
            "a 128k file stopping at 15.7 kHz is doing exactly what it claims"
        );
        assert_eq!(
            encoding(rec, Format::Flac, 900, Some(15_700)).verdict(),
            Verdict::Good,
            "a lossless cutoff is a fact about the source, not the encoder"
        );
    }

    /// Cutoff decides between files that are alike in every other respect —
    /// the whole reason the quality pass was run.
    #[test]
    fn the_better_measured_file_wins_between_equals() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        let mut worse = encoding(rec, Format::Mp3, 320, Some(19_200));
        let mut better = encoding(rec, Format::Mp3, 320, Some(19_800));
        assert_eq!(worse.verdict(), better.verdict(), "both honest");
        assert_eq!(
            worse.bitrate, better.bitrate,
            "and alike but for the measurement"
        );
        stack_the_ids_against(&mut better, &mut worse);
        let better_id = better.id;
        e.encodings.insert(worse.id, worse);
        e.encodings.insert(better.id, better);

        assert_eq!(e.canonical(rec).unwrap().id, better_id);
    }

    /// Format decides between files alike in everything measured — which is
    /// what makes the master represent the recording even when a transcode of
    /// it measures identically well.
    #[test]
    fn lossless_represents_when_nothing_else_separates_them() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        let mut lossy = encoding(rec, Format::Mp3, 320, Some(20_000));
        let mut lossless = encoding(rec, Format::Flac, 320, Some(20_000));
        assert_eq!(canonical_key(&lossy).0, canonical_key(&lossless).0);
        assert_eq!(lossy.bitrate, lossless.bitrate);
        stack_the_ids_against(&mut lossless, &mut lossy);
        let want = lossless.id;
        e.encodings.insert(lossy.id, lossy);
        e.encodings.insert(lossless.id, lossless);

        assert_eq!(e.canonical(rec).unwrap().id, want);
    }

    /// **Measurement is a fact about our knowledge, not about the audio.**
    ///
    /// `Verdict` used to sort first, so a measured 320k MP3 outranked an
    /// unmeasured FLAC — meaning that running the quality pass over part of
    /// the library changed which copy represents a recording, in the wrong
    /// direction (#110).
    #[test]
    fn an_unmeasured_master_still_represents_the_recording() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        let mut looked_at = encoding(rec, Format::Mp3, 320, Some(20_000));
        let mut untouched = encoding(rec, Format::Flac, 1000, None);
        assert_eq!(looked_at.verdict(), Verdict::Good);
        assert_eq!(untouched.verdict(), Verdict::Unmeasured);
        stack_the_ids_against(&mut untouched, &mut looked_at);
        let want = untouched.id;
        e.encodings.insert(looked_at.id, looked_at);
        e.encodings.insert(untouched.id, untouched);

        assert_eq!(e.canonical(rec).unwrap().id, want);
    }

    /// **A copy we believe is lying sinks below everything.**
    ///
    /// This is why the verdict cannot simply be demoted under the format: a
    /// file claiming 320k with a 15 kHz spectrum is a bad transcode wearing a
    /// good bitrate, and it should lose to an honest copy in a worse format at
    /// half the rate.
    ///
    /// Note the shape of the case. A *lossless* file cannot currently be
    /// `Suspect` at all — `verdict` returns `Good` for any measured lossless
    /// format without looking at the spectrum — so an upsampled lossy-to-FLAC
    /// file reads as good. That is a hole in `verdict`, not in this ordering,
    /// and it is why the case here is a lying MP3 rather than a lying FLAC.
    #[test]
    fn a_copy_that_lies_about_its_bitrate_loses_to_an_honest_one() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        // 320k claimed, 15 kHz delivered: a 128k source re-encoded upward.
        let mut lying = encoding(rec, Format::Mp3, 320, Some(15_000));
        let mut honest = encoding(rec, Format::Opus, 128, Some(19_500));
        assert_eq!(lying.verdict(), Verdict::Suspect);
        assert_ne!(honest.verdict(), Verdict::Suspect);
        assert!(
            Format::Opus.rank() > Format::Mp3.rank() && honest.bitrate < lying.bitrate,
            "the honest copy has to be worse on every other term, or this \
             test passes without the verdict doing anything"
        );
        stack_the_ids_against(&mut honest, &mut lying);
        let want = honest.id;
        e.encodings.insert(lying.id, lying);
        e.encodings.insert(honest.id, honest);

        assert_eq!(e.canonical(rec).unwrap().id, want);
    }

    /// **Measuring a copy does not move the canonical**, unless what it finds
    /// is that the copy is suspect. That is the property the reordering is
    /// for: the quality pass answers a question about the bytes, and it should
    /// not silently rewrite which copy the library shows.
    #[test]
    fn measuring_a_copy_does_not_change_which_one_represents_it() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        let flac = encoding(rec, Format::Flac, 1000, None);
        let mp3 = encoding(rec, Format::Mp3, 320, None);
        let (flac_id, mp3_id) = (flac.id, mp3.id);
        e.encodings.insert(flac_id, flac);
        e.encodings.insert(mp3_id, mp3);
        assert_eq!(e.canonical(rec).unwrap().id, flac_id);

        // The analyser gets to the MP3 first and finds nothing wrong with it.
        e.encodings.get_mut(&mp3_id).unwrap().cutoff_hz = Some(Measured::now(20_000));
        assert_eq!(
            e.canonical(rec).unwrap().id,
            flac_id,
            "looking at one copy changed which copy represents the recording"
        );
    }

    /// **The ordering is pinned, so `POLICY` means something.**
    ///
    /// The constant promises that "a changed ordering should read as an
    /// explainable diff rather than as a mystery", and nothing read it — a
    /// version nobody checks is a comment with extra steps. This is what
    /// makes the promise enforceable: change `canonical_key` and this fails,
    /// and the way to make it pass is to say what changed and bump `POLICY`.
    ///
    /// The set is chosen so every term of the tuple decides at least one
    /// pair: a liar, two formats, two bitrates, and one measured against one
    /// not.
    #[test]
    fn the_canonical_ordering_is_pinned() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        let cases = [
            ("suspect mp3", Format::Mp3, 320, Some(15_000)),
            ("honest opus", Format::Opus, 128, Some(19_500)),
            ("unmeasured flac", Format::Flac, 1000, None),
            ("measured flac", Format::Flac, 1000, Some(22_000)),
            ("good mp3", Format::Mp3, 320, Some(20_000)),
            // Format and bitrate disagree here, and nothing else does: a
            // better format at a worse rate. Without this pair, swapping the
            // two terms changes no answer and the pin proves nothing about
            // their order.
            ("low mp3", Format::Mp3, 96, Some(15_500)),
            ("high aac", Format::Aac, 256, Some(19_000)),
            // Bitrate and cutoff disagree here, and the type cannot keep them
            // apart — both are `u32`, so only a case where they point
            // different ways pins which comes first. Both are honest: 320
            // expects 19 kHz and 256 expects 19 kHz, and each clears it.
            ("fat mp3", Format::Mp3, 320, Some(19_200)),
            ("wide mp3", Format::Mp3, 256, Some(21_000)),
        ];
        let mut named = Vec::new();
        for (name, format, bitrate, cutoff) in cases {
            let enc = encoding(rec, format, bitrate, cutoff);
            named.push((name, enc.id));
            e.encodings.insert(enc.id, enc);
        }
        let mut order: Vec<&str> = named.iter().map(|(n, _)| *n).collect();
        order.sort_by_key(|n| {
            let id = named.iter().find(|(m, _)| m == n).unwrap().1;
            std::cmp::Reverse(canonical_key(&e.encodings[&id]))
        });

        assert_eq!(
            order,
            vec![
                "measured flac",
                "unmeasured flac",
                // `low mp3` above `high aac` is the format outranking the
                // bitrate, which is the whole reason the pair is here.
                "good mp3",
                // `fat mp3` above `wide mp3` is the bitrate outranking the
                // measurement, which the tuple's types cannot express.
                "fat mp3",
                "wide mp3",
                "low mp3",
                "high aac",
                "honest opus",
                "suspect mp3",
            ],
            "the canonical ordering changed — say what changed and bump POLICY \
             (it is {POLICY})"
        );
        assert_eq!(POLICY, 2, "the ordering above is what POLICY 2 means");
    }

    /// Every replica computes the same canonical without talking to anyone.
    #[test]
    fn canonical_is_the_same_answer_everywhere() {
        let rec = Id::<Recording>::derived(b"callisto");
        let mut e = Encodings::default();
        for enc in [
            encoding(rec, Format::Mp3, 320, Some(19_800)),
            encoding(rec, Format::Mp3, 128, Some(15_900)),
            encoding(rec, Format::Flac, 900, Some(22_000)),
        ] {
            e.encodings.insert(enc.id, enc);
        }
        let before = e.canonical(rec).unwrap().id;
        assert_eq!(e.encodings[&before].format, Format::Flac);

        // Now hold *only the worst* copy, on two devices. A policy that looked
        // at holdings at all would change its mind; this one cannot see them.
        let worst = e.of(rec).into_iter().min_by_key(|x| x.bitrate).unwrap().id;
        for name in [&b"laptop"[..], &b"nas"[..]] {
            e.holdings.push(Holding {
                encoding: Ref::to(worst),
                device: Ref::to(Id::<Device>::derived(name)),
                locator: "/x.mp3".into(),
            });
        }

        assert_eq!(
            e.canonical(rec).unwrap().id,
            before,
            "canonical reads no holding, so having copies cannot promote one"
        );
    }

    /// A suspect file loses to an honest one even when it claims more.
    #[test]
    fn a_lie_does_not_win_on_its_claimed_bitrate() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        for enc in [
            encoding(rec, Format::Mp3, 320, Some(15_700)), // suspect
            encoding(rec, Format::Mp3, 192, Some(19_100)), // honest, lower claim
        ] {
            e.encodings.insert(enc.id, enc);
        }
        assert_eq!(e.canonical(rec).unwrap().bitrate, 192);
    }

    /// And it loses even when it measures *better*, which is the case the
    /// verdict has to carry on its own: a 320k file stopping at 17.5 kHz is
    /// still lying, and a 192k file stopping at 17.0 kHz is telling the truth.
    #[test]
    fn a_lie_loses_to_an_honest_file_that_measures_worse() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        let lying = encoding(rec, Format::Mp3, 320, Some(17_500));
        let honest = encoding(rec, Format::Mp3, 192, Some(17_000));
        assert_eq!(lying.verdict(), Verdict::Suspect);
        assert_eq!(honest.verdict(), Verdict::Good);
        assert!(
            lying.cutoff_hz.unwrap().value > honest.cutoff_hz.unwrap().value,
            "the liar wins on every term except the verdict"
        );
        e.encodings.insert(lying.id, lying);
        e.encodings.insert(honest.id, honest);

        assert_eq!(e.canonical(rec).unwrap().bitrate, 192);
    }

    /// **Total.** Two encodings alike in every measured way still have one
    /// winner, and it is the same winner on every machine.
    #[test]
    fn the_ordering_is_total_even_between_twins() {
        let rec = Id::<Recording>::derived(b"r");
        let mut a = encoding(rec, Format::Mp3, 320, Some(19_800));
        let mut b = a.clone();
        a.audio = audio(1);
        a.id = Id::of_audio(&a.audio);
        b.audio = audio(2);
        b.id = Id::of_audio(&b.audio);

        assert_ne!(
            canonical_key(&a),
            canonical_key(&b),
            "alike in every measured way, and still distinguishable"
        );

        // Considered in both orders, because "every replica agrees" must not
        // rest on how a collection happened to be iterated.
        let forwards = [&a, &b]
            .into_iter()
            .max_by_key(|e| canonical_key(e))
            .unwrap()
            .id;
        let backwards = [&b, &a]
            .into_iter()
            .max_by_key(|e| canonical_key(e))
            .unwrap()
            .id;
        assert_eq!(forwards, backwards, "order of consideration cannot matter");
        assert_eq!(forwards, a.id.max(b.id), "and it is the id that decides");
    }

    /// **The FLAC-and-MP3 case.** Canonical says the master represents the
    /// recording; the laptop still opens the transcode, because that is a
    /// preference about the machine.
    #[test]
    fn what_represents_and_what_plays_are_different_answers() {
        let rec = Id::<Recording>::derived(b"r");
        let laptop = Id::<Device>::derived(b"laptop");
        let mut e = Encodings::default();
        let flac = encoding(rec, Format::Flac, 900, Some(22_000));
        let mp3 = encoding(rec, Format::Mp3, 320, Some(19_800));
        e.holdings.push(holding(&flac, laptop, "/x.flac"));
        e.holdings.push(holding(&mp3, laptop, "/x.mp3"));
        let (flac_id, mp3_id) = (flac.id, mp3.id);
        e.encodings.insert(flac.id, flac);
        e.encodings.insert(mp3.id, mp3);

        assert_eq!(
            e.canonical(rec).unwrap().id,
            flac_id,
            "the master represents"
        );
        assert_eq!(
            e.for_playback(rec, None, &[laptop], &[Format::Mp3, Format::Flac])
                .unwrap()
                .id,
            mp3_id,
            "and the laptop opens the transcode, because it said it prefers to"
        );
    }

    /// You cannot play what is not here, whatever the policy says.
    #[test]
    fn playback_will_not_offer_a_copy_this_machine_does_not_have() {
        let rec = Id::<Recording>::derived(b"r");
        let (laptop, nas) = (Id::<Device>::derived(b"l"), Id::<Device>::derived(b"n"));
        let mut e = Encodings::default();
        let flac = encoding(rec, Format::Flac, 900, Some(22_000));
        let mp3 = encoding(rec, Format::Mp3, 320, Some(19_800));
        e.holdings.push(holding(&flac, nas, "/nas/x.flac"));
        e.holdings.push(holding(&mp3, laptop, "/x.mp3"));
        let (flac_id, mp3_id) = (flac.id, mp3.id);
        e.encodings.insert(flac.id, flac);
        e.encodings.insert(mp3.id, mp3);

        assert_eq!(e.canonical(rec).unwrap().id, flac_id, "still represents");
        assert_eq!(
            e.for_playback(rec, None, &[laptop], &[Format::Flac])
                .unwrap()
                .id,
            mp3_id,
            "even though this machine would rather have the flac"
        );
        assert!(
            e.for_playback(rec, None, &[], &[Format::Flac]).is_none(),
            "and nothing reachable is nothing to play"
        );
    }

    /// A pin overrules the policy — the vinyl rip that measures worse and
    /// sounds right.
    #[test]
    fn a_pin_beats_the_numbers() {
        let rec = Id::<Recording>::derived(b"r");
        let laptop = Id::<Device>::derived(b"l");
        let mut e = Encodings::default();
        let good = encoding(rec, Format::Mp3, 320, Some(19_800));
        let liked = encoding(rec, Format::Mp3, 192, Some(18_000));
        e.holdings.push(holding(&good, laptop, "/good.mp3"));
        e.holdings.push(holding(&liked, laptop, "/vinyl.mp3"));
        let liked_id = liked.id;
        e.encodings.insert(good.id, good);
        e.encodings.insert(liked.id, liked);

        assert_eq!(
            e.for_playback(rec, Some(liked_id), &[laptop], &[])
                .unwrap()
                .id,
            liked_id
        );
    }

    /// **A dangling pin is information, not an error.** If the pinned encoding
    /// is not here, the policy answers rather than nothing playing.
    #[test]
    fn a_pin_at_something_absent_falls_back() {
        let rec = Id::<Recording>::derived(b"r");
        let laptop = Id::<Device>::derived(b"l");
        let mut e = Encodings::default();
        let here = encoding(rec, Format::Mp3, 320, Some(19_800));
        e.holdings.push(holding(&here, laptop, "/here.mp3"));
        let here_id = here.id;
        e.encodings.insert(here.id, here);

        let gone = Id::<Encoding>::of_audio(&audio(0xdead));
        assert_eq!(
            e.for_playback(rec, Some(gone), &[laptop], &[]).unwrap().id,
            here_id,
            "the pin is a preference, not a constraint"
        );
    }

    /// A re-measure pass targets what the old analyser touched, not the whole
    /// library.
    #[test]
    fn stale_measurements_can_be_found_without_redoing_everything() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        let fresh = encoding(rec, Format::Mp3, 320, Some(19_800));
        let mut old = encoding(rec, Format::Mp3, 192, Some(19_100));
        old.cutoff_hz = Some(Measured {
            value: 19_100,
            analyser: ANALYSER - 1,
        });
        let old_id = old.id;
        e.encodings.insert(fresh.id, fresh);
        e.encodings.insert(old.id, old);

        assert_eq!(e.stale_measurements(), vec![old_id]);
    }

    #[test]
    fn encodings_round_trip_through_storage() {
        let rec = Id::<Recording>::derived(b"r");
        let mut e = Encodings::default();
        let enc = encoding(rec, Format::Flac, 900, Some(22_000));
        e.encodings.insert(enc.id, enc);
        let bytes = postcard::to_allocvec(&e).unwrap();
        let back: Encodings = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(
            back.canonical(rec).map(|x| x.id),
            e.canonical(rec).map(|x| x.id)
        );
    }
}
