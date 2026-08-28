//! Key names, and the round trip between them and terminal key events.
//!
//! The rc file stores bindings as text, so `parse(format(k)) == k` has to hold
//! for every key we can bind — otherwise saving a config quietly changes it.
//! That property is what the tests here assert.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A bindable key: a code plus the modifiers that matter.
///
/// Deliberately *not* `crossterm::KeyEvent`: that also carries a press/release
/// kind and a state bitfield, and two events that a user would call "the same
/// key" can differ in those. Normalising into this type is what makes the
/// binding map lookups reliable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

impl Key {
    /// Used by tests and by callers building a binding directly; the default
    /// table goes through the parser instead.
    #[allow(dead_code)]
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
        }
    }

    #[allow(dead_code)]
    pub fn ctrl(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: true,
            alt: false,
        }
    }

    /// Normalise a terminal event into a bindable key.
    ///
    /// Shift is folded away for characters: the terminal reports `J` with
    /// SHIFT set, and treating that as different from a bound `J` would mean
    /// no capital binding ever fired.
    pub fn from_event(event: KeyEvent) -> Self {
        Self {
            code: event.code,
            ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
            alt: event.modifiers.contains(KeyModifiers::ALT),
        }
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.alt {
            write!(f, "M-")?;
        }
        if self.ctrl {
            write!(f, "^")?;
        }
        f.write_str(&code_name(self.code))
    }
}

fn code_name(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Esc => "escape".into(),
        KeyCode::F(n) => format!("f{n}"),
        other => format!("{other:?}").to_lowercase(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a key name: {0:?}")]
pub struct KeyParseError(pub String);

/// A bound key sequence: one key, or a chord like `g g`.
///
/// Written space-separated, as in eilmeldung's `"g g"`. A separator character
/// was not an option — `-` and `+` are themselves bindable keys.
pub type Seq = Vec<Key>;

/// Parse a whitespace-separated key sequence.
pub fn parse_seq(s: &str) -> Result<Seq, KeyParseError> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.is_empty() {
        return Err(KeyParseError(s.to_string()));
    }
    parts.iter().map(|p| parse(p)).collect()
}

/// Render a sequence back to text.
pub fn format_seq(seq: &[Key]) -> String {
    seq.iter()
        .map(|k| k.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a key name. Accepts `j`, `space`, `f5`, `^f`, `M-x`, `M-^f`.
pub fn parse(s: &str) -> Result<Key, KeyParseError> {
    let original = s;
    let mut s = s.trim();
    if s.is_empty() {
        return Err(KeyParseError(original.to_string()));
    }

    let mut alt = false;
    let mut ctrl = false;

    // Modifiers are prefixes and may be combined, alt outermost.
    loop {
        if let Some(rest) = s.strip_prefix("M-") {
            alt = true;
            s = rest;
        } else if let Some(rest) = s.strip_prefix('^') {
            ctrl = true;
            s = rest;
        } else if let Some(rest) = s.strip_prefix("C-") {
            ctrl = true;
            s = rest;
        } else {
            break;
        }
    }

    let code = parse_code(s).ok_or_else(|| KeyParseError(original.to_string()))?;
    Ok(Key { code, ctrl, alt })
}

fn parse_code(s: &str) -> Option<KeyCode> {
    // A single character is itself — including an uppercase one, which is how
    // a shifted binding is written.
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && !c.is_whitespace()
    {
        return Some(KeyCode::Char(c));
    }

    let lower = s.to_ascii_lowercase();
    Some(match lower.as_str() {
        "space" => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "escape" | "esc" => KeyCode::Esc,
        other => {
            let n: u8 = other.strip_prefix('f')?.parse().ok()?;
            if (1..=12).contains(&n) {
                KeyCode::F(n)
            } else {
                return None;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_characters() {
        assert_eq!(parse("j").unwrap(), Key::new(KeyCode::Char('j')));
        // Uppercase is its own key, not j-plus-shift.
        assert_eq!(parse("J").unwrap(), Key::new(KeyCode::Char('J')));
        assert_eq!(parse("/").unwrap(), Key::new(KeyCode::Char('/')));
        assert_eq!(parse("1").unwrap(), Key::new(KeyCode::Char('1')));
    }

    #[test]
    fn named_keys_and_aliases() {
        assert_eq!(parse("space").unwrap(), Key::new(KeyCode::Char(' ')));
        assert_eq!(parse("enter").unwrap(), Key::new(KeyCode::Enter));
        assert_eq!(parse("return").unwrap(), Key::new(KeyCode::Enter));
        assert_eq!(parse("pgup").unwrap(), Key::new(KeyCode::PageUp));
        assert_eq!(parse("PageUp").unwrap(), Key::new(KeyCode::PageUp));
        assert_eq!(parse("f5").unwrap(), Key::new(KeyCode::F(5)));
    }

    #[test]
    fn modifiers() {
        assert_eq!(parse("^f").unwrap(), Key::ctrl(KeyCode::Char('f')));
        assert_eq!(parse("C-f").unwrap(), Key::ctrl(KeyCode::Char('f')));
        let alt_x = parse("M-x").unwrap();
        assert!(alt_x.alt && !alt_x.ctrl);
        let both = parse("M-^f").unwrap();
        assert!(both.alt && both.ctrl);
    }

    #[test]
    fn sequences_round_trip() {
        for text in ["j", "g g", "g t", "^f", "M-x", "g space", "g f5"] {
            let seq = parse_seq(text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(format_seq(&seq), text, "{text:?} did not round trip");
        }
    }

    #[test]
    fn a_sequence_of_one_is_just_a_key() {
        assert_eq!(parse_seq("j").unwrap(), vec![Key::new(KeyCode::Char('j'))]);
    }

    #[test]
    fn extra_whitespace_is_forgiven() {
        assert_eq!(parse_seq("  g   g  ").unwrap().len(), 2);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
        assert!(parse("notakey").is_err());
        assert!(parse("f13").is_err());
        assert!(parse_seq("").is_err());
        assert!(parse_seq("g notakey").is_err());
    }

    /// The property the rc file depends on: a saved binding must parse back to
    /// the key it came from, or writing a config silently rewrites it.
    #[test]
    fn every_bindable_key_round_trips() {
        let mut keys: Vec<Key> = Vec::new();
        for c in "abzAZ019/?:<>-+=[]".chars() {
            keys.push(Key::new(KeyCode::Char(c)));
            keys.push(Key::ctrl(KeyCode::Char(c)));
        }
        keys.push(Key::new(KeyCode::Char(' ')));
        for code in [
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Esc,
        ] {
            keys.push(Key::new(code));
            keys.push(Key::ctrl(code));
        }
        for n in 1..=12 {
            keys.push(Key::new(KeyCode::F(n)));
        }
        keys.push(Key {
            code: KeyCode::Char('x'),
            ctrl: true,
            alt: true,
        });

        for key in keys {
            let text = key.to_string();
            let back = parse(&text)
                .unwrap_or_else(|e| panic!("{key:?} formatted as {text:?} and failed: {e}"));
            assert_eq!(back, key, "{text:?} did not round trip");
        }
    }

    #[test]
    fn shift_is_folded_into_the_character() {
        let event = KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT);
        // Otherwise a bound `J` would never fire, because the terminal always
        // sets SHIFT alongside the capital.
        assert_eq!(Key::from_event(event), Key::new(KeyCode::Char('J')));
    }
}
