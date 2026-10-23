//! **From the command language to what the engine can do** — #213, #243, I9, §12.
//!
//! Rehomed out of `remote.rs` when `--remote` was retired (#243). It was never part
//! of that door: the old socket happened to be the first caller, and the engine's
//! `cmd.exec_line` became the only one — so when the module went, this had to not go
//! with it.
//!
//! **And it cannot live in `gmuse-engine`**, which is where it looked like it
//! belonged. §4.3 is explicit that the engine crate is *"**Not** the command
//! language"*: it owns `Instruction`, the vocabulary a control thread acts on, and
//! knows nothing of `Command`'s sixty-three variants. A translation between two
//! vocabularies belongs with the one that has the wider surface, which is this one.
//!
//! **`is_remotable` did not come with it.** It was `instruction_of(cmd).is_ok()`, and
//! its only caller was the old socket's `answer`, which asked before acting. The
//! engine's `cmd.exec_line` does not ask: it translates, and carries out or reports
//! the reason. Several comments still name `is_remotable` because it is the thing they
//! are explaining the absence of — #213 is the story of it having drifted out of step
//! with `apply`, and that story is why this function is total.

use gmuse_engine::instruction::Instruction;

use crate::cmd::Command;

/// **The one translation from the command language to what the engine can do** —
/// #213, I9, §12.
///
/// `Ok` means the control thread can carry it out with no view and no cursor; `Err`
/// is the reason it cannot, for a client to be told rather than left guessing.
///
/// **This replaces two lists that had drifted.** `is_remotable` said what the socket
/// would accept and `control::apply` said what the engine would do, and they
/// disagreed in both directions: `vol`, `rate`, `prev`, `set` and `toggle` were
/// admitted and silently dropped — each answering `{"accepted": true}` and doing
/// nothing — while `play` was handled and refused at the gate, so unreachable.
///
/// #233 caught that with a pair of tests that scraped both functions' source and
/// compared the names, which is a poor guard and knew it: the first version read
/// `Command::` out of comments. One total function is a shape in which the drift
/// cannot be written down, and `Instruction`'s own match is exhaustive with no
/// catch-all — so the two halves cannot disagree about anything.
///
/// **Not `Command::Play`**, and the test below says why: it starts the *selected*
/// track, so down a socket it would start whatever row a cursor sits on. Resuming is
/// `pause`, which toggles; #225 is where the protocol gets three distinct verbs.
pub fn instruction_of(cmd: &Command) -> Result<Instruction, &'static str> {
    use crate::cmd::{RateSpec, SeekSpec, SettingValue, VolSpec};
    use gmuse_engine::instruction::{Fader, Mode, ScopeTo, Switch};

    /// Which engine mode a setting key names, if it names one.
    fn mode_of(key: crate::cmd::SettingKey) -> Option<Mode> {
        match key {
            crate::cmd::SettingKey::Shuffle => Some(Mode::Shuffle),
            crate::cmd::SettingKey::Repeat => Some(Mode::Repeat),
            crate::cmd::SettingKey::RepeatCurrent => Some(Mode::RepeatCurrent),
            _ => None,
        }
    }

    match cmd {
        Command::Pause => Ok(Instruction::Toggle),
        Command::Cue => Ok(Instruction::Cue),
        Command::CuePreview => Ok(Instruction::CuePreview),
        Command::Next => Ok(Instruction::Next),
        Command::Prev => Ok(Instruction::Prev),
        Command::Rescan => Ok(Instruction::Rescan),
        // **Shuffle on is already a reshuffle**: the engine's `set_mode` draws the next
        // seed every time, on or not. What #320 changed is that the seed is not a constant.
        Command::Reshuffle => Ok(Instruction::Mode(Mode::Shuffle, Switch::On)),
        // **Resolved here, because the text is this side's** — §12. `"+10"` and
        // `"50%"` are spellings; what crosses is frames.
        Command::Seek(spec) => Ok(Instruction::Seek(match spec {
            SeekSpec::Absolute(secs) => {
                crate::audio::SeekTo::Absolute(std::time::Duration::from_secs_f64(secs.max(0.0)))
            }
            SeekSpec::Relative(delta) => crate::audio::SeekTo::Relative(*delta),
        })),
        Command::Vol(spec) => Ok(Instruction::Volume(match spec {
            VolSpec::Absolute(v) => Fader::To(*v),
            VolSpec::Relative(d) => Fader::By(*d),
        })),
        // **`here` is a cursor's word**, and the engine has none to rate under (#324).
        Command::Rate(_, crate::cmd::RateWhat::Here) => Err(
            "`rate … here` rates the row under a cursor, and the engine has none — drop `here` to rate what is playing",
        ),
        // **A bare `rate` asks, and the engine has nowhere to answer.** It used to share
        // one catch-all arm with `none`, so `gmuse ctl rate` *cleared* the playing
        // track's rating — a question carried out as an erasure.
        Command::Rate(RateSpec::Report, _) => Err(
            "`rate` with no number reports the rating, and the engine has nowhere to say it — `rate 4` or `rate none`",
        ),
        // A rating and no rating are different acts, and the model already
        // distinguishes them. Each is named, so nothing else can fall into one.
        Command::Rate(RateSpec::Set(n), _) => Ok(Instruction::Rate(Some(*n))),
        Command::Rate(RateSpec::Clear, _) => Ok(Instruction::Rate(None)),
        // **`SettingKey::not_the_engines` is the gate for a setting**, so the
        // refusal reads as a sentence about that setting rather than about `set`.
        Command::Set { key, value } => {
            if let Some(why) = key.not_the_engines() {
                return Err(why);
            }
            match (mode_of(*key), value) {
                (Some(which), SettingValue::Bool(on)) => Ok(Instruction::Mode(
                    which,
                    if *on { Switch::On } else { Switch::Off },
                )),
                (None, SettingValue::Scope(mode)) => Ok(Instruction::Scope(ScopeTo::Set(*mode))),
                (None, SettingValue::OnDeviceGone(to)) => Ok(Instruction::OnDeviceGone(*to)),
                // A key `not_the_engines` admits whose value is neither: nothing to
                // do, and saying so beats guessing.
                _ => Err("that setting's value is not one the engine can act on"),
            }
        }
        Command::Toggle(key) => {
            if let Some(why) = key.not_the_engines() {
                return Err(why);
            }
            match (mode_of(*key), key) {
                (Some(which), _) => Ok(Instruction::Mode(which, Switch::Flip)),
                // **Named, not fallen through to** — #318. This arm was the
                // catch-all for "the one other key it admits", and the first new
                // engine setting would have toggled the scope.
                (None, crate::cmd::SettingKey::Scope) => Ok(Instruction::Scope(ScopeTo::Cycle)),
                (None, _) => Err("that setting is set, not toggled"),
            }
        }
        _ => Err("that command needs a view: it moves a cursor or a selection"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::OnDeviceGone;
    use gmuse_engine::instruction::ScopeTo;

    fn of(line: &str) -> Result<Instruction, &'static str> {
        instruction_of(&crate::cmd::parse::command(line).expect("the line parses"))
    }

    /// **`on_device_gone` reaches the engine as itself** — #318. Both values, so a
    /// translation that dropped the value would be caught on one of them.
    #[test]
    fn on_device_gone_is_carried_to_the_engine() {
        for to in OnDeviceGone::ALL {
            let got = of(&format!("set on_device_gone={}", to.label()));
            assert!(
                matches!(got, Ok(Instruction::OnDeviceGone(v)) if v == to),
                "{} became {got:?}",
                to.label()
            );
        }
    }

    /// **`reshuffle` is shuffle on**, which the engine answers with a seed it has not used
    /// — #320. Not a flip: reshuffling with shuffle on must not turn it off.
    #[test]
    fn reshuffle_turns_shuffle_on_rather_than_flipping_it() {
        use gmuse_engine::instruction::{Mode, Switch};
        assert!(matches!(
            of("reshuffle"),
            Ok(Instruction::Mode(Mode::Shuffle, Switch::On))
        ));
    }

    /// **Toggling a setting that is set is refused, not read as the scope** — #318.
    /// The arm after `mode_of` was a catch-all meaning "the other key it admits",
    /// which was `Scope` until there were two.
    #[test]
    fn toggling_on_device_gone_does_not_cycle_the_scope() {
        assert!(
            matches!(of("toggle scope"), Ok(Instruction::Scope(ScopeTo::Cycle))),
            "the instrument: toggling the scope still cycles it"
        );
        assert!(of("toggle on_device_gone").is_err());
    }

    /// **A bare `rate` asks, and must not erase** — found with #324. It shared the
    /// `none` arm, so `gmuse ctl rate` cleared the playing track's rating.
    #[test]
    fn asking_for_a_rating_is_refused_rather_than_clearing_it() {
        // The instrument: the two that do write still reach the engine as themselves.
        assert!(matches!(of("rate 4"), Ok(Instruction::Rate(Some(4)))));
        assert!(matches!(of("rate none"), Ok(Instruction::Rate(None))));
        assert!(of("rate").is_err(), "a question became {:?}", of("rate"));
    }

    /// **`here` is a cursor's word**, and the engine has no cursor (#324). Accepting
    /// it would rate the playing track while the line said otherwise.
    #[test]
    fn rating_here_is_refused_by_the_engine() {
        assert!(of("rate 4 here").is_err());
        assert!(of("rate none here").is_err());
    }
}
