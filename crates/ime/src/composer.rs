//! The composition state machine: romaji input, conversion, clause focus,
//! candidate selection, commit and cancel.
//!
//! It does not touch TSF. Each key updates the state and queues [`Effect`]s,
//! which the text service then applies to the document in order. Effects
//! queued before a worker failure stay queued, so a commit made earlier in
//! the same key still reaches the document.

use crate::{
    candidates::{CandidateList, Page},
    keys::Key,
    worker::{Converter, WorkerError},
};
use simeji_protocol::{Candidate, Clause, CloudWord};
use std::ops::Range;

/// Typing beyond this much romaji first commits what was typed so far.
const MAX_ROMAJI_BYTES: usize = 512;

/// What the document shows for the composition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct View {
    pub text: String,
    /// Whether `text` is conversion output rather than the typed reading.
    pub converted: bool,
    /// UTF-16 range of the focused clause within `text`.
    pub focus: Option<Range<usize>>,
    /// The candidate window contents; `None` hides the window.
    pub candidates: Option<Page>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Show the composition, starting one if needed.
    Show(View),
    /// Replace the composition with `text` and end it. `accepted` is the
    /// engine candidate the user chose, to learn if the field allows it.
    Commit {
        text: String,
        accepted: Option<Candidate>,
    },
    /// Remove the composition text and end the composition.
    Cancel,
    /// Look up cloud candidates for this reading if the field allows it.
    QueueCloud(String),
}

#[derive(Debug)]
pub(crate) struct Composer {
    /// Japanese input is on. 半角/全角 switches it.
    enabled: bool,
    romaji: String,
    reading: String,
    /// The converted sentence; empty until the user converts.
    clauses: Vec<Clause>,
    focus: usize,
    candidates: CandidateList,
    /// A selected whole-sentence cloud candidate, shown instead of the clauses.
    sentence: Option<String>,
    /// The engine candidate to learn when the composition is committed.
    accepted: Option<Candidate>,
    cloud_enabled: bool,
    cloud_words: Vec<CloudWord>,
    effects: Vec<Effect>,
}

impl Composer {
    pub fn new() -> Self {
        Self {
            enabled: true,
            romaji: String::new(),
            reading: String::new(),
            clauses: Vec::new(),
            focus: 0,
            candidates: CandidateList::default(),
            sentence: None,
            accepted: None,
            cloud_enabled: false,
            cloud_words: Vec::new(),
            effects: Vec::new(),
        }
    }

    pub fn is_composing(&self) -> bool {
        !self.romaji.is_empty()
    }

    fn is_converting(&self) -> bool {
        !self.clauses.is_empty()
    }

    /// Whether the key is for the composer rather than the application.
    pub fn accepts(&self, key: Key) -> bool {
        match key {
            Key::ToggleInput => true,
            _ if !self.enabled => false,
            Key::Printable { .. } => true,
            _ => self.is_composing(),
        }
    }

    /// Handles a key and returns whether it was consumed. `typed` yields the
    /// text the key types; it is only asked for keys that type text.
    pub fn press(
        &mut self,
        key: Key,
        typed: impl FnOnce() -> Option<String>,
        converter: &mut impl Converter,
    ) -> Result<bool, WorkerError> {
        if !self.accepts(key) {
            return Ok(false);
        }
        match key {
            Key::ToggleInput => {
                if self.is_composing() {
                    self.commit();
                }
                self.enabled = !self.enabled;
            }
            Key::Enter => self.commit(),
            Key::Escape if self.is_converting() => {
                self.revert();
                self.show();
            }
            Key::Escape => self.cancel(),
            Key::Space { reverse } => {
                if self.is_converting() {
                    self.step(if reverse { -1 } else { 1 });
                } else {
                    self.convert(converter)?;
                }
                self.show();
            }
            Key::Left | Key::Right if self.is_converting() => {
                self.focus = if key == Key::Left {
                    self.focus.saturating_sub(1)
                } else {
                    (self.focus + 1).min(self.clauses.len() - 1)
                };
                self.load_candidates(converter)?;
                self.show();
            }
            Key::Up | Key::Down if self.is_converting() => {
                self.step(if key == Key::Up { -1 } else { 1 });
                self.show();
            }
            Key::Printable {
                candidate: Some(number),
            } if !self.candidates.is_empty() => {
                if self.candidates.pick(number).is_some() {
                    self.apply_selection();
                    self.commit();
                }
            }
            Key::Backspace => {
                self.romaji.pop();
                self.revert();
                return self.refresh(converter);
            }
            // Other printable keys type text. Arrow keys get here only
            // before conversion; they type nothing and go to the application.
            _ => return self.type_text(typed, converter),
        }
        Ok(true)
    }

    /// The effects queued since the last call, in the order to apply them.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Clears the composition, keeping the input mode and cloud setting.
    pub fn reset(&mut self) {
        self.romaji.clear();
        self.reading.clear();
        self.focus = 0;
        self.revert();
        self.cloud_words.clear();
    }

    pub fn set_cloud_enabled(&mut self, enabled: bool) {
        self.cloud_enabled = enabled;
    }

    pub fn discard_cloud_words(&mut self) {
        self.cloud_words.clear();
    }

    /// The reading to poll cloud results for: cloud conversion is on, the
    /// user is typing, has not converted yet and no result has arrived.
    pub fn cloud_reading(&self) -> Option<&str> {
        (self.cloud_enabled
            && self.is_composing()
            && !self.is_converting()
            && self.cloud_words.is_empty())
        .then_some(self.reading.as_str())
    }

    /// Keeps cloud results if they are still for the current, unconverted reading.
    pub fn receive_cloud_words(&mut self, reading: &str, words: Vec<CloudWord>) {
        if self.reading == reading && !self.is_converting() {
            self.cloud_words = words;
        }
    }

    fn type_text(
        &mut self,
        typed: impl FnOnce() -> Option<String>,
        converter: &mut impl Converter,
    ) -> Result<bool, WorkerError> {
        if self.is_converting() {
            self.commit();
        }
        let Some(text) = typed() else {
            return Ok(false);
        };
        if self.romaji.len() + text.len() > MAX_ROMAJI_BYTES {
            self.commit();
        }
        self.romaji.push_str(&text);
        self.refresh(converter)
    }

    /// Updates the reading after the romaji changed.
    fn refresh(&mut self, converter: &mut impl Converter) -> Result<bool, WorkerError> {
        if self.romaji.is_empty() {
            self.cancel();
            return Ok(true);
        }
        self.reading = converter.transliterate(&self.romaji)?;
        self.cloud_words.clear();
        self.show();
        self.effects.push(Effect::QueueCloud(self.reading.clone()));
        Ok(true)
    }

    fn convert(&mut self, converter: &mut impl Converter) -> Result<(), WorkerError> {
        let conversion = converter.convert(&self.reading)?;
        self.accepted = conversion.candidates.into_iter().next();
        self.clauses = conversion.clauses;
        self.focus = 0;
        self.load_candidates(converter)
    }

    /// Fetches the alternatives for the focused clause.
    fn load_candidates(&mut self, converter: &mut impl Converter) -> Result<(), WorkerError> {
        let Some(clause) = self.clauses.get(self.focus) else {
            return Ok(());
        };
        let context = self.clauses[..self.focus]
            .iter()
            .map(|clause| clause.text.as_str())
            .collect::<String>();
        let engine = converter.candidates(&clause.reading, &context)?;
        self.candidates =
            CandidateList::new(engine, &self.cloud_words, &self.reading, &clause.text);
        Ok(())
    }

    fn step(&mut self, step: isize) {
        if self.candidates.step(step).is_some() {
            self.apply_selection();
        }
    }

    /// Puts the selected candidate into the focused clause, or shows the
    /// selected cloud sentence in place of all clauses.
    fn apply_selection(&mut self) {
        let Some(choice) = self.candidates.selected().cloned() else {
            return;
        };
        if choice.whole_sentence {
            self.sentence = Some(choice.candidate.text);
            self.accepted = None;
        } else if let Some(clause) = self.clauses.get_mut(self.focus) {
            self.sentence = None;
            clause.text.clone_from(&choice.candidate.text);
            self.accepted = Some(choice.candidate);
        }
    }

    /// Returns from conversion to the reading.
    fn revert(&mut self) {
        self.clauses.clear();
        self.candidates.clear();
        self.accepted = None;
        self.sentence = None;
    }

    fn commit(&mut self) {
        let text = self.text();
        let accepted = self.accepted.take();
        self.reset();
        self.effects.push(Effect::Commit { text, accepted });
    }

    fn cancel(&mut self) {
        self.reset();
        self.effects.push(Effect::Cancel);
    }

    fn show(&mut self) {
        let view = self.view();
        self.effects.push(Effect::Show(view));
    }

    /// The composition text as the document shows it.
    pub fn text(&self) -> String {
        if let Some(sentence) = &self.sentence {
            sentence.clone()
        } else if self.clauses.is_empty() {
            self.reading.clone()
        } else {
            self.clauses
                .iter()
                .map(|clause| clause.text.as_str())
                .collect()
        }
    }

    fn view(&self) -> View {
        let utf16 = |text: &str| text.encode_utf16().count();
        let focus = match (&self.sentence, self.clauses.get(self.focus)) {
            (None, Some(clause)) => {
                let start = self.clauses[..self.focus]
                    .iter()
                    .map(|clause| utf16(&clause.text))
                    .sum::<usize>();
                Some(start..start + utf16(&clause.text))
            }
            _ => None,
        };
        View {
            text: self.text(),
            converted: self.is_converting(),
            focus,
            candidates: self.candidates.page(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use simeji_protocol::Conversion;

    /// Reads romaji back unchanged and converts every reading to the same clauses.
    struct Fake {
        clauses: Vec<(&'static str, &'static str)>,
        alternatives: Vec<(&'static str, Vec<&'static str>)>,
        requests: Vec<(String, String)>,
        fail: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                clauses: vec![("kyou", "今日"), ("ha", "は")],
                alternatives: vec![("kyou", vec!["今日", "京", "強"]), ("ha", vec!["は", "葉"])],
                requests: Vec::new(),
                fail: false,
            }
        }
    }

    fn candidate(text: &str, reading: &str) -> Candidate {
        Candidate {
            text: text.into(),
            reading: reading.into(),
            property: 0,
            left_id: 0,
            right_id: 0,
        }
    }

    impl Converter for Fake {
        fn transliterate(&mut self, romaji: &str) -> Result<String, WorkerError> {
            if self.fail {
                return Err(WorkerError::Failed("worker stopped".into()));
            }
            Ok(romaji.to_owned())
        }
        fn convert(&mut self, reading: &str) -> Result<Conversion, WorkerError> {
            let text = self
                .clauses
                .iter()
                .map(|(_, text)| *text)
                .collect::<String>();
            Ok(Conversion {
                candidates: vec![candidate(&text, reading)],
                clauses: self
                    .clauses
                    .iter()
                    .map(|(reading, text)| Clause {
                        text: (*text).into(),
                        reading: (*reading).into(),
                    })
                    .collect(),
            })
        }
        fn candidates(
            &mut self,
            reading: &str,
            context: &str,
        ) -> Result<Vec<Candidate>, WorkerError> {
            self.requests.push((reading.into(), context.into()));
            Ok(self
                .alternatives
                .iter()
                .find(|(clause, _)| *clause == reading)
                .map(|(_, texts)| texts.iter().map(|text| candidate(text, reading)).collect())
                .unwrap_or_default())
        }
    }

    fn press(composer: &mut Composer, converter: &mut Fake, key: Key) -> bool {
        composer.press(key, || None, converter).unwrap()
    }

    fn type_text(composer: &mut Composer, converter: &mut Fake, text: &str) {
        for character in text.chars() {
            let typed = || Some(character.to_string());
            let key = Key::Printable { candidate: None };
            assert!(composer.press(key, typed, converter).unwrap());
        }
    }

    const SPACE: Key = Key::Space { reverse: false };

    fn reading(text: &str) -> View {
        View {
            text: text.into(),
            converted: false,
            focus: None,
            candidates: None,
        }
    }

    fn page(items: &[&str], selected: usize) -> Option<Page> {
        Some(Page {
            items: items.iter().map(|item| (*item).into()).collect(),
            selected,
            total: items.len(),
        })
    }

    /// Types "kyouha" and converts it to 今日|は with 今日 focused.
    fn converted() -> (Composer, Fake) {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyouha");
        assert!(press(&mut composer, &mut converter, SPACE));
        composer.take_effects();
        (composer, converter)
    }

    fn shown(composer: &mut Composer) -> View {
        match composer.take_effects().as_slice() {
            [Effect::Show(view)] => view.clone(),
            effects => panic!("expected one view, got {effects:?}"),
        }
    }

    #[test]
    fn typing_shows_the_reading_and_queues_a_cloud_lookup() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        assert!(!composer.is_composing());
        type_text(&mut composer, &mut converter, "ka");
        assert_eq!(
            composer.take_effects(),
            [
                Effect::Show(reading("k")),
                Effect::QueueCloud("k".into()),
                Effect::Show(reading("ka")),
                Effect::QueueCloud("ka".into()),
            ]
        );
    }

    #[test]
    fn enter_commits_the_reading_without_a_candidate_to_learn() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "ab");
        composer.take_effects();
        assert!(press(&mut composer, &mut converter, Key::Enter));
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "ab".into(),
                accepted: None,
            }]
        );
        assert!(!composer.is_composing());
        assert!(!composer.accepts(Key::Enter));
    }

    #[test]
    fn space_converts_and_enter_commits_the_sentence_for_learning() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyouha");
        composer.take_effects();
        assert!(press(&mut composer, &mut converter, SPACE));
        assert_eq!(
            shown(&mut composer),
            View {
                text: "今日は".into(),
                converted: true,
                focus: Some(0..2),
                candidates: page(&["今日", "京", "強"], 0),
            }
        );
        assert_eq!(converter.requests, [("kyou".into(), String::new())]);
        assert!(press(&mut composer, &mut converter, Key::Enter));
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "今日は".into(),
                accepted: Some(candidate("今日は", "kyouha")),
            }]
        );
    }

    #[test]
    fn space_shift_space_and_arrows_cycle_through_candidates() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(shown(&mut composer).text, "京は");
        press(&mut composer, &mut converter, Key::Space { reverse: true });
        assert_eq!(shown(&mut composer).text, "今日は");
        press(&mut composer, &mut converter, Key::Up);
        let view = shown(&mut composer);
        assert_eq!(view.text, "強は");
        assert_eq!(view.candidates, page(&["今日", "京", "強"], 2));
        press(&mut composer, &mut converter, Key::Down);
        assert_eq!(shown(&mut composer).text, "今日は");
        press(&mut composer, &mut converter, Key::Down);
        press(&mut composer, &mut converter, Key::Enter);
        assert_eq!(
            composer.take_effects()[1],
            Effect::Commit {
                text: "京は".into(),
                accepted: Some(candidate("京", "kyou")),
            }
        );
    }

    #[test]
    fn left_and_right_move_the_focus_with_the_preceding_text_as_context() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, Key::Right);
        assert_eq!(
            shown(&mut composer),
            View {
                text: "今日は".into(),
                converted: true,
                focus: Some(2..3),
                candidates: page(&["は", "葉"], 0),
            }
        );
        assert_eq!(converter.requests[1], ("ha".into(), "今日".into()));
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(shown(&mut composer).text, "今日葉");
        // The focus stops at the last clause.
        press(&mut composer, &mut converter, Key::Right);
        assert_eq!(shown(&mut composer).focus, Some(2..3));
        press(&mut composer, &mut converter, Key::Left);
        let view = shown(&mut composer);
        assert_eq!(view.focus, Some(0..2));
        assert_eq!(view.candidates, page(&["今日", "京", "強"], 0));
        assert_eq!(
            converter.requests.last().unwrap(),
            &("kyou".into(), String::new())
        );
    }

    #[test]
    fn number_keys_commit_the_candidate_with_that_number() {
        let (mut composer, mut converter) = converted();
        // There is no fifth candidate: the key is consumed and nothing changes.
        assert!(press(
            &mut composer,
            &mut converter,
            Key::Printable { candidate: Some(5) }
        ));
        assert!(composer.take_effects().is_empty());
        assert!(press(
            &mut composer,
            &mut converter,
            Key::Printable { candidate: Some(2) }
        ));
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "京は".into(),
                accepted: Some(candidate("京", "kyou")),
            }]
        );
    }

    #[test]
    fn number_keys_type_digits_before_conversion() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        let key = Key::Printable { candidate: Some(1) };
        assert!(
            composer
                .press(key, || Some("1".into()), &mut converter)
                .unwrap()
        );
        assert_eq!(composer.take_effects()[0], Effect::Show(reading("1")));
    }

    #[test]
    fn escape_returns_to_the_reading_and_then_cancels() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, Key::Escape);
        assert_eq!(shown(&mut composer), reading("kyouha"));
        press(&mut composer, &mut converter, Key::Escape);
        assert_eq!(composer.take_effects(), [Effect::Cancel]);
        assert!(!composer.is_composing());
    }

    #[test]
    fn backspace_removes_romaji_and_cancels_when_nothing_is_left() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, Key::Backspace);
        assert_eq!(
            composer.take_effects(),
            [
                Effect::Show(reading("kyouh")),
                Effect::QueueCloud("kyouh".into())
            ]
        );
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "a");
        composer.take_effects();
        press(&mut composer, &mut converter, Key::Backspace);
        assert_eq!(composer.take_effects(), [Effect::Cancel]);
    }

    #[test]
    fn typing_during_conversion_commits_it_first() {
        let (mut composer, mut converter) = converted();
        type_text(&mut composer, &mut converter, "a");
        assert_eq!(
            composer.take_effects(),
            [
                Effect::Commit {
                    text: "今日は".into(),
                    accepted: Some(candidate("今日は", "kyouha")),
                },
                Effect::Show(reading("a")),
                Effect::QueueCloud("a".into()),
            ]
        );
    }

    #[test]
    fn keys_that_type_nothing_go_to_the_application() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "a");
        composer.take_effects();
        // Before conversion the arrows are not composer keys.
        assert!(composer.accepts(Key::Left));
        assert!(!press(&mut composer, &mut converter, Key::Left));
        assert!(composer.take_effects().is_empty());
        let (mut composer, mut converter) = converted();
        assert!(!press(
            &mut composer,
            &mut converter,
            Key::Printable { candidate: None }
        ));
        assert!(matches!(
            composer.take_effects().as_slice(),
            [Effect::Commit { .. }]
        ));
    }

    #[test]
    fn the_toggle_key_commits_and_switches_japanese_input() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "a");
        composer.take_effects();
        assert!(press(&mut composer, &mut converter, Key::ToggleInput));
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "a".into(),
                accepted: None,
            }]
        );
        let printable = Key::Printable { candidate: None };
        assert!(!composer.accepts(printable));
        assert!(!press(&mut composer, &mut converter, printable));
        assert!(composer.accepts(Key::ToggleInput));
        assert!(press(&mut composer, &mut converter, Key::ToggleInput));
        assert!(composer.accepts(printable));
        assert!(composer.take_effects().is_empty());
    }

    #[test]
    fn long_romaji_is_committed_before_it_grows_further() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, &"a".repeat(MAX_ROMAJI_BYTES));
        composer.take_effects();
        type_text(&mut composer, &mut converter, "b");
        assert_eq!(
            composer.take_effects()[..2],
            [
                Effect::Commit {
                    text: "a".repeat(MAX_ROMAJI_BYTES),
                    accepted: None,
                },
                Effect::Show(reading("b")),
            ]
        );
    }

    #[test]
    fn a_worker_failure_keeps_the_commit_that_preceded_it() {
        let (mut composer, mut converter) = converted();
        converter.fail = true;
        let result = composer.press(
            Key::Printable { candidate: None },
            || Some("a".into()),
            &mut converter,
        );
        assert!(result.is_err());
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "今日は".into(),
                accepted: Some(candidate("今日は", "kyouha")),
            }]
        );
        assert!(composer.is_composing());
    }

    #[test]
    fn cloud_sentences_join_the_candidates_and_replace_all_clauses() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        composer.set_cloud_enabled(true);
        type_text(&mut composer, &mut converter, "kyouha");
        assert_eq!(composer.cloud_reading(), Some("kyouha"));
        let words = vec![CloudWord {
            text: "今日は晴れ".into(),
            reading: "kyouha".into(),
            property: 0,
            kind: 0,
            scene: String::new(),
        }];
        composer.receive_cloud_words("kyou", words.clone());
        assert_eq!(composer.cloud_reading(), Some("kyouha"));
        composer.receive_cloud_words("kyouha", words);
        assert_eq!(composer.cloud_reading(), None);
        composer.take_effects();
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(
            shown(&mut composer).candidates,
            page(&["今日", "今日は晴れ", "京", "強"], 0)
        );
        press(&mut composer, &mut converter, SPACE);
        let view = shown(&mut composer);
        assert_eq!((view.text.as_str(), view.focus), ("今日は晴れ", None));
        press(&mut composer, &mut converter, Key::Enter);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "今日は晴れ".into(),
                accepted: None,
            }]
        );
    }

    #[test]
    fn cloud_polling_waits_for_typing_and_stops_after_conversion() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        composer.set_cloud_enabled(true);
        assert_eq!(composer.cloud_reading(), None);
        type_text(&mut composer, &mut converter, "kyouha");
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(composer.cloud_reading(), None);
        composer.set_cloud_enabled(false);
        press(&mut composer, &mut converter, Key::Escape);
        assert_eq!(composer.cloud_reading(), None);
    }

    #[test]
    fn text_follows_the_selected_candidates() {
        let (mut composer, mut converter) = converted();
        assert_eq!(composer.text(), "今日は");
        press(&mut composer, &mut converter, Key::Right);
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(composer.text(), "今日葉");
        composer.reset();
        assert_eq!(composer.text(), "");
        assert!(!composer.is_composing());
    }
}
