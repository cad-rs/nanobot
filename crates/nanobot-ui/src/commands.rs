//! What the command palette and the composer's candidate lists decide before
//! anything is drawn (docs/ui-redesign-design.md §7.1, §8): which commands
//! match what was typed and in what order, which row is selected, and whether
//! the text before the caret is an `@` mention or a `/` command in the making.
//!
//! Plain data, tested without a window.

use std::ops::Range;

/// Something the palette can run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Command {
    /// What running it is addressed by.
    pub id: String,
    pub title: String,
    /// The key that also runs it, shown at the row's end.
    pub keys: Option<String>,
}

impl Command {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            keys: None,
        }
    }

    pub fn with_keys(mut self, keys: impl Into<String>) -> Self {
        self.keys = Some(keys.into());
        self
    }
}

/// How well `title` answers `query`; `None` when it does not. Lower is better.
fn rank(title: &str, query: &str) -> Option<u8> {
    let (title, query) = (title.to_lowercase(), query.to_lowercase());
    if query.is_empty() {
        return Some(0);
    }
    if title.starts_with(&query) {
        return Some(0);
    }
    // The start of a later word: "side" finds "Toggle sidebar".
    let at_word = title
        .match_indices(&query)
        .any(|(ix, _)| title[..ix].ends_with(|c: char| !c.is_alphanumeric()));
    if at_word {
        return Some(1);
    }
    if title.contains(&query) {
        return Some(2);
    }
    // The letters in order, anywhere: "tgsb" finds "Toggle sidebar".
    let mut wanted = query.chars().filter(|c| !c.is_whitespace()).peekable();
    for c in title.chars() {
        if wanted.peek() == Some(&c) {
            wanted.next();
        }
    }
    wanted.peek().is_none().then_some(3)
}

/// The commands that answer `query`, best first; ties keep their given order.
pub fn filter<'a>(commands: &'a [Command], query: &str) -> Vec<&'a Command> {
    let query = query.trim();
    let mut found: Vec<(u8, &Command)> = commands
        .iter()
        .filter_map(|command| rank(&command.title, query).map(|rank| (rank, command)))
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, command)| command).collect()
}

/// Which row of a filtered list is selected.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Selection(usize);

impl Selection {
    /// The selected row among `len`; `None` when there are none.
    pub fn index(self, len: usize) -> Option<usize> {
        (len > 0).then(|| self.0.min(len - 1))
    }

    /// Moves down (`+1`) or up (`-1`) among `len` rows, wrapping at the ends.
    pub fn step(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.0 = 0;
            return;
        }
        let current = self.0.min(len - 1) as isize;
        self.0 = (current + delta).rem_euclid(len as isize) as usize;
    }

    /// Back to the first row: the list under it changed.
    pub fn reset(&mut self) {
        self.0 = 0;
    }
}

/// What the text before the caret is the beginning of.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Trigger {
    /// `@` for a reference to something of the product, `/` for a command.
    pub sigil: char,
    /// The trigger and what follows it, as a byte range of the text: what
    /// accepting a candidate replaces.
    pub range: Range<usize>,
    /// What was typed after the sigil.
    pub query: String,
}

/// The mention or command being typed at byte offset `caret` of `text`.
///
/// `@` starts one anywhere a word could start; `/` only at the start of the
/// text, where a command goes — a path or a fraction further in is not one.
/// Whitespace ends it.
pub fn trigger_at(text: &str, caret: usize) -> Option<Trigger> {
    let before = text.get(..caret)?;
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| !c.is_whitespace())
        .last()
        .map(|(ix, _)| ix)?;
    let word = &before[start..];
    let sigil = word.chars().next()?;
    let allowed = match sigil {
        '@' => true,
        '/' => before[..start].trim().is_empty(),
        _ => false,
    };
    allowed.then(|| Trigger {
        sigil,
        range: start..caret,
        query: word[sigil.len_utf8()..].to_string(),
    })
}

/// One row of a candidate list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Candidate {
    /// What the reference points at (an object's id, a command's name).
    pub id: String,
    /// What the row shows and, for a mention, what the chip shows.
    pub label: String,
    /// A second, quieter piece of text on the row.
    pub detail: Option<String>,
}

impl Candidate {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// How far back a rewind reaches.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum RewindMode {
    /// Drop the conversation after the message; leave the product as it is.
    Conversation,
    /// Drop the conversation and put the product back as it was then.
    ConversationAndState,
    /// Leave this session alone and continue from the message in a new one.
    Fork,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<Command> {
        ["Toggle sidebar", "Toggle dock", "New session", "Stop turn", "Side by side"]
            .into_iter()
            .enumerate()
            .map(|(ix, title)| Command::new(format!("c{ix}"), title))
            .collect()
    }

    fn titles(found: Vec<&Command>) -> Vec<&str> {
        found.into_iter().map(|c| c.title.as_str()).collect()
    }

    #[test]
    fn an_empty_query_lists_everything_in_order() {
        let commands = commands();
        assert_eq!(filter(&commands, "").len(), 5);
        assert_eq!(filter(&commands, "  ")[0].title, "Toggle sidebar");
    }

    #[test]
    fn better_matches_come_first() {
        let commands = commands();
        // A title that starts with it, then a word that does.
        assert_eq!(titles(filter(&commands, "side")), ["Side by side", "Toggle sidebar"]);
        // Case does not matter.
        assert_eq!(titles(filter(&commands, "TOG")), ["Toggle sidebar", "Toggle dock"]);
        // Inside a word.
        assert_eq!(titles(filter(&commands, "essi")), ["New session"]);
        // The letters in order.
        assert_eq!(titles(filter(&commands, "tgdk")), ["Toggle dock"]);
        assert!(filter(&commands, "zzz").is_empty());
    }

    #[test]
    fn the_selection_wraps_and_survives_a_shorter_list() {
        let mut selection = Selection::default();
        assert_eq!(selection.index(0), None);
        assert_eq!(selection.index(3), Some(0));
        selection.step(1, 3);
        selection.step(1, 3);
        assert_eq!(selection.index(3), Some(2));
        selection.step(1, 3);
        assert_eq!(selection.index(3), Some(0), "past the end is the start");
        selection.step(-1, 3);
        assert_eq!(selection.index(3), Some(2), "and the other way");
        // The list shrank under it.
        assert_eq!(selection.index(1), Some(0));
        selection.step(1, 0);
        assert_eq!(selection.index(0), None);
        selection.step(1, 5);
        selection.reset();
        assert_eq!(selection.index(5), Some(0));
    }

    #[test]
    fn a_mention_starts_wherever_a_word_does() {
        let text = "cut holes in @Bod";
        let trigger = trigger_at(text, text.len()).expect("a mention");
        assert_eq!(trigger.sigil, '@');
        assert_eq!(&text[trigger.range.clone()], "@Bod");
        assert_eq!(trigger.query, "Bod");

        assert_eq!(trigger_at("@", 1).unwrap().query, "", "just the sigil lists everything");
        assert_eq!(trigger_at("see @顶面", "see @顶面".len()).unwrap().query, "顶面");
        // The caret decides, not the end of the text.
        assert_eq!(trigger_at("@Body1 and more", 4).unwrap().query, "Bod");
        assert_eq!(trigger_at("@Body1 and more", 10), None);
        // A space ends it; an address is not one.
        assert_eq!(trigger_at("@Body1 ", 7), None);
        assert_eq!(trigger_at("mail a@b", 8), None);
        assert_eq!(trigger_at("", 0), None);
    }

    #[test]
    fn a_command_only_starts_the_message() {
        let trigger = trigger_at("/cle", 4).expect("a command");
        assert_eq!((trigger.sigil, trigger.query.as_str()), ('/', "cle"));
        assert_eq!(trigger.range, 0..4);
        assert_eq!(trigger_at("  /clear", 8).unwrap().range, 2..8, "leading space is fine");
        assert_eq!(trigger_at("open src/main", 13), None, "a path is not a command");
        assert_eq!(trigger_at("scale 1/2", 9), None);
        assert_eq!(trigger_at("/clear now", 10), None, "it ended at the space");
    }
}
