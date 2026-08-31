//! Completion for the `:` line.
//!
//! Verbs come from [`parse::VERBS`], which a test keeps in step with the
//! parser. Arguments are completed per verb, and for `set` the valid values
//! depend on the key already typed — the same key-directed typing the parser
//! does in `setting_value`.
//!
//! Deliberately *not* fuzzy: `:` completion is over a small closed vocabulary
//! the user is trying to type exactly, where a prefix match is predictable and
//! a fuzzy one is a guess. Fuzzy matching belongs on `/` search, over titles.

use super::keys;
use super::{Context, SettingKey, parse};

/// What is available to complete against, from the running application.
#[derive(Debug, Default, Clone)]
pub struct World {
    pub catalogs: Vec<String>,
    /// Every name `:playlist open` accepts — rules and saved files alike,
    /// because they are one vocabulary (#15, #22).
    pub playlists: Vec<String>,
    /// `(context, key text)` for every binding, so `unbind` offers only keys
    /// that are actually bound.
    pub bound: Vec<(Context, String)>,
}

/// The result of a completion request.
#[derive(Debug, PartialEq)]
pub struct Completion {
    /// The line with the longest unambiguous extension applied.
    pub line: String,
    /// Every candidate, for display when more than one remains.
    pub options: Vec<String>,
}

/// Complete `line`, which is the prompt buffer without its leading `:`.
///
/// Returns `None` when nothing matches, so the caller can leave the line alone
/// rather than deleting what was typed.
pub fn complete(line: &str, world: &World) -> Option<Completion> {
    let (head, word) = split_last_word(line);
    let candidates = candidates_for(head, word, world);

    let matches: Vec<String> = candidates
        .into_iter()
        .filter(|c| c.starts_with(word))
        .collect();
    if matches.is_empty() {
        return None;
    }

    let extended = common_prefix(&matches);
    Some(Completion {
        // A single match gets a trailing space: the next argument is what you
        // want to type next, and having to add the space yourself is friction.
        line: if matches.len() == 1 {
            format!("{head}{extended} ")
        } else {
            format!("{head}{extended}")
        },
        options: matches,
    })
}

/// Every candidate matching the final word, **without touching the line**.
///
/// [`complete`] extends the line to the longest common prefix, which is right
/// for `Tab` and wrong for filtering as you type: silently rewriting what
/// someone is in the middle of typing is the one thing completion must never
/// do. This is the same matching with the rewriting removed.
///
/// An empty result is meaningful — "nothing matches what you have typed" — and
/// is distinct from a closed menu, which is why the caller keeps the menu in an
/// `Option` rather than reading emptiness as absence.
pub fn matches(line: &str, world: &World) -> Vec<String> {
    let (head, word) = split_last_word(line);

    // `set key=value` is one word whose halves complete against different
    // things, so the split is inside the word rather than at a space.
    if let Some((key_text, typed)) = word.rsplit_once('=') {
        return match SettingKey::parse(key_text) {
            Some(key) => setting_values(key)
                .into_iter()
                .filter(|v| v.starts_with(typed))
                .collect(),
            None => Vec::new(),
        };
    }

    candidates_for(head, word, world)
        .into_iter()
        .filter(|c| c.starts_with(word))
        .collect()
}

/// Everything valid in the position `word` occupies.
fn candidates_for(head: &str, _word: &str, world: &World) -> Vec<String> {
    let words: Vec<&str> = head.split_whitespace().collect();

    // The first word is a verb.
    let Some(verb) = words.first() else {
        return parse::VERBS.iter().map(|s| s.to_string()).collect();
    };

    match *verb {
        "view" if words.len() == 1 => (1..=8).map(|n| n.to_string()).collect(),
        "rate" if words.len() == 1 => std::iter::once("none".to_string())
            .chain((1..=5).map(|n| n.to_string()))
            .collect(),

        // Subcommands first, then names — so `:playlist <Tab>` shows what can
        // be done and `:playlist open <Tab>` what there is to do it to.
        "playlist" if words.len() == 1 => ["list", "new", "open", "save"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        // `save` completes existing names too: overwriting one is a normal
        // thing to want, and typing it out again is not.
        "playlist" if words.len() == 2 && matches!(words[1], "open" | "save") => {
            world.playlists.clone()
        }

        // Catalogs accept a list, so they keep completing.
        "catalog" | "cat" => {
            let mut out = vec!["all".to_string()];
            for name in &world.catalogs {
                out.push(name.clone());
                out.push(format!("+{name}"));
                out.push(format!("-{name}"));
            }
            out
        }

        "toggle" if words.len() == 1 => SettingKey::ALL
            .iter()
            // resample_quality is not a boolean, so it is not a toggle.
            .filter(|k| **k != SettingKey::ResampleQuality)
            .map(|k| k.name().to_string())
            .collect(),

        // `set key=value`: before the `=` complete keys, after it complete the
        // values that key accepts.
        "set" if words.len() == 1 => SettingKey::ALL
            .iter()
            .map(|k| format!("{}=", k.name()))
            .collect(),

        "in" if words.len() == 1 => vec!["tree".into(), "tracks".into()],
        // Anything after the pane is a whole command again.
        "in" if words.len() >= 2 => parse::VERBS.iter().map(|s| s.to_string()).collect(),

        "bind" | "unbind" if words.len() == 1 => [
            "common", "library", "sorted", "playlist", "queue", "browser", "filters", "settings",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        // `unbind <ctx> <key>`: only keys actually bound there, plus the
        // common ones that would apply.
        "unbind" if words.len() == 2 => {
            let ctx = Context::parse(words[1]);
            world
                .bound
                .iter()
                .filter(|(c, _)| Some(*c) == ctx || *c == Context::Common)
                .map(|(_, k)| k.clone())
                .collect()
        }
        "bind" if words.len() == 2 => keys::NAMES.iter().map(|s| s.to_string()).collect(),
        // `bind <ctx> <key> <command...>` — a whole command again, which is
        // what makes the wrapper worth having.
        "bind" if words.len() >= 3 => parse::VERBS.iter().map(|s| s.to_string()).collect(),

        _ => Vec::new(),
    }
}

/// `set shuffle=` completes the values valid for that key.
fn setting_values(key: SettingKey) -> Vec<String> {
    match key {
        SettingKey::AaaMode => vec!["all".into(), "artist".into(), "album".into()],
        SettingKey::ResampleQuality => vec!["fast".into(), "balanced".into(), "high".into()],
        SettingKey::ReplayGain => vec!["off".into(), "track".into(), "album".into(), "auto".into()],
        _ => vec!["true".into(), "false".into()],
    }
}

/// Split into everything before the final word, and the final word itself.
///
/// A trailing space means a *new* word is starting, so the caret is at the
/// beginning of an empty one.
fn split_last_word(line: &str) -> (&str, &str) {
    match line.rfind(char::is_whitespace) {
        Some(i) => line.split_at(i + 1),
        None => ("", line),
    }
}

fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut len = first.len();
    for other in &items[1..] {
        len = len.min(
            first
                .chars()
                .zip(other.chars())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    first.chars().take(len).collect()
}

/// `set key=value` is one word, so the value half needs its own handling.
pub fn complete_setting_value(word: &str) -> Option<Completion> {
    let (key_text, typed) = word.split_once('=')?;
    let key = SettingKey::parse(key_text)?;
    let matches: Vec<String> = setting_values(key)
        .into_iter()
        .filter(|v| v.starts_with(typed))
        .collect();
    if matches.is_empty() {
        return None;
    }
    let extended = common_prefix(&matches);
    Some(Completion {
        line: format!("{key_text}={extended}"),
        options: matches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        World {
            catalogs: vec!["general".into(), "dance".into()],
            playlists: vec!["fresh".into(), "neglected".into()],
            bound: vec![
                (Context::Common, "j".into()),
                (Context::Common, "g g".into()),
                (Context::Playlist, "x".into()),
            ],
        }
    }

    fn line_of(input: &str) -> String {
        complete(input, &world())
            .map(|c| c.line)
            .unwrap_or_default()
    }

    #[test]
    fn a_verb_completes_to_its_unambiguous_extension() {
        // `search`, `search-next` and `search-prev` agree this far, so it
        // stops on the bare verb without claiming to be finished.
        assert_eq!(line_of("sea"), "search");
        // Unique, so it is finished, and says so with a space.
        assert_eq!(line_of("see"), "seek ");
        assert_eq!(line_of("quí"), "");
        assert_eq!(line_of("qui"), "quit ");
    }

    #[test]
    fn an_empty_line_offers_everything() {
        let c = complete("", &world()).unwrap();
        assert_eq!(c.options.len(), parse::VERBS.len());
    }

    #[test]
    fn a_single_match_gains_a_trailing_space() {
        // The next thing you type is an argument, so the space is free.
        assert_eq!(line_of("cata"), "catalog ");
    }

    #[test]
    fn ambiguous_matches_extend_only_as_far_as_they_agree() {
        let c = complete("pan", &world()).unwrap();
        assert_eq!(c.line, "pane-", "should stop where they diverge");
        assert!(c.options.contains(&"pane-next".to_string()));
        assert!(c.options.contains(&"pane-left".to_string()));
    }

    #[test]
    fn nothing_matching_leaves_the_line_alone() {
        assert!(complete("frobnicate", &world()).is_none());
    }

    #[test]
    fn view_completes_to_numbers() {
        let c = complete("view ", &world()).unwrap();
        assert_eq!(c.options, ["1", "2", "3", "4", "5", "6", "7", "8"]);
    }

    #[test]
    fn catalogs_come_from_the_running_application() {
        let c = complete("catalog ", &world()).unwrap();
        assert!(c.options.contains(&"general".to_string()));
        assert!(c.options.contains(&"dance".to_string()));
        assert!(c.options.contains(&"all".to_string()));
        // Adjustments too, since they are how you toggle one off.
        assert!(c.options.contains(&"-dance".to_string()));

        // And a catalog list keeps completing after the first name.
        let c = complete("catalog general da", &world()).unwrap();
        assert_eq!(c.line, "catalog general dance ");
    }

    #[test]
    fn set_offers_keys_with_the_equals_already_there() {
        let c = complete("set shuf", &world()).unwrap();
        assert_eq!(c.line, "set shuffle= ");
    }

    /// The interesting one: which values are valid depends on the key.
    #[test]
    fn set_values_depend_on_the_key() {
        let c = complete_setting_value("aaa_mode=a").unwrap();
        assert_eq!(c.options, ["all", "artist", "album"]);

        let c = complete_setting_value("resample_quality=").unwrap();
        assert_eq!(c.options, ["fast", "balanced", "high"]);

        let c = complete_setting_value("shuffle=t").unwrap();
        assert_eq!(c.line, "shuffle=true");

        assert!(complete_setting_value("nosuch=x").is_none());
    }

    #[test]
    fn toggle_omits_the_setting_that_is_not_a_boolean() {
        let c = complete("toggle ", &world()).unwrap();
        assert!(c.options.contains(&"shuffle".to_string()));
        assert!(
            !c.options.contains(&"resample_quality".to_string()),
            "resample_quality has no toggle"
        );
    }

    #[test]
    fn bind_completes_context_then_key_then_a_whole_command() {
        let c = complete("bind ", &world()).unwrap();
        assert!(c.options.contains(&"library".to_string()));

        let c = complete("bind common pag", &world()).unwrap();
        assert!(c.options.contains(&"pageup".to_string()));

        // The third field is a command again, recursively.
        let c = complete("bind common x play", &world()).unwrap();
        assert!(c.options.contains(&"play".to_string()));
    }

    #[test]
    fn unbind_offers_only_keys_that_are_bound() {
        let c = complete("unbind playlist ", &world()).unwrap();
        assert!(c.options.contains(&"x".to_string()), "{:?}", c.options);
        // Common bindings apply in every context, so they are offered too.
        assert!(c.options.contains(&"j".to_string()));

        let c = complete("unbind queue ", &world()).unwrap();
        assert!(!c.options.contains(&"x".to_string()), "x is playlist-only");
    }

    #[test]
    fn in_completes_a_pane_then_a_command() {
        let c = complete("in ", &world()).unwrap();
        assert_eq!(c.options, ["tree", "tracks"]);

        let c = complete("in tree d", &world()).unwrap();
        assert_eq!(c.line, "in tree down ");
    }

    #[test]
    fn a_verb_with_no_completable_arguments_offers_nothing() {
        // Numbers cannot be guessed at.
        assert!(complete("seek ", &world()).is_none());
        assert!(complete("vol ", &world()).is_none());
    }

    #[test]
    fn common_prefix_of_one_is_itself() {
        assert_eq!(common_prefix(&["only".to_string()]), "only");
        assert_eq!(common_prefix(&[]), "");
    }
}
