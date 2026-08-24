//! Typing, for the few things that genuinely cannot be tapped.
//!
//! This is a composite, not a primitive. The protocol has no keyboard node,
//! the layout engine has never heard of one, and the renderer draws it as what
//! it is: four grids of tappable cells. That was a deliberate decision, a
//! keyboard, a calculator, a board and a colour picker are all the same shape,
//! and adding a node for each would mean adding it to the wire format, the
//! layout engine, the renderer and the hit test before anybody could use it.
//! Everything here is built out of [`ScreenBuilder::grid`].
//!
//! ## Why the keys are addressed by position
//!
//! A key's identity is where it is, not what it types. `Shift` and the layer
//! keys change every label on the panel but no cell moves, so a tap resolves
//! to a position and this module decides what that position means right now.
//! Naming keys after their characters instead would need a different action id
//! per layer, which with three layers and a shift is six times the identifiers
//! and one more thing to get out of step with what is drawn.
//!
//! Every layer therefore holds exactly as many characters per row as the
//! letters do. A constant asserts it, because a short row would slide every
//! key after it onto a neighbour's cell and nothing at runtime could tell.
//!
//! ## Why there is no cursor
//!
//! Text is only ever appended to or deleted from the end. Moving a caret needs
//! a caret to be visible, which needs a repaint per tap on a panel that takes
//! tens of milliseconds a repaint, and the payoff is editing in the middle of
//! a search query. Applications that want prose should be asking a question
//! with [`ScreenBuilder::choose`] instead.

use crate::ScreenBuilder;
use kobo_ui::ActionId;

/// The longest string the keyboard will accumulate.
///
/// A bound exists because the text is redrawn on the panel on every keystroke,
/// and an unbounded string would eventually take longer to lay out than to
/// type. It is generous enough for a search query or a chat message.
pub const MAX_TEXT: usize = 512;

/// Letters, in the arrangement every reader already knows.
const LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

/// The second layer. Chosen for what these applications actually need: digits,
/// the punctuation of a sentence, and the handful of symbols that appear in
/// search queries and addresses.
const SYMBOLS: [&str; 3] = ["1234567890", "-/:;()&@\"", ".,?!'+="];

/// The third layer: every letter French writes with a diacritic, both
/// ligatures, and the letters the rest of the catalogue needs.
///
/// This is not decoration. The catalogues these applications search are
/// Québécois, so a majority of titles and author names carry one of these, and
/// without them the search field cannot express what the reader is holding.
///
/// What earns a key is a character with no substitute a reader could think to
/// type instead. That is why the last five are Spanish and Portuguese rather
/// than more punctuation: an em dash, an ellipsis and a typographic apostrophe
/// have `-`, `.` and `'` waiting on the symbol layer, and telling them apart is
/// the search engine's job, not the panel's. The guillemets stay because ASCII
/// has no French quotation mark, and this keyboard writes messages and notes as
/// well as queries.
///
/// Grouped by base letter rather than by frequency: a reader hunting for `ë`
/// finds it next to `é`, which is where the eye goes first.
const ACCENTS: [&str; 3] = ["àâäçéèêëîï", "ôöùûüÿœæñ", "«»áíóúã"];

/// Every layer is the same shape, because a key's identity is its position.
///
/// A layer with a row of a different length would move every key to its right
/// and silently change what the cells before it type, which is the one failure
/// the position scheme cannot detect at runtime. Cheaper to refuse to compile.
const _: () = {
    let mut row = 0;
    while row < 3 {
        assert!(row_length(SYMBOLS[row]) == row_length(LETTERS[row]));
        assert!(row_length(ACCENTS[row]) == row_length(LETTERS[row]));
        row += 1;
    }
};

/// Characters, not bytes: the accent layer's keys are two bytes each.
const fn row_length(row: &str) -> usize {
    let bytes = row.as_bytes();
    let mut characters = 0;
    let mut index = 0;
    while index < bytes.len() {
        // A continuation byte is the tail of a character already counted.
        if bytes[index] & 0b1100_0000 != 0b1000_0000 {
            characters += 1;
        }
        index += 1;
    }
    characters
}

/// What a tap on the keyboard meant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pressed {
    /// The text changed. The caller should repaint.
    Edited,
    /// A modifier changed. The labels changed, so the caller should repaint,
    /// but the text did not.
    Shifted,
    /// The reader asked to accept what they typed.
    Submitted,
}

/// Which set of characters the keys are currently showing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Layer {
    #[default]
    Letters,
    Symbols,
    /// The French diacritics. Reached by its own key rather than by cycling
    /// through the symbols, because a Québécois catalogue needs it on most
    /// searches and a cycle would charge two taps for the common case.
    Accents,
}

/// An on-screen keyboard and the text typed on it.
///
/// The application owns one of these, hands it to
/// [`ScreenBuilder::keyboard`] when it draws, and gives it every action it
/// does not recognise.
#[derive(Clone, Debug, Default)]
pub struct Keyboard {
    text: String,
    shift: bool,
    layer: Layer,
}

impl Keyboard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts with text already typed, truncated to [`MAX_TEXT`].
    #[must_use]
    pub fn with_text(text: impl Into<String>) -> Self {
        let mut keyboard = Self::default();
        for character in text.into().chars() {
            keyboard.push(character);
        }
        keyboard
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    #[must_use]
    pub fn layer(&self) -> Layer {
        self.layer
    }

    #[must_use]
    pub fn is_shifted(&self) -> bool {
        self.shift
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    /// Takes the text, leaving the keyboard empty and ready for the next entry.
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.text)
    }

    /// Applies `action` if it belongs to the keyboard.
    ///
    /// Returns `None` for anything else, so an application can pass every
    /// action it receives here first and handle its own afterwards without
    /// having to know which identifiers the keyboard claimed.
    pub fn press(&mut self, action: ActionId) -> Option<Pressed> {
        if action == crate::action_id(SHIFT) {
            self.shift = !self.shift;
            return Some(Pressed::Shifted);
        }
        if action == crate::action_id(LAYER) {
            self.layer = match self.layer {
                Layer::Symbols => Layer::Letters,
                Layer::Letters | Layer::Accents => Layer::Symbols,
            };
            return Some(Pressed::Shifted);
        }
        if action == crate::action_id(ACCENT_LAYER) {
            self.layer = match self.layer {
                Layer::Accents => Layer::Letters,
                Layer::Letters | Layer::Symbols => Layer::Accents,
            };
            return Some(Pressed::Shifted);
        }
        if action == crate::action_id(SPACE) {
            self.push(' ');
            return Some(Pressed::Edited);
        }
        if action == crate::action_id(BACKSPACE) {
            // By character, not by byte. Truncating a multi-byte character
            // would leave the string invalid, and this is the one operation a
            // reader repeats without looking.
            self.text.pop();
            return Some(Pressed::Edited);
        }
        if action == crate::action_id(ENTER) {
            return Some(Pressed::Submitted);
        }
        let character = self.character_at(action)?;
        self.push(character);
        // A shift applies to exactly one letter, as on every phone. Leaving it
        // latched would mean reading the panel to find out what the next key
        // will do.
        self.shift = false;
        Some(Pressed::Edited)
    }

    /// The character this key would type right now, without typing it.
    ///
    /// A terminal needs this: a keystroke there is sent to a program the
    /// instant it happens rather than accumulated into a field, so the layout
    /// and the modifier state are wanted but the text buffer is not.
    #[must_use]
    pub fn resolves(&self, action: ActionId) -> Option<char> {
        self.character_at(action)
    }

    fn character_at(&self, action: ActionId) -> Option<char> {
        let rows = self.rows();
        for (row, characters) in rows.iter().enumerate() {
            for (column, character) in characters.chars().enumerate() {
                if action == crate::action_id(&key_name(row, column)) {
                    return Some(if self.shift {
                        uppercase(character)
                    } else {
                        character
                    });
                }
            }
        }
        None
    }

    pub(crate) fn rows(&self) -> [&'static str; 3] {
        match self.layer {
            Layer::Letters => LETTERS,
            Layer::Symbols => SYMBOLS,
            Layer::Accents => ACCENTS,
        }
    }

    fn push(&mut self, character: char) {
        // Counted in characters rather than bytes so the limit means the same
        // thing whatever alphabet the reader is using.
        if self.text.chars().count() < MAX_TEXT {
            self.text.push(character);
        }
    }
}

/// What a tap did to a [`TextEntry`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Typing {
    /// The field opened, changed, or a modifier moved. Repaint.
    Changed,
    /// The reader accepted what they typed. The text comes with it, and the
    /// field is closed and empty again, so the next entry starts clean.
    Submitted(String),
    /// The reader backed out. Nothing was entered.
    Cancelled,
}

/// A field that raises the keyboard when it is tapped, and puts it away again.
///
/// This exists because "tap the row, get a keyboard, get the text back" was
/// being written out by hand in every application, and every application got a
/// slightly different one. The row an application draws with
/// [`ScreenBuilder::or_type`] emits an action like any other; binding that
/// action here is what makes the row open the keyboard, rather than each
/// author remembering to switch a view enum.
///
/// The application still owns its screens: this decides *whether* the keyboard
/// is showing, and [`ScreenBuilder::text_entry`] draws it.
#[derive(Clone, Debug, Default)]
pub struct TextEntry {
    keyboard: Keyboard,
    open: bool,
    opens_on: Option<ActionId>,
}

impl TextEntry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds the action that opens this field.
    ///
    /// Named rather than numbered, and resolved the same way every other
    /// action name is, so it is the same string passed to
    /// [`ScreenBuilder::or_type`].
    #[must_use]
    pub fn opened_by(mut self, name: &str) -> Self {
        self.opens_on = Some(crate::action_id(name));
        self
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    #[must_use]
    pub fn text(&self) -> &str {
        self.keyboard.text()
    }

    #[must_use]
    pub const fn keyboard(&self) -> &Keyboard {
        &self.keyboard
    }

    /// Opens the field with nothing in it.
    pub fn open(&mut self) {
        self.keyboard.clear();
        self.open = true;
    }

    /// Opens the field with `text` already in it, for editing something.
    pub fn open_with(&mut self, text: impl Into<String>) {
        self.keyboard = Keyboard::with_text(text);
        self.open = true;
    }

    /// Closes the field and throws away anything typed.
    pub fn close(&mut self) {
        self.keyboard.clear();
        self.open = false;
    }

    /// Applies `action` if it belongs to this field.
    ///
    /// Returns `None` for anything else, so an application passes every action
    /// here first and handles its own afterwards. While the field is open it
    /// claims the cancel action too, because a keyboard covering the panel is
    /// modal whether or not the author thought of it that way.
    pub fn handle(&mut self, action: ActionId) -> Option<Typing> {
        if !self.open {
            if self.opens_on == Some(action) {
                self.open();
                return Some(Typing::Changed);
            }
            return None;
        }
        if action == crate::action_id(CANCEL) {
            self.close();
            return Some(Typing::Cancelled);
        }
        match self.keyboard.press(action)? {
            Pressed::Edited | Pressed::Shifted => Some(Typing::Changed),
            Pressed::Submitted => {
                let text = self.keyboard.take();
                self.open = false;
                // Whitespace only is nothing. Returning it would make every
                // caller check, and half of them would forget.
                if text.trim().is_empty() {
                    Some(Typing::Cancelled)
                } else {
                    Some(Typing::Submitted(text.trim().to_string()))
                }
            }
        }
    }
}

const CANCEL: &str = "kb.cancel";

const SHIFT: &str = "kb.shift";
const LAYER: &str = "kb.layer";
const ACCENT_LAYER: &str = "kb.accents";
const SPACE: &str = "kb.space";
const BACKSPACE: &str = "kb.backspace";
const ENTER: &str = "kb.enter";

/// The face of the accent key. Three letters of what it opens, which needs no
/// translating and no icon the renderer would have to carry a glyph for.
///
/// The runtime recognises a keyboard by its faces (`kobod`'s `keyboard_keys`),
/// so this string is spelled out there too. Changing it means changing both.
const ACCENT_FACE: &str = "éàç";

fn key_name(row: usize, column: usize) -> String {
    format!("kb.r{row}c{column}")
}

/// The shifted face of a key.
///
/// Unicode's uppercase rather than ASCII's, which is not a detail: `é` is not
/// an ASCII character, so `to_ascii_uppercase` returns it unchanged and a
/// shifted accent layer would have typed lower case while drawing capitals.
///
/// A character whose uppercase is more than one character (German `ß` becomes
/// `SS`) is left as it is. A key types one character, and widening that to a
/// string would spread through [`Keyboard::resolves`] and into every terminal
/// that reads a keystroke straight off the panel. No layer here holds one.
fn uppercase(character: char) -> char {
    let mut upper = character.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => single,
        _ => character,
    }
}

impl ScreenBuilder {
    /// Draws `keyboard`, and the text typed on it so far.
    ///
    /// The rows are separate grids rather than one, because the alphabet's
    /// rows are ten, nine and nine keys wide and a single grid would either
    /// leave a ragged edge or need padding cells that look like keys but do
    /// nothing.
    #[must_use]
    pub fn keyboard(self, keyboard: &Keyboard, submit: &str) -> Self {
        let rows = keyboard.rows();
        // The keys go to the foot of the panel wherever they are added, which
        // is where thumbs are and where every other platform puts them. In
        // flow they landed wherever the prompt above them ended: the todo
        // application's compose screen drew its keyboard across the middle of
        // the panel with a third of a page of paper under it. A second `fill`
        // above this one, as `terminal_keys` has, changes nothing, because a
        // fill only ever pushes down.
        let mut screen = self.fill();
        for (index, characters) in rows.iter().enumerate() {
            let mut cells = Vec::new();
            // Shift and backspace sit on the bottom letter row, where the
            // reader's thumbs already are, and where they are on every phone.
            if index == 2 {
                cells.push((
                    SHIFT.to_string(),
                    // "SHIFT" is 104 pixels of Atkinson in a 98 pixel cell on
                    // the panel that ships, so the latched face was clipped.
                    // One capital is enough of a difference: the loud signal
                    // that shift is on is the other twenty-six keys, which are
                    // all showing capitals at the same time.
                    if keyboard.is_shifted() {
                        "Shift".to_string()
                    } else {
                        "shift".to_string()
                    },
                ));
            }
            for (column, character) in characters.chars().enumerate() {
                let label = if keyboard.is_shifted() {
                    uppercase(character).to_string()
                } else {
                    character.to_string()
                };
                cells.push((key_name(index, column), label));
            }
            if index == 2 {
                cells.push((BACKSPACE.to_string(), "back".to_string()));
            }
            let columns = u8::try_from(cells.len()).unwrap_or(u8::MAX);
            screen = screen.grid(columns, false, cells);
        }
        // Four controls, and each one says where it takes you rather than what
        // it is. A single key cycling three layers would read "?123" on the
        // letters and leave the accents undiscoverable behind it, and cost the
        // French reader two taps for the layer they need most. The two
        // never both say "abc": only one of them leads back to the letters
        // from wherever the keyboard is.
        screen.grid(
            4,
            false,
            [
                (
                    LAYER.to_string(),
                    match keyboard.layer() {
                        Layer::Letters | Layer::Accents => "?123".to_string(),
                        Layer::Symbols => "abc".to_string(),
                    },
                ),
                (
                    ACCENT_LAYER.to_string(),
                    match keyboard.layer() {
                        Layer::Letters | Layer::Symbols => ACCENT_FACE.to_string(),
                        Layer::Accents => "abc".to_string(),
                    },
                ),
                (SPACE.to_string(), "space".to_string()),
                (ENTER.to_string(), submit.to_string()),
            ],
        )
    }

    /// Draws an open [`TextEntry`]: the prompt, the text, the keyboard and a
    /// way out.
    ///
    /// A way out is not optional. The keyboard fills the panel, so without it
    /// a reader who tapped the field by accident has no route back except
    /// submitting something they did not want.
    #[must_use]
    pub fn text_entry(self, entry: &TextEntry, prompt: &str, submit: &str) -> Self {
        self.heading(prompt)
            .typed(entry.keyboard(), "Type here")
            .divider()
            .keyboard(entry.keyboard(), submit)
            .button(CANCEL, "Cancel")
    }

    /// Shows what has been typed, or `placeholder` when nothing has been.
    ///
    /// An empty field draws nothing at all, which on a panel with no cursor
    /// reads as a broken screen rather than an empty one.
    #[must_use]
    pub fn typed(self, keyboard: &Keyboard, placeholder: &str) -> Self {
        if keyboard.is_empty() {
            self.text(placeholder)
        } else {
            self.text(keyboard.text())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TextEntry;
    use super::{key_name, Keyboard, Layer, Pressed, ACCENTS, ACCENT_FACE, MAX_TEXT};
    use crate::{action_id, ScreenBuilder};
    use kobo_ui::{
        Chrome, DisplayMetrics, LayoutIssueKind, LayoutKind, TextScale, CLARA_BW_METRICS,
    };

    /// The panels this keyboard has to fit: the two the hardware gate supports
    /// (`kobo_profile::SUPPORTED_PROFILES`), and the Nia, which is the fewest
    /// pixels the design system is built to reach. A row of ten keys is a
    /// division, so the narrowest panel is the one that decides.
    const PANELS: [(&str, DisplayMetrics); 3] = [
        ("clara", CLARA_BW_METRICS),
        (
            "elipsa-2e",
            DisplayMetrics {
                width: 1404,
                height: 1872,
                pixels_per_inch: 227,
                text_scale: TextScale::Default,
            },
        ),
        (
            "nia",
            DisplayMetrics {
                width: 758,
                height: 1024,
                pixels_per_inch: 212,
                text_scale: TextScale::Default,
            },
        ),
    ];

    fn tap(keyboard: &mut Keyboard, name: &str) -> Option<Pressed> {
        keyboard.press(action_id(name))
    }

    /// Presses whichever layer key leads to `layer` from the letters.
    fn reach(keyboard: &mut Keyboard, layer: Layer) {
        match layer {
            Layer::Letters => {}
            Layer::Symbols => {
                tap(keyboard, "kb.layer");
            }
            Layer::Accents => {
                tap(keyboard, "kb.accents");
            }
        }
        assert_eq!(keyboard.layer(), layer);
    }

    /// Every character a layer can type, written out rather than read from the
    /// constant, so that a test can fail when the layout changes underneath it.
    fn faces(layer: Layer) -> &'static str {
        match layer {
            Layer::Letters => "qwertyuiopasdfghjklzxcvbnm",
            Layer::Symbols => "1234567890-/:;()&@\".,?!'+=",
            Layer::Accents => "àâäçéèêëîïôöùûüÿœæñ«»áíóúã",
        }
    }

    /// A keyboard belongs under the thumbs.
    ///
    /// It was placed in flow, so it started wherever the prompt above it
    /// finished: two lines of text put the keys across the middle of the panel
    /// with five hundred pixels of paper underneath them.
    #[test]
    fn a_keyboard_sits_at_the_foot_of_the_panel() {
        let mut entry = TextEntry::new();
        entry.open();
        let screen = ScreenBuilder::new("todo")
            .top_bar("Todo")
            .text_entry(&entry, "New item", "Add")
            .build();
        let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::measuring(true));
        let bottom = layout
            .nodes
            .iter()
            .map(|node| node.rect.y + node.rect.height)
            .max()
            .unwrap_or(0);
        let content = layout.content;
        let spare = content.y + content.height - bottom;
        assert!(
            (0..=CLARA_BW_METRICS.touch_target_default()).contains(&spare),
            "a compose screen left {spare} pixels of paper under its keyboard"
        );
    }

    #[test]
    fn typing_a_word_produces_that_word() {
        let mut keyboard = Keyboard::new();
        for name in ["kb.r0c0", "kb.r1c0", "kb.r0c2"] {
            tap(&mut keyboard, name);
        }
        assert_eq!(keyboard.text(), "qae");
    }

    #[test]
    fn shift_applies_to_one_letter_and_then_releases() {
        // A latched shift would mean reading the panel before every key to
        // find out what it will do, which is exactly the cost this device
        // cannot afford.
        let mut keyboard = Keyboard::new();
        assert_eq!(tap(&mut keyboard, "kb.shift"), Some(Pressed::Shifted));
        tap(&mut keyboard, "kb.r0c0");
        tap(&mut keyboard, "kb.r0c1");
        assert_eq!(keyboard.text(), "Qw");
        assert!(!keyboard.is_shifted());
    }

    #[test]
    fn the_same_key_types_a_different_character_on_the_symbol_layer() {
        // The whole reason keys are addressed by position: the cell does not
        // move, so the action id must not change either.
        let mut keyboard = Keyboard::new();
        tap(&mut keyboard, "kb.r0c0");
        tap(&mut keyboard, "kb.layer");
        assert_eq!(keyboard.layer(), Layer::Symbols);
        tap(&mut keyboard, "kb.r0c0");
        assert_eq!(keyboard.text(), "q1");
    }

    /// The reason the third layer exists: these titles are on the shelf.
    #[test]
    fn the_accent_layer_types_a_title_from_the_catalogue() {
        let mut keyboard = Keyboard::new();
        reach(&mut keyboard, Layer::Accents);
        // "mûre" and "rafraîchissant" need û and î; neither is on the letters.
        tap(&mut keyboard, "kb.r1c3"); // û
        tap(&mut keyboard, "kb.r0c8"); // î
        tap(&mut keyboard, "kb.accents");
        assert_eq!(keyboard.layer(), Layer::Letters);
        tap(&mut keyboard, "kb.r1c0"); // a
        assert_eq!(keyboard.text(), "ûîa");
    }

    /// Shift used to be `to_ascii_uppercase`, which is a no-op on every one of
    /// these: the panel would have drawn a capital and typed a small letter.
    #[test]
    fn shift_uppercases_every_key_on_the_accent_layer() {
        let mut typed = String::new();
        for (row, characters) in ACCENTS.iter().enumerate() {
            for column in 0..characters.chars().count() {
                let mut keyboard = Keyboard::new();
                reach(&mut keyboard, Layer::Accents);
                tap(&mut keyboard, "kb.shift");
                tap(&mut keyboard, &key_name(row, column));
                typed.push_str(keyboard.text());
            }
        }
        // Written out, not computed: computing it with the same call the code
        // under test uses would pass however wrong that call was. The
        // guillemets have no upper case and come through unchanged.
        assert_eq!(typed, "ÀÂÄÇÉÈÊËÎÏÔÖÙÛÜŸŒÆÑ«»ÁÍÓÚÃ");
    }

    /// A key face that lies about where it goes is worse than no key: the
    /// reader taps it, the panel repaints, and the letters are still gone.
    #[test]
    fn each_layer_key_says_where_it_leads() {
        for layer in [Layer::Letters, Layer::Symbols, Layer::Accents] {
            let mut keyboard = Keyboard::new();
            reach(&mut keyboard, layer);
            let screen = ScreenBuilder::new("keyboard")
                .keyboard(&keyboard, "Send")
                .build();
            let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
            let faces = layout
                .nodes
                .iter()
                .filter(|node| matches!(node.kind, LayoutKind::CellLabel))
                .filter_map(|node| node.text_lines.first().cloned())
                .collect::<Vec<_>>();
            let back = faces.iter().filter(|face| *face == "abc").count();
            assert_eq!(
                back,
                usize::from(layer != Layer::Letters),
                "{layer:?} drew {back} keys offering the letters: {faces:?}"
            );
            for face in [
                if layer == Layer::Accents {
                    "abc"
                } else {
                    ACCENT_FACE
                },
                if layer == Layer::Symbols {
                    "abc"
                } else {
                    "?123"
                },
            ] {
                assert!(faces.iter().any(|drawn| drawn == face), "{layer:?}: {face}");
            }
        }
    }

    /// The control row grew a fourth key, and a key too narrow to hit is a key
    /// that is not there. Checked on the narrowest panel this SDK draws for as
    /// well as the one that ships, because the answer is a division.
    #[test]
    fn every_key_can_be_hit_on_every_panel() {
        for (name, metrics) in PANELS {
            for layer in [Layer::Letters, Layer::Symbols, Layer::Accents] {
                let mut keyboard = Keyboard::new();
                reach(&mut keyboard, layer);
                let mut entry = TextEntry::new();
                entry.open();
                let screen = ScreenBuilder::new("keyboard")
                    .text_entry(&entry, "Search the libraries", "Search")
                    .keyboard(&keyboard, "Search")
                    .build();
                let undersized = screen
                    .diagnostics(&metrics, &Chrome::with_back(true))
                    .issues
                    .into_iter()
                    .filter(|issue| {
                        matches!(issue.kind, LayoutIssueKind::TouchTargetTooSmall { .. })
                    })
                    .collect::<Vec<_>>();
                assert!(undersized.is_empty(), "{name}, {layer:?}: {undersized:?}");
            }
        }
    }

    #[test]
    fn backspace_removes_a_whole_character_rather_than_a_byte() {
        // Truncating by byte would leave the string invalid, and backspace is
        // the one key a reader presses repeatedly without looking.
        let mut keyboard = Keyboard::with_text("café");
        tap(&mut keyboard, "kb.backspace");
        assert_eq!(keyboard.text(), "caf");
    }

    #[test]
    fn backspace_on_an_empty_field_is_harmless() {
        let mut keyboard = Keyboard::new();
        assert_eq!(tap(&mut keyboard, "kb.backspace"), Some(Pressed::Edited));
        assert!(keyboard.is_empty());
    }

    #[test]
    fn enter_submits_without_changing_the_text() {
        let mut keyboard = Keyboard::with_text("dickens");
        assert_eq!(tap(&mut keyboard, "kb.enter"), Some(Pressed::Submitted));
        assert_eq!(keyboard.text(), "dickens");
    }

    #[test]
    fn an_action_the_keyboard_does_not_own_is_handed_back() {
        // Applications pass every action here first, so claiming one that
        // belongs to the application would silently break its own buttons.
        let mut keyboard = Keyboard::new();
        assert_eq!(tap(&mut keyboard, "search"), None);
        assert_eq!(tap(&mut keyboard, "back"), None);
    }

    #[test]
    fn text_stops_growing_at_the_limit() {
        let mut keyboard = Keyboard::with_text("x".repeat(MAX_TEXT + 50));
        assert_eq!(keyboard.text().chars().count(), MAX_TEXT);
        tap(&mut keyboard, "kb.r0c0");
        assert_eq!(keyboard.text().chars().count(), MAX_TEXT);
    }

    #[test]
    fn taking_the_text_leaves_the_keyboard_ready_for_the_next_entry() {
        let mut keyboard = Keyboard::with_text("hello");
        assert_eq!(keyboard.take(), "hello");
        assert!(keyboard.is_empty());
    }

    #[test]
    fn every_drawn_key_is_reachable_by_a_tap_at_its_centre() {
        // The keyboard is drawn by one function and decoded by another, and
        // nothing but this test stops them drifting apart. It lays the real
        // screen out and taps the middle of every cell.
        for layer in [Layer::Letters, Layer::Symbols, Layer::Accents] {
            let mut drawn = Keyboard::new();
            reach(&mut drawn, layer);
            let screen = ScreenBuilder::new("keyboard")
                .keyboard(&drawn, "Send")
                .build();
            let layout = screen.layout_with(&CLARA_BW_METRICS, &Chrome::default());
            let cells = layout
                .nodes
                .iter()
                .filter_map(|node| match node.kind {
                    LayoutKind::Cell(action, ..) => Some((action, node.rect)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(!cells.is_empty(), "the keyboard drew no keys at all");
            let mut typed = String::new();
            for (action, rect) in &cells {
                let x = rect.x + rect.width / 2;
                let y = rect.y + rect.height / 2;
                let hit = layout.hit_test(x, y).expect("a tap inside a key hits it");
                assert_eq!(
                    hit, *action,
                    "a key was covered by something drawn after it"
                );
                let mut keyboard = drawn.clone();
                let outcome = keyboard.press(hit).expect("every drawn key decodes");
                if outcome == Pressed::Edited {
                    typed.push_str(keyboard.text());
                }
            }
            // Every character of the layer, plus the space the space bar types.
            for character in faces(layer).chars() {
                assert!(
                    typed.contains(character),
                    "{character} is drawn but cannot be typed"
                );
            }
            assert!(typed.contains(' '), "the space bar types a space");
        }
    }
}
