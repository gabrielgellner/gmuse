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
        Command::Next => Ok(Instruction::Next),
        Command::Prev => Ok(Instruction::Prev),
        Command::Rescan => Ok(Instruction::Rescan),
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
        // A rating of zero and no rating are different acts, and the model already
        // distinguishes them.
        Command::Rate(spec) => Ok(Instruction::Rate(match spec {
            RateSpec::Set(n) if *n > 0 => Some(*n),
            _ => None,
        })),
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
                // A key `not_the_engines` admits whose value is neither: nothing to
                // do, and saying so beats guessing.
                _ => Err("that setting's value is not one the engine can act on"),
            }
        }
        Command::Toggle(key) => {
            if let Some(why) = key.not_the_engines() {
                return Err(why);
            }
            match mode_of(*key) {
                Some(which) => Ok(Instruction::Mode(which, Switch::Flip)),
                // The only other key it admits is `Scope`, which cycles rather than
                // flips — three values, not two.
                None => Ok(Instruction::Scope(ScopeTo::Cycle)),
            }
        }
        _ => Err("that command needs a view: it moves a cursor or a selection"),
    }
}
