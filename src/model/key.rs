//! The key a recording is in — issue #136.
//!
//! ## Why a position on the circle, and not a note name
//!
//! A key is stored as **where it sits on the circle of fifths**: a number 1–12
//! and a mode. That is the Camelot wheel's own coordinate, and it is chosen
//! because it makes the question this exists to answer into arithmetic.
//!
//! Harmonically compatible keys are the neighbours: the same number in the
//! other mode, and one step either way. As a note name that is a lookup table
//! nobody can check; as a position it is `n ± 1`. The circle of fifths is a
//! closed fact about twelve pitch classes, so it stays a **function** — #136
//! refused to write it into the vocabulary as hierarchy edges, where every row
//! would be a claim nobody could ever retract.
//!
//! ## Three notations, one value
//!
//! The tags in this library carry the same key three ways, sometimes on one
//! file: `TKEY=Cmin` musically, `TKY2=4m` in open-key notation, and `F#m - 6`
//! inside a comment. They are renderings, and the key is the thing — so every
//! spelling parses to one value at the boundary and nothing downstream sees a
//! notation it has to understand.

/// Major or minor. The wheel has two rings and this says which.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Mode {
    Major,
    Minor,
}

/// A musical key: a position on the circle of fifths, and a mode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Key {
    /// 1–12, the Camelot number. 8 is C major and A minor.
    position: u8,
    pub mode: Mode,
}

/// The conventional spelling of each major key, in Camelot order — 1B first.
///
/// **A table rather than a computation**, because the enharmonic choice is a
/// convention and not arithmetic: 3B is `Db` and not `C#`, and no rule derives
/// that from a pitch class. These are the spellings the wheel itself uses.
const MAJOR: [&str; 12] = [
    "B", "F#", "Db", "Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E",
];

/// The same for the minor ring — 1A first.
const MINOR: [&str; 12] = [
    "Abm", "Ebm", "Bbm", "Fm", "Cm", "Gm", "Dm", "Am", "Em", "Bm", "F#m", "C#m",
];

impl Key {
    /// From a Camelot position and mode. `None` outside 1–12.
    pub fn at(position: u8, mode: Mode) -> Option<Self> {
        (1..=12)
            .contains(&position)
            .then_some(Key { position, mode })
    }

    /// Where it sits on the wheel: 1–12.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the notations and the wheel are what the harmonic surface will ask for, and nothing asks yet (#136)"
    )]
    pub fn position(self) -> u8 {
        self.position
    }

    /// `Am`, `C#m`, `F`, `Bb` — the conventional spelling, and what is stored.
    pub fn written(self) -> &'static str {
        let at = usize::from(self.position) - 1;
        match self.mode {
            Mode::Major => MAJOR[at],
            Mode::Minor => MINOR[at],
        }
    }

    /// `8A`, `12B` — the Camelot wheel, as DJ software writes it.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the notations and the wheel are what the harmonic surface will ask for, and nothing asks yet (#136)"
    )]
    pub fn camelot(self) -> String {
        let ring = match self.mode {
            Mode::Major => 'B',
            Mode::Minor => 'A',
        };
        format!("{}{ring}", self.position)
    }

    /// `1m`, `4d` — open-key notation, which is the same wheel turned so that
    /// A minor is 1 rather than 8. Traktor writes this one.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the notations and the wheel are what the harmonic surface will ask for, and nothing asks yet (#136)"
    )]
    pub fn open_key(self) -> String {
        let ring = match self.mode {
            Mode::Major => 'd',
            Mode::Minor => 'm',
        };
        format!("{}{ring}", (self.position + 4) % 12 + 1)
    }

    /// **The keys that mix with this one**, which is the whole reason the
    /// position is what is stored.
    ///
    /// Its relative major or minor, and a step either way around the wheel.
    /// Four answers, always, because the circle closes.
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the notations and the wheel are what the harmonic surface will ask for, and nothing asks yet (#136)"
    )]
    pub fn compatible(self) -> [Key; 4] {
        let step = |by: i8| {
            let n = (i16::from(self.position) - 1 + i16::from(by)).rem_euclid(12) + 1;
            Key {
                position: n as u8,
                mode: self.mode,
            }
        };
        let other = Key {
            position: self.position,
            mode: match self.mode {
                Mode::Major => Mode::Minor,
                Mode::Minor => Mode::Major,
            },
        };
        [other, step(-1), step(1), step(0)]
    }

    /// Read a key out of whatever a tag wrote — #136.
    ///
    /// Musical (`Am`, `A minor`, `Cmin`, `C#m`, `Dbm`), Camelot (`8A`), or
    /// open key (`4m`). **A leading digit is what tells the wheels apart** from
    /// a note name, and the two wheels apart from each other by their ring
    /// letter: Camelot spells them `A`/`B` and open key `m`/`d`.
    ///
    /// Anything else is `None`. A key nobody can read is not a key, and
    /// guessing one would put a claim in the log that no tag ever made.
    pub fn parse(text: &str) -> Option<Self> {
        let s = text.trim();
        if s.is_empty() {
            return None;
        }
        if s.as_bytes()[0].is_ascii_digit() {
            return Self::wheel(s);
        }
        Self::musical(s)
    }

    /// `8A`, `12B`, `4m`, `1d`.
    fn wheel(s: &str) -> Option<Self> {
        let split = s.find(|c: char| !c.is_ascii_digit())?;
        let (number, ring) = s.split_at(split);
        let n: u8 = number.parse().ok()?;
        if !(1..=12).contains(&n) {
            return None;
        }
        match ring.trim().to_ascii_lowercase().as_str() {
            "a" => Key::at(n, Mode::Minor),
            "b" => Key::at(n, Mode::Major),
            // Open key is the same wheel with A minor at 1 rather than 8.
            "m" => Key::at((n + 6) % 12 + 1, Mode::Minor),
            "d" => Key::at((n + 6) % 12 + 1, Mode::Major),
            _ => None,
        }
    }

    /// `Am`, `A minor`, `Cmin`, `F`, `Bb major`.
    fn musical(s: &str) -> Option<Self> {
        let mut chars = s.chars();
        let letter = chars.next()?.to_ascii_uppercase();
        let mut pitch = match letter {
            'C' => 0i16,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            'B' => 11,
            _ => return None,
        };
        let mut rest = chars.as_str();
        // **Several accidentals are refused, not summed.** `A##` is not a
        // spelling any tag writes, and reading it as B would invent a key.
        match rest.chars().next() {
            Some('#' | '♯') => {
                pitch += 1;
                rest = &rest[rest.char_indices().nth(1).map_or(rest.len(), |(i, _)| i)..];
            }
            Some('b' | '♭')
                if !rest.trim_start_matches(['b', '♭']).is_empty() || rest.len() == 1 =>
            {
                pitch -= 1;
                rest = &rest[rest.char_indices().nth(1).map_or(rest.len(), |(i, _)| i)..];
            }
            _ => {}
        }

        let word = rest
            .trim()
            .trim_start_matches([' ', '-', '_'])
            .to_ascii_lowercase();
        let mode = match word.as_str() {
            "" | "maj" | "major" | "d" | "dur" => Mode::Major,
            "m" | "min" | "minor" | "moll" => Mode::Minor,
            _ => return None,
        };
        Key::at(Self::position_of(pitch.rem_euclid(12) as u8, mode), mode)
    }

    /// Where a pitch class sits on the wheel.
    ///
    /// Each step around the circle is a fifth — seven semitones — and seven is
    /// its own inverse modulo twelve, so the way back is another seven. C major
    /// is 8 and A minor is 8, which is what anchors both rings.
    fn position_of(pitch: u8, mode: Mode) -> u8 {
        let anchor = match mode {
            Mode::Major => 0i16, // C
            Mode::Minor => 9,    // A
        };
        ((8 - 1 + 7 * (i16::from(pitch) - anchor)).rem_euclid(12) + 1) as u8
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.written())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key, so a table typed out by hand is checked against the
    /// arithmetic that is supposed to agree with it.
    fn all() -> Vec<Key> {
        (1..=12)
            .flat_map(|n| [Mode::Major, Mode::Minor].map(|m| Key::at(n, m).expect("1..=12")))
            .collect()
    }

    /// **Every spelling of a key reads back as that key** — #136, and the
    /// reason a key is a value rather than whatever string a tagger wrote.
    #[test]
    fn every_notation_parses_to_one_value() {
        for key in all() {
            for written in [key.written().to_string(), key.camelot(), key.open_key()] {
                assert_eq!(
                    Key::parse(&written),
                    Some(key),
                    "{written} did not read back as {key}"
                );
            }
        }
    }

    /// The spellings the tags in this library actually carry, checked against
    /// what the same files say in another notation — `TKEY=4m` on a file whose
    /// own comment reads `F#m`.
    #[test]
    fn the_tags_this_library_carries_read_correctly() {
        let same = |a: &str, b: &str| {
            assert_eq!(
                Key::parse(a),
                Key::parse(b),
                "{a} and {b} are the same key written two ways"
            );
            assert!(Key::parse(a).is_some(), "{a} did not parse at all");
        };
        same("4m", "F#m");
        same("Cmin", "Cm");
        same("8A", "Am");
        same("8B", "C");
        same("C#m", "Dbm");
        same("Ab", "G#");
        same("A minor", "Am");
        same("Bb major", "Bb");

        assert_eq!(Key::parse("Am").map(|k| k.camelot()), Some("8A".into()));
        assert_eq!(Key::parse("C").map(|k| k.camelot()), Some("8B".into()));
        assert_eq!(Key::parse("F#m").map(|k| k.open_key()), Some("4m".into()));
    }

    /// **Every spelling the dance library actually carries.**
    ///
    /// The 39 distinct `TKEY` values across its 592 files — musical with
    /// sharps and flats, `min`/`maj` words, and open key. Not a sample: the
    /// whole set, so a notation this library holds cannot be one the parser
    /// silently drops.
    #[test]
    fn every_spelling_on_disk_reads() {
        const ON_DISK: [&str; 39] = [
            "10m", "11m", "12m", "2m", "3m", "4m", "5m", "6d", "9m", "A", "A#", "A#m", "Ab", "Am",
            "Amaj", "Amin", "B", "Bbm", "Bm", "C", "C#", "C#m", "Cm", "Cmin", "D", "D#m", "Dm",
            "E", "Eb", "Ebm", "Em", "F", "F#m", "Fm", "G", "G#", "G#m", "Gb", "Gm",
        ];
        for spelling in ON_DISK {
            assert!(
                Key::parse(spelling).is_some(),
                "{spelling} is on disk here and does not parse"
            );
        }

        // **The enharmonics agree**, which is what stops one key becoming two
        // concepts because two taggers spelled it differently.
        assert_eq!(Key::parse("A#m"), Key::parse("Bbm"));
        assert_eq!(Key::parse("D#m"), Key::parse("Ebm"));
        assert_eq!(Key::parse("G#"), Key::parse("Ab"));
        assert_eq!(Key::parse("Gb"), Key::parse("F#"));
        assert_eq!(Key::parse("Amaj"), Key::parse("A"));
        assert_eq!(Key::parse("Amin"), Key::parse("Am"));
    }

    /// **Nonsense is refused rather than guessed at.** A key invented from an
    /// unreadable tag is a claim in the log that no tag ever made.
    #[test]
    fn what_is_not_a_key_is_not_read_as_one() {
        for bad in [
            "", " ", "H", "Hm", "13A", "0A", "8C", "Am7", "A#m#", "sharp", "12", "-", "Cmaybe",
        ] {
            assert_eq!(Key::parse(bad), None, "{bad:?} parsed as a key");
        }
    }

    /// **Compatibility is arithmetic, which is why the position is stored.**
    ///
    /// The relative major or minor, and one step either way. Written out for A
    /// minor, which every DJ knows the answer for: C major, D minor, E minor.
    #[test]
    fn the_keys_that_mix_are_the_neighbours_on_the_wheel() {
        let am = Key::parse("Am").expect("a key");
        let mut mixes: Vec<&str> = am.compatible().iter().map(|k| k.written()).collect();
        mixes.sort_unstable();
        assert_eq!(mixes, ["Am", "C", "Dm", "Em"]);

        // **The circle closes**, so the ends wrap rather than running out.
        let one = Key::at(1, Mode::Minor).expect("1A");
        assert!(
            one.compatible().iter().any(|k| k.position() == 12),
            "1A has no neighbour at 12A, so the wheel is not a wheel"
        );
    }

    /// Two rings, twelve positions, and no two keys sharing a name — the
    /// property every table here depends on.
    #[test]
    fn the_wheel_has_twenty_four_distinct_keys() {
        let names: std::collections::BTreeSet<_> = all().iter().map(|k| k.written()).collect();
        assert_eq!(names.len(), 24);
        let wheels: std::collections::BTreeSet<_> = all().iter().map(|k| k.camelot()).collect();
        assert_eq!(wheels.len(), 24);
        assert_eq!(Key::at(0, Mode::Major), None);
        assert_eq!(Key::at(13, Mode::Major), None);
    }
}
