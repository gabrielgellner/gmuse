//! What a file's audio is called, independent of anything a tagger does.
//!
//! **The type, not its derivation.** Computing an `AudioId` means probing a
//! file and hashing its packets, which needs a decoder; and this crate is the
//! one `gmuse-proto` depends on, so a decoder here would put symphonia on the
//! wire crate's dependency graph. `gmuse::audio_id` keeps `measure` and `of`
//! and the reasoning about what is hashed; what crosses a wire or names a row
//! is this.

use std::fmt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AudioId {
    pub scheme: u8,
    pub hash: u128,
}

/// **It crosses text formats as its `Display` form, and binary ones unchanged** — #240.
///
/// The derive wrote `{"scheme":1,"hash":187646888898413691098496886067505897950}`,
/// which is valid JSON and unusable as *data*: `serde_json::Number` holds an `i64`, a
/// `u64` or an `f64`, and a 128-bit hash is none of them. `to_string` wrote it and
/// `from_str` read it back — so the log was always fine — but `to_value` answered
/// "number out of range", and the protocol's ingress parses a line into a `Value`
/// before typing it (I4). So no fact carrying one could cross the wire, and the first
/// attempt to send one *panicked* inside a library operation, because `json!` unwraps.
///
/// **`is_human_readable` is what makes one impl serve both formats**, and finding that
/// out cost two failures worth recording. A plain string form broke postcard twice
/// over: `collect_str` answers `WontImplement`, because a length-prefixed format needs
/// the length first; and `#[serde(untagged)]` cannot read the old shape at all, because
/// it buffers into serde's `Content` and that has no `u128`.
///
/// So JSON gets `v1:8d2b…` — the spelling `Display` has always used, greppable, and it
/// says what made it — and postcard keeps the struct, which costs no cache version and
/// is nobody's to read by eye anyway.
impl serde::Serialize for AudioId {
    fn serialize<S: serde::Serializer>(&self, out: S) -> Result<S::Ok, S::Error> {
        if out.is_human_readable() {
            // `serialize_str`, not `collect_str`: postcard refuses the latter and a
            // serializer that says it is human-readable today may not tomorrow.
            out.serialize_str(&self.to_string())
        } else {
            use serde::ser::SerializeStruct;
            let mut it = out.serialize_struct("AudioId", 2)?;
            it.serialize_field("scheme", &self.scheme)?;
            it.serialize_field("hash", &self.hash)?;
            it.end()
        }
    }
}

/// **Reads the string, the integers 280,000 records already hold, and postcard.**
///
/// An append-only log is not rewritten for a spelling change, so the old JSON shape has
/// to keep working for ever rather than for a migration window. A hand-written visitor
/// rather than `untagged` for the reason above: `Content` cannot hold a `u128`, so the
/// derived either-or silently fails on exactly the records this has to read.
impl<'de> serde::Deserialize<'de> for AudioId {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        struct Either;

        impl<'de> serde::de::Visitor<'de> for Either {
            type Value = AudioId;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an audio id, as `v1:…` or as {scheme, hash}")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<AudioId, E> {
                text.parse()
                    .map_err(|()| E::custom(format!("{text:?} is not an audio id")))
            }

            /// **Postcard's shape, and the third thing this cost to learn.**
            ///
            /// A compact format writes a struct as its fields in order and hands the
            /// visitor a `SeqAccess`, not a `MapAccess` — so a visitor with only
            /// `visit_map` answers `SerdeDeCustom` on a round trip that used to work.
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<AudioId, A::Error> {
                let scheme = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
                let hash = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
                Ok(AudioId { scheme, hash })
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<AudioId, M::Error> {
                let mut scheme = None;
                let mut hash = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "scheme" => scheme = Some(map.next_value()?),
                        "hash" => hash = Some(map.next_value()?),
                        // **Ignored rather than refused.** A field this build does not
                        // know is a later one's, and `SchemaFence` (#107) is about a
                        // stale reader losing nothing.
                        _ => {
                            let _ = map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(AudioId {
                    scheme: scheme.ok_or_else(|| serde::de::Error::missing_field("scheme"))?,
                    hash: hash.ok_or_else(|| serde::de::Error::missing_field("hash"))?,
                })
            }
        }

        if input.is_human_readable() {
            // Self-describing, so the shape decides which arm runs and both are live.
            input.deserialize_any(Either)
        } else {
            // Postcard is not self-describing and never holds the old shape: the
            // caches that carry one are versioned and discarded on a mismatch.
            input.deserialize_struct("AudioId", &["scheme", "hash"], Either)
        }
    }
}

impl fmt::Display for AudioId {
    /// `v1:` then 32 hex digits — greppable, and it says what made it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}:{:032x}", self.scheme, self.hash)
    }
}

impl std::str::FromStr for AudioId {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s.strip_prefix('v').ok_or(())?;
        let (scheme, hash) = rest.split_once(':').ok_or(())?;
        Ok(AudioId {
            scheme: scheme.parse().map_err(|_| ())?,
            hash: u128::from_str_radix(hash, 16).map_err(|_| ())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An audio id crosses as data, which a `u128` cannot** — #240.
    ///
    /// The one assertion that makes every fact carrying one safe. `serde_json::Number`
    /// holds an `i64`, a `u64` or an `f64`, so `to_value` on the old derived form
    /// answered "number out of range" — and since the protocol's ingress parses a line
    /// into a `Value` before typing it (I4), no fact carrying an audio id could cross
    /// at all. It took a *panic* inside a library operation to notice, because
    /// `serde_json::json!` unwraps.
    ///
    /// `to_string` always worked, which is exactly why this hid: the log was fine.
    #[test]
    fn an_audio_id_survives_the_value_the_protocol_parses_into() {
        // The hash from the file that found this, which is above `u64::MAX` and so is
        // the case that matters.
        let id = AudioId {
            scheme: 1,
            hash: 187_646_888_898_413_691_098_496_886_067_505_897_950,
        };
        assert!(
            id.hash > u128::from(u64::MAX),
            "a smaller one proves nothing"
        );

        let value = serde_json::to_value(id).expect("an audio id is representable");
        assert_eq!(
            value,
            serde_json::Value::String("v1:8d2b79ea3683c3f7920fea9976afa1de".into()),
            "the wire form is what `Display` says"
        );
        let back: AudioId = serde_json::from_value(value).expect("and reads back");
        assert_eq!(back, id, "the round trip is not lossy");
    }

    /// **The integer form 280,000 records already hold still reads** — #240.
    ///
    /// An append-only log is not rewritten for a spelling change, so this is for ever
    /// rather than for a migration window.
    #[test]
    fn the_form_the_derive_wrote_reads_back_unchanged() {
        let old = r#"{"scheme":1,"hash":187646888898413691098496886067505897950}"#;
        let id: AudioId = serde_json::from_str(old).expect("a record from before #240");
        assert_eq!(id.scheme, 1);
        assert_eq!(id.hash, 187_646_888_898_413_691_098_496_886_067_505_897_950);
        // And writing it again says the new spelling, which is what makes the log
        // converge rather than keeping two forms alive.
        assert_eq!(
            serde_json::to_string(&id).expect("encodes"),
            "\"v1:8d2b79ea3683c3f7920fea9976afa1de\""
        );
    }
}
