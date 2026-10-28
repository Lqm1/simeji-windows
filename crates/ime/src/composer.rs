//! The composition state machine: typing with a caret, conversion, clause
//! focus and size, candidate selection, commit and cancel, following the
//! key behaviour of Microsoft IME.
//!
//! It does not touch TSF. Each key updates the state and queues [`Effect`]s
//! for the text service to apply in order. Effects queued before a worker
//! failure stay queued, so a commit made earlier in the same key still
//! reaches the document.

use crate::{
    candidates::{CandidateList, CandidateView},
    keys::Key,
    worker::{Converter, WorkerError},
};
use simeji_protocol::{Candidate, CloudCandidate, Conversion as SentenceConversion, Lookup};
use std::ops::Range;

/// Typing beyond this much input first commits what was typed so far.
const MAX_INPUT_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompositionView {
    pub text: String,
    pub converted: bool,
    /// UTF-16 range within `text`.
    pub focus: Option<Range<usize>>,
    /// UTF-16 offset within `text`.
    pub caret: usize,
    pub candidates: Option<CandidateView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Learning {
    pub candidate: Candidate,
    pub previous: Option<Candidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Show(CompositionView),
    /// Replaces the composition with `text` and ends it. `learn` is learned
    /// in order if the field allows it.
    Commit {
        text: String,
        learn: Vec<Learning>,
    },
    Cancel,
    /// The candidates changed while the composition stayed as shown.
    Candidates(Option<CandidateView>),
    /// Starts the background lookups for the reading: the engine's
    /// conversion, and the cloud's if the field allows it.
    Lookup(String),
}

/// What an application that shows the candidates itself asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CandidateAction {
    Select(usize),
    Page(isize),
    /// Commits the composition as shown.
    Commit,
    /// Closes the candidates and returns to the reading.
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputMode {
    Japanese,
    Direct,
}

#[derive(Clone, Debug)]
struct Clause {
    reading: String,
    text: String,
    /// The candidate the user chose or resized the clause to.
    chosen: Option<Candidate>,
}

#[derive(Clone, Debug)]
struct Conversion {
    clauses: Vec<Clause>,
    focus: usize,
    /// The focused clause's alternatives; empty while the window is closed.
    candidates: CandidateList,
    /// Shown instead of the clauses while selected.
    cloud_sentence: Option<String>,
    /// Learned when the user commits without choosing anything.
    sentence: Option<Candidate>,
}

impl Conversion {
    fn reading(&self) -> String {
        self.clauses
            .iter()
            .map(|clause| clause.reading.as_str())
            .collect()
    }

    fn context(&self, index: usize) -> String {
        self.clauses[..index]
            .iter()
            .map(|clause| clause.text.as_str())
            .collect()
    }
}

/// The engine's conversion of a reading, made in the background.
#[derive(Clone, Debug)]
struct LocalLookup {
    reading: String,
    conversion: SentenceConversion,
    /// Alternatives for the conversion's first clause, once they arrive.
    first_clause: Option<Vec<Candidate>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Composer {
    mode: InputMode,
    /// Input left of the caret: kana, followed by the romaji typed since
    /// the caret last moved.
    input: String,
    /// `input` transliterated; romaji that forms no kana yet stays as typed.
    left: String,
    /// Kana right of the caret.
    right: String,
    conversion: Option<Conversion>,
    cloud_enabled: bool,
    /// The reading of the lookups started last.
    requested: Option<String>,
    local: Option<LocalLookup>,
    /// Cloud sentences and the reading they are for.
    cloud: Option<(String, Vec<CloudCandidate>)>,
    /// The candidates shown while typing. They stay until the lookups for
    /// a new reading finish, so the window does not flicker; until then
    /// they cannot be selected.
    suggestions: CandidateList,
    suggestions_reading: Option<String>,
    selecting_suggestion: bool,
    effects: Vec<Effect>,
}

/// A pending `n` at the end becomes `ん` when the input is finished.
fn finish_romaji(reading: &str) -> String {
    match reading.strip_suffix('n') {
        Some(rest) => format!("{rest}ん"),
        None => reading.to_owned(),
    }
}

fn utf16_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn remove_first(text: &mut String) -> Option<char> {
    let first = text.chars().next()?;
    text.drain(..first.len_utf8());
    Some(first)
}

impl Composer {
    pub fn new() -> Self {
        Self {
            mode: InputMode::Japanese,
            input: String::new(),
            left: String::new(),
            right: String::new(),
            conversion: None,
            cloud_enabled: false,
            requested: None,
            local: None,
            cloud: None,
            suggestions: CandidateList::default(),
            suggestions_reading: None,
            selecting_suggestion: false,
            effects: Vec::new(),
        }
    }

    pub fn is_composing(&self) -> bool {
        !self.input.is_empty() || !self.right.is_empty()
    }

    fn is_converting(&self) -> bool {
        self.conversion.is_some()
    }

    fn reading(&self) -> String {
        format!("{}{}", self.left, self.right)
    }

    /// The reading as a conversion reads it, with a pending `n` as `ん`.
    fn conversion_reading(&self) -> String {
        format!("{}{}", finish_romaji(&self.left), self.right)
    }

    /// The reading to look up in the background; none while the romaji
    /// typed last forms no kana yet, as in "ky".
    fn lookup_reading(&self) -> Option<String> {
        let pending = finish_romaji(&self.left)
            .ends_with(|c: char| c.is_ascii_alphabetic() && !"aeiouAEIOU".contains(c));
        let reading = self.conversion_reading();
        (!pending && !reading.is_empty()).then_some(reading)
    }

    /// Whether the key is for the composer rather than the application.
    pub fn accepts(&self, key: Key) -> bool {
        match key {
            Key::ToggleInputMode => true,
            _ if self.mode == InputMode::Direct => false,
            Key::Printable { .. } => true,
            _ => self.is_composing(),
        }
    }

    /// Handles a key and returns whether it was consumed. `typed` yields the
    /// text of a key that types text and is not called for other keys.
    pub fn press(
        &mut self,
        key: Key,
        typed: impl FnOnce() -> Option<String>,
        converter: &mut impl Converter,
    ) -> Result<bool, WorkerError> {
        if !self.accepts(key) {
            return Ok(false);
        }
        if key == Key::ToggleInputMode {
            self.set_mode(match self.mode {
                InputMode::Japanese => InputMode::Direct,
                InputMode::Direct => InputMode::Japanese,
            });
            return Ok(true);
        }
        if self.is_converting() {
            self.press_converting(key, typed, converter)
        } else {
            self.press_composing(key, typed, converter)
        }
    }

    fn press_composing(
        &mut self,
        key: Key,
        typed: impl FnOnce() -> Option<String>,
        converter: &mut impl Converter,
    ) -> Result<bool, WorkerError> {
        if self.selecting_suggestion {
            match key {
                Key::Escape | Key::Backspace | Key::Delete => {
                    self.selecting_suggestion = false;
                    self.show();
                    return Ok(true);
                }
                Key::Printable {
                    candidate_number: Some(number),
                } => {
                    if self.suggestions.select_number(number).is_some() {
                        self.commit();
                    }
                    return Ok(true);
                }
                Key::Printable { .. } => {
                    self.commit();
                    return self.type_text(typed, converter);
                }
                _ => {}
            }
        }
        match key {
            Key::Printable { .. } => return self.type_text(typed, converter),
            Key::Enter => self.commit(),
            Key::Escape => self.cancel(),
            Key::Space { .. } => self.convert(converter)?,
            Key::Backspace => self.delete_before_caret(),
            Key::Delete => self.delete_after_caret(),
            Key::Left { .. } => self.move_caret_left(),
            Key::Right { .. } => self.move_caret_right(),
            Key::Home => self.move_caret_home(),
            Key::End => self.move_caret_end(),
            Key::Tab { shift } | Key::Down { shift } => {
                self.choose_suggestion(if shift { -1 } else { 1 }, converter)?;
            }
            Key::Up { .. } => self.choose_suggestion(-1, converter)?,
            Key::NextCandidatePage | Key::PreviousCandidatePage => self.convert(converter)?,
            Key::ToggleInputMode => {}
        }
        Ok(true)
    }

    fn press_converting(
        &mut self,
        key: Key,
        typed: impl FnOnce() -> Option<String>,
        converter: &mut impl Converter,
    ) -> Result<bool, WorkerError> {
        match key {
            Key::Printable {
                candidate_number: Some(number),
            } if self.candidate_window_open() => {
                if self.select_number(number) {
                    self.commit();
                }
            }
            Key::Printable { .. } => {
                self.commit();
                return self.type_text(typed, converter);
            }
            Key::Enter => self.commit(),
            Key::Escape | Key::Backspace | Key::Delete => self.leave_conversion(),
            Key::Space { shift: false }
            | Key::Down { shift: false }
            | Key::Tab { shift: false } => {
                self.move_selection(1, converter)?;
            }
            Key::Space { shift: true } | Key::Up { shift: false } | Key::Tab { shift: true } => {
                self.move_selection(-1, converter)?;
            }
            Key::Down { shift: true } | Key::NextCandidatePage => self.move_page(1, converter)?,
            Key::Up { shift: true } | Key::PreviousCandidatePage => {
                self.move_page(-1, converter)?;
            }
            Key::Left { shift: false } => self.focus_clause(|focus, _| focus.saturating_sub(1)),
            Key::Right { shift: false } => self.focus_clause(|focus, last| (focus + 1).min(last)),
            Key::Home => self.focus_clause(|_, _| 0),
            Key::End => self.focus_clause(|_, last| last),
            Key::Left { shift: true } => self.resize_clause(false, converter)?,
            Key::Right { shift: true } => self.resize_clause(true, converter)?,
            Key::ToggleInputMode => {}
        }
        Ok(true)
    }

    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// Switches the input mode, committing the composition when leaving
    /// Japanese input.
    pub fn set_mode(&mut self, mode: InputMode) {
        if mode == self.mode {
            return;
        }
        if self.is_composing() {
            self.commit();
        }
        self.mode = mode;
    }

    pub fn act(&mut self, action: CandidateAction) {
        if !self.is_converting() {
            match action {
                CandidateAction::Select(index) if self.suggestions_are_current() => {
                    if self.suggestions.select(index).is_some() {
                        self.selecting_suggestion = true;
                        self.show();
                    }
                }
                CandidateAction::Page(step) if self.suggestions_are_current() => {
                    if self.suggestions.move_page(step).is_some() {
                        self.selecting_suggestion = true;
                        self.show();
                    }
                }
                CandidateAction::Commit if self.is_composing() => self.commit(),
                CandidateAction::Cancel if self.selecting_suggestion => {
                    self.selecting_suggestion = false;
                    self.show();
                }
                _ => {}
            }
            return;
        }
        match action {
            CandidateAction::Page(step) => {
                if let Some(conversion) = self.conversion.as_mut() {
                    conversion.candidates.move_page(step);
                }
                self.apply_selection();
                self.show();
            }
            CandidateAction::Select(index) => {
                let selected = self
                    .conversion
                    .as_mut()
                    .is_some_and(|conversion| conversion.candidates.select(index).is_some());
                if selected {
                    self.apply_selection();
                    self.show();
                }
            }
            CandidateAction::Commit if self.is_composing() => self.commit(),
            CandidateAction::Cancel if self.is_converting() => self.leave_conversion(),
            _ => {}
        }
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Clears the composition, keeping the input mode and the cloud setting.
    pub fn reset(&mut self) {
        self.input.clear();
        self.left.clear();
        self.right.clear();
        self.conversion = None;
        self.forget_lookups();
    }

    /// The text a commit would insert.
    pub fn text(&self) -> String {
        if !self.is_converting()
            && self.selecting_suggestion
            && let Some(choice) = self.suggestions.selected()
        {
            return choice.candidate.text.clone();
        }
        match &self.conversion {
            Some(conversion) => conversion.cloud_sentence.clone().unwrap_or_else(|| {
                conversion
                    .clauses
                    .iter()
                    .map(|clause| clause.text.as_str())
                    .collect()
            }),
            None => format!("{}{}", finish_romaji(&self.left), self.right),
        }
    }

    pub fn set_cloud_enabled(&mut self, enabled: bool) {
        self.cloud_enabled = enabled;
    }

    /// Drops the lookup results; the next change of the reading starts new
    /// lookups.
    pub fn forget_lookups(&mut self) {
        self.selecting_suggestion = false;
        self.requested = None;
        self.local = None;
        self.cloud = None;
        self.suggestions.clear();
        self.suggestions_reading = None;
    }

    /// The reading whose lookups are still on their way, while composing.
    pub fn pending_lookup(&self) -> Option<String> {
        if !self.is_composing() {
            return None;
        }
        let reading = self.lookup_reading()?;
        let local = !self.is_converting()
            && !self
                .local
                .as_ref()
                .is_some_and(|local| local.reading == reading && local.first_clause.is_some());
        let cloud = self.cloud_enabled && self.cloud_for(&reading).is_none();
        (local || cloud).then_some(reading)
    }

    /// Takes in background results that still match the reading. Cloud
    /// sentences join the end of the list without moving the selection.
    pub fn receive_lookup(&mut self, reading: &str, lookup: Lookup) {
        if !self.is_composing() || self.lookup_reading().as_deref() != Some(reading) {
            return;
        }
        let mut changed = false;
        if let Some(cloud) = lookup.cloud
            && self.cloud_enabled
            && self.cloud_for(reading).is_none()
        {
            self.cloud = Some((reading.to_owned(), cloud));
            changed |= self.append_cloud(reading);
        }
        if let Some(conversion) = lookup.conversion {
            let arrived = !self
                .local
                .as_ref()
                .is_some_and(|local| local.reading == reading);
            self.local = Some(LocalLookup {
                reading: reading.to_owned(),
                conversion,
                first_clause: lookup.first_clause,
            });
            if arrived && !self.is_converting() {
                self.refresh_suggestions();
                changed = true;
            }
        }
        if changed {
            let candidates = self.view().candidates;
            self.effects.push(Effect::Candidates(candidates));
        }
    }

    fn cloud_for(&self, reading: &str) -> Option<&[CloudCandidate]> {
        self.cloud
            .as_ref()
            .filter(|(query, _)| query == reading)
            .map(|(_, cloud)| cloud.as_slice())
    }

    /// Adds the cloud sentences for `reading` to the list on screen and
    /// returns whether it changed.
    fn append_cloud(&mut self, reading: &str) -> bool {
        let Some((_, cloud)) = self.cloud.as_ref() else {
            return false;
        };
        let list = match self.conversion.as_mut() {
            Some(conversion) if conversion.reading() == reading => &mut conversion.candidates,
            Some(_) => return false,
            None if self.suggestions_reading.as_deref() == Some(reading) => &mut self.suggestions,
            None => return false,
        };
        if list.is_empty() {
            return false;
        }
        let count = list.len();
        list.append_cloud(cloud, reading);
        list.len() != count
    }

    fn suggestions_are_current(&self) -> bool {
        !self.suggestions.is_empty()
            && self.suggestions_reading.as_deref() == Some(self.conversion_reading().as_str())
    }

    /// Lists the engine's conversion of the reading, then its cloud sentences.
    fn refresh_suggestions(&mut self) {
        let reading = self.conversion_reading();
        let Some(local) = self.local.as_ref().filter(|local| local.reading == reading) else {
            return;
        };
        let mut candidates = local.conversion.candidates.clone();
        if candidates.is_empty() {
            candidates.push(Candidate {
                text: reading.clone(),
                reading: reading.clone(),
                property: 0,
                left_id: 0,
                right_id: 0,
            });
        }
        self.suggestions = CandidateList::new(candidates, &[], &reading, "");
        if let Some(cloud) = self.cloud_for(&reading) {
            let cloud = cloud.to_vec();
            self.suggestions.append_cloud(&cloud, &reading);
        }
        self.suggestions_reading = Some(reading);
    }

    /// Selects among the suggestions, converting the reading first if the
    /// window still shows those of an earlier one.
    fn choose_suggestion(
        &mut self,
        step: isize,
        converter: &mut impl Converter,
    ) -> Result<(), WorkerError> {
        if !self.suggestions_are_current() {
            let reading = self.conversion_reading();
            let looked_up = self
                .local
                .as_ref()
                .is_some_and(|local| local.reading == reading);
            if !looked_up {
                self.local = Some(LocalLookup {
                    conversion: converter.convert_sentence(&reading)?,
                    reading,
                    first_clause: None,
                });
            }
            self.refresh_suggestions();
            self.selecting_suggestion = false;
        }
        if self.suggestions.is_empty() {
            if step > 0 {
                self.move_caret_end();
            }
            return Ok(());
        }
        if self.selecting_suggestion {
            self.suggestions.move_selection(step);
        } else {
            self.selecting_suggestion = true;
        }
        self.show();
        Ok(())
    }

    fn type_text(
        &mut self,
        typed: impl FnOnce() -> Option<String>,
        converter: &mut impl Converter,
    ) -> Result<bool, WorkerError> {
        let Some(text) = typed() else {
            return Ok(false);
        };
        if self.input.len() + self.right.len() + text.len() > MAX_INPUT_BYTES {
            self.commit();
        }
        self.input.push_str(&text);
        self.left = converter.transliterate(&self.input)?;
        self.reading_changed();
        Ok(true)
    }

    /// Shows the new reading and starts its lookups, or cancels the
    /// composition when nothing is left. The candidates on screen stay
    /// until the lookups finish.
    fn reading_changed(&mut self) {
        if !self.is_composing() {
            self.cancel();
            return;
        }
        self.selecting_suggestion = false;
        self.show();
        if let Some(reading) = self.lookup_reading()
            && self.requested.as_ref() != Some(&reading)
        {
            self.requested = Some(reading.clone());
            self.effects.push(Effect::Lookup(reading));
        }
    }

    /// Ends romaji input left of the caret before the caret moves. The
    /// lookups already read a pending `n` as `ん`.
    fn settle_input(&mut self) {
        self.left = finish_romaji(&self.left);
        self.selecting_suggestion = false;
        self.input.clone_from(&self.left);
    }

    fn delete_before_caret(&mut self) {
        if self.left.pop().is_none() {
            return;
        }
        self.input.clone_from(&self.left);
        self.reading_changed();
    }

    fn delete_after_caret(&mut self) {
        if remove_first(&mut self.right).is_some() {
            self.reading_changed();
        }
    }

    fn move_caret_left(&mut self) {
        self.settle_input();
        if let Some(last) = self.left.pop() {
            self.right.insert(0, last);
            self.input.clone_from(&self.left);
        }
        self.show();
    }

    fn move_caret_right(&mut self) {
        self.settle_input();
        if let Some(first) = remove_first(&mut self.right) {
            self.left.push(first);
            self.input.clone_from(&self.left);
        }
        self.show();
    }

    fn move_caret_home(&mut self) {
        self.settle_input();
        self.right.insert_str(0, &self.left);
        self.left.clear();
        self.input.clear();
        self.show();
    }

    fn move_caret_end(&mut self) {
        self.settle_input();
        self.left.push_str(&self.right);
        self.right.clear();
        self.input.clone_from(&self.left);
        self.show();
    }

    /// Converts the reading and opens alternatives on the first Space,
    /// reusing what the background lookup already converted.
    fn convert(&mut self, converter: &mut impl Converter) -> Result<(), WorkerError> {
        self.selecting_suggestion = false;
        let reading = self.conversion_reading();
        let looked_up = self
            .local
            .as_ref()
            .filter(|local| local.reading == reading && !local.conversion.clauses.is_empty())
            .map(|local| local.conversion.clone());
        let conversion = match looked_up {
            Some(conversion) => conversion,
            None => converter.convert_sentence(&reading)?,
        };
        self.left.clone_from(&reading);
        self.input = reading;
        self.right.clear();
        if !conversion.clauses.is_empty() {
            self.conversion = Some(Conversion {
                clauses: conversion
                    .clauses
                    .into_iter()
                    .map(|clause| Clause {
                        reading: clause.reading,
                        text: clause.text,
                        chosen: None,
                    })
                    .collect(),
                focus: 0,
                candidates: CandidateList::default(),
                cloud_sentence: None,
                sentence: conversion.candidates.into_iter().next(),
            });
        }
        self.open_candidates(converter)?;
        self.show();
        Ok(())
    }

    fn leave_conversion(&mut self) {
        self.conversion = None;
        self.show();
    }

    fn candidate_window_open(&self) -> bool {
        self.conversion
            .as_ref()
            .is_some_and(|conversion| !conversion.candidates.is_empty())
    }

    /// Loads the focused clause's candidates into the window if it is closed.
    fn open_candidates(&mut self, converter: &mut impl Converter) -> Result<(), WorkerError> {
        let Some(conversion) = self.conversion.as_mut() else {
            return Ok(());
        };
        if !conversion.candidates.is_empty() {
            return Ok(());
        }
        let clause = &conversion.clauses[conversion.focus];
        let reading = conversion.reading();
        let looked_up = self
            .local
            .as_ref()
            .filter(|local| conversion.focus == 0 && local.reading == reading)
            .filter(|local| {
                local
                    .conversion
                    .clauses
                    .first()
                    .is_some_and(|first| first.reading == clause.reading)
            })
            .and_then(|local| local.first_clause.clone());
        let engine = match looked_up {
            Some(engine) => engine,
            None => converter
                .clause_candidates(&clause.reading, &conversion.context(conversion.focus))?,
        };
        let cloud = self
            .cloud
            .as_ref()
            .filter(|(query, _)| *query == reading)
            .map_or(&[][..], |(_, cloud)| cloud.as_slice());
        conversion.candidates = CandidateList::new(engine, cloud, &reading, &clause.text);
        Ok(())
    }

    fn move_selection(
        &mut self,
        step: isize,
        converter: &mut impl Converter,
    ) -> Result<(), WorkerError> {
        self.open_candidates(converter)?;
        if let Some(conversion) = self.conversion.as_mut()
            && conversion.candidates.move_selection(step).is_some()
        {
            self.apply_selection();
        }
        self.show();
        Ok(())
    }

    fn move_page(
        &mut self,
        step: isize,
        converter: &mut impl Converter,
    ) -> Result<(), WorkerError> {
        self.open_candidates(converter)?;
        if let Some(conversion) = self.conversion.as_mut()
            && conversion.candidates.move_page(step).is_some()
        {
            self.apply_selection();
        }
        self.show();
        Ok(())
    }

    fn select_number(&mut self, number: usize) -> bool {
        let selected = self
            .conversion
            .as_mut()
            .is_some_and(|conversion| conversion.candidates.select_number(number).is_some());
        if selected {
            self.apply_selection();
        }
        selected
    }

    /// Puts the selected candidate into the focused clause, or shows the
    /// selected cloud sentence in place of all clauses.
    fn apply_selection(&mut self) {
        let Some(conversion) = self.conversion.as_mut() else {
            return;
        };
        let Some(choice) = conversion.candidates.selected().cloned() else {
            return;
        };
        if choice.whole_sentence {
            conversion.cloud_sentence = Some(choice.candidate.text);
        } else {
            conversion.cloud_sentence = None;
            let clause = &mut conversion.clauses[conversion.focus];
            clause.text.clone_from(&choice.candidate.text);
            clause.chosen = Some(choice.candidate);
        }
    }

    /// Moves the focus to `target(focus, last clause)` and closes the window.
    fn focus_clause(&mut self, target: impl FnOnce(usize, usize) -> usize) {
        if let Some(conversion) = self.conversion.as_mut() {
            let last = conversion.clauses.len() - 1;
            conversion.focus = target(conversion.focus, last).min(last);
            conversion.candidates.clear();
            conversion.cloud_sentence = None;
        }
        self.show();
    }

    /// Moves one reading character between the focused clause and the next
    /// one, then converts both again.
    fn resize_clause(
        &mut self,
        grow: bool,
        converter: &mut impl Converter,
    ) -> Result<(), WorkerError> {
        let Some(conversion) = self.conversion.as_mut() else {
            return Ok(());
        };
        let focus = conversion.focus;
        let moved = if grow {
            match conversion.clauses.get_mut(focus + 1) {
                Some(next) => {
                    let first = remove_first(&mut next.reading);
                    if next.reading.is_empty() {
                        conversion.clauses.remove(focus + 1);
                    }
                    first.inspect(|first| conversion.clauses[focus].reading.push(*first))
                }
                None => None,
            }
        } else if conversion.clauses[focus].reading.chars().count() > 1 {
            let last = conversion.clauses[focus].reading.pop();
            if let Some(last) = last {
                match conversion.clauses.get_mut(focus + 1) {
                    Some(next) => next.reading.insert(0, last),
                    None => conversion.clauses.push(Clause {
                        reading: last.to_string(),
                        text: String::new(),
                        chosen: None,
                    }),
                }
            }
            last
        } else {
            None
        };
        if moved.is_none() {
            return Ok(());
        }
        let end = (focus + 2).min(conversion.clauses.len());
        for index in focus..end {
            let context = conversion.context(index);
            let clause = &mut conversion.clauses[index];
            let best = converter
                .clause_candidates(&clause.reading, &context)?
                .into_iter()
                .next();
            clause.text = best.as_ref().map_or_else(
                || clause.reading.clone(),
                |candidate| candidate.text.clone(),
            );
            clause.chosen = best;
        }
        conversion.candidates.clear();
        conversion.cloud_sentence = None;
        self.show();
        Ok(())
    }

    /// What to learn on commit: every candidate the user chose, after the
    /// chosen candidate of the clause before it, or else the converted
    /// sentence. A cloud sentence is not learned.
    fn learning(&self) -> Vec<Learning> {
        if !self.is_converting() && self.selecting_suggestion {
            return self
                .suggestions
                .selected()
                .filter(|choice| !choice.whole_sentence)
                .map(|choice| Learning {
                    candidate: choice.candidate.clone(),
                    previous: None,
                })
                .into_iter()
                .collect();
        }
        let Some(conversion) = &self.conversion else {
            return Vec::new();
        };
        if conversion.cloud_sentence.is_some() {
            return Vec::new();
        }
        let chosen = conversion
            .clauses
            .iter()
            .enumerate()
            .filter_map(|(index, clause)| {
                Some(Learning {
                    candidate: clause.chosen.clone()?,
                    previous: index
                        .checked_sub(1)
                        .and_then(|previous| conversion.clauses[previous].chosen.clone()),
                })
            })
            .collect::<Vec<_>>();
        if !chosen.is_empty() {
            return chosen;
        }
        conversion
            .sentence
            .clone()
            .map(|candidate| Learning {
                candidate,
                previous: None,
            })
            .into_iter()
            .collect()
    }

    fn commit(&mut self) {
        let text = self.text();
        let learn = self.learning();
        self.reset();
        self.effects.push(Effect::Commit { text, learn });
    }

    fn cancel(&mut self) {
        self.reset();
        self.effects.push(Effect::Cancel);
    }

    fn show(&mut self) {
        let view = self.view();
        self.effects.push(Effect::Show(view));
    }

    pub fn view(&self) -> CompositionView {
        let Some(conversion) = &self.conversion else {
            return CompositionView {
                text: if self.selecting_suggestion {
                    self.text()
                } else {
                    self.reading()
                },
                converted: false,
                focus: None,
                caret: if self.selecting_suggestion {
                    utf16_length(&self.text())
                } else {
                    utf16_length(&self.left)
                },
                candidates: self.suggestions.view().map(|mut view| {
                    view.selecting = self.selecting_suggestion;
                    view
                }),
            };
        };
        let text = self.text();
        let focus = conversion.cloud_sentence.is_none().then(|| {
            let start = utf16_length(&conversion.context(conversion.focus));
            start..start + utf16_length(&conversion.clauses[conversion.focus].text)
        });
        CompositionView {
            caret: utf16_length(&text),
            text,
            converted: true,
            focus,
            candidates: conversion.candidates.view(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use simeji_protocol::Clause as SentenceClause;

    fn sentence(texts: &[&str], reading: &str) -> SentenceConversion {
        SentenceConversion {
            candidates: texts.iter().map(|text| candidate(text, reading)).collect(),
            clauses: vec![SentenceClause {
                text: texts[0].into(),
                reading: reading.into(),
            }],
        }
    }

    fn local(texts: &[&str], reading: &str) -> Lookup {
        Lookup {
            conversion: Some(sentence(texts, reading)),
            ..Lookup::default()
        }
    }

    fn cloud_sentence(text: &str, reading: &str) -> CloudCandidate {
        CloudCandidate {
            text: text.into(),
            reading: reading.into(),
            property: 0,
            kind: 0,
            scene: String::new(),
        }
    }

    fn cloud(texts: &[&str], reading: &str) -> Lookup {
        Lookup {
            cloud: Some(
                texts
                    .iter()
                    .map(|text| cloud_sentence(text, reading))
                    .collect(),
            ),
            ..Lookup::default()
        }
    }

    fn candidates(composer: &mut Composer) -> Option<CandidateView> {
        match composer.take_effects().as_slice() {
            [Effect::Candidates(view)] => view.clone(),
            effects => panic!("expected new candidates, got {effects:?}"),
        }
    }

    #[test]
    fn typing_looks_up_complete_readings_and_keeps_the_reading_until_tab() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyou");
        assert_eq!(
            composer.take_effects(),
            [
                Effect::Show(typing("k", 1)),
                Effect::Show(typing("ky", 2)),
                Effect::Show(typing("kyo", 3)),
                Effect::Lookup("kyo".into()),
                Effect::Show(typing("kyou", 4)),
                Effect::Lookup("kyou".into()),
            ]
        );
        composer.receive_lookup("kyou", local(&["今日", "京"], "kyou"));
        let view = candidates(&mut composer).unwrap();
        assert_eq!(view.texts, ["今日", "京"]);
        assert!(!view.selecting);
        assert_eq!(composer.view().text, "kyou");
        press(&mut composer, &mut converter, Key::Tab { shift: false });
        assert_eq!(shown(&mut composer).text, "今日");
        press(&mut composer, &mut converter, DOWN);
        assert_eq!(shown(&mut composer).text, "京");
        press(&mut composer, &mut converter, Key::Enter);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "京".into(),
                learn: vec![learning(candidate("京", "kyou"), None)]
            }]
        );
        assert!(composer.view().candidates.is_none());
        assert!(converter.converted.is_empty());
    }

    #[test]
    fn earlier_suggestions_stay_on_screen_until_new_ones_arrive_but_cannot_be_chosen() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyou");
        composer.receive_lookup("kyou", local(&["今日"], "kyou"));
        composer.take_effects();
        type_text(&mut composer, &mut converter, "ha");
        let effects = composer.take_effects();
        assert_eq!(effects.len(), 3);
        assert_eq!(effects[2], Effect::Lookup("kyouha".into()));
        let Effect::Show(view) = &effects[1] else {
            panic!("expected the reading, got {effects:?}");
        };
        assert_eq!(view.text, "kyouha");
        assert_eq!(view.candidates.as_ref().unwrap().texts, ["今日"]);
        composer.receive_lookup("kyou", local(&["今日"], "kyou"));
        composer.act(CandidateAction::Select(0));
        assert!(composer.take_effects().is_empty());
        // Tab converts the reading itself instead of choosing the old list.
        press(&mut composer, &mut converter, Key::Tab { shift: false });
        let view = shown(&mut composer);
        assert_eq!(view.text, "今日は");
        assert_eq!(view.candidates, page(&["今日は"], 0));
        assert_eq!(converter.converted, ["kyouha"]);
        // The result that arrives afterwards changes nothing.
        composer.receive_lookup("kyouha", local(&["今日は"], "kyouha"));
        assert!(composer.take_effects().is_empty());
    }

    #[test]
    fn late_cloud_results_append_without_changing_the_selection_or_committing_the_cloud_marker() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        composer.set_cloud_enabled(true);
        type_text(&mut composer, &mut converter, "kyou");
        composer.receive_lookup("kyou", local(&["今日", "京"], "kyou"));
        press(&mut composer, &mut converter, Key::Tab { shift: false });
        press(&mut composer, &mut converter, DOWN);
        composer.take_effects();
        composer.receive_lookup("kyou", cloud(&["今日は晴れ"], "kyou"));
        let view = candidates(&mut composer).unwrap();
        assert_eq!(view.texts, ["今日", "京", "今日は晴れ"]);
        assert_eq!(view.cloud, [false, false, true]);
        assert_eq!(view.selected, 1);
        assert_eq!(composer.view().text, "京");
        composer.receive_lookup("stale", cloud(&["古い"], "stale"));
        assert!(composer.take_effects().is_empty());
        composer.act(CandidateAction::Select(2));
        composer.take_effects();
        composer.act(CandidateAction::Commit);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "今日は晴れ".into(),
                learn: vec![]
            }]
        );
    }

    #[test]
    fn lookups_are_polled_until_the_conversion_its_first_clause_and_the_cloud_arrive() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        composer.set_cloud_enabled(true);
        assert_eq!(composer.pending_lookup(), None);
        type_text(&mut composer, &mut converter, "kyouha");
        assert_eq!(composer.pending_lookup().as_deref(), Some("kyouha"));
        composer.receive_lookup("kyouha", cloud(&["今日は晴れ"], "kyouha"));
        composer.receive_lookup("kyouha", local(&["今日は"], "kyouha"));
        assert_eq!(composer.pending_lookup().as_deref(), Some("kyouha"));
        let view = composer.view().candidates.unwrap();
        assert_eq!(view.texts, ["今日は", "今日は晴れ"]);
        composer.receive_lookup(
            "kyouha",
            Lookup {
                first_clause: Some(Vec::new()),
                ..local(&["今日は"], "kyouha")
            },
        );
        assert_eq!(composer.pending_lookup(), None);
        type_text(&mut composer, &mut converter, "h");
        assert_eq!(composer.pending_lookup(), None);
    }

    #[test]
    fn space_reuses_the_conversion_and_first_clause_found_while_typing() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyouha");
        composer.receive_lookup(
            "kyouha",
            Lookup {
                conversion: Some(SentenceConversion {
                    candidates: vec![candidate("今日は", "kyouha")],
                    clauses: vec![
                        SentenceClause {
                            text: "今日".into(),
                            reading: "kyou".into(),
                        },
                        SentenceClause {
                            text: "は".into(),
                            reading: "ha".into(),
                        },
                    ],
                }),
                first_clause: Some(vec![candidate("今日", "kyou"), candidate("共", "kyou")]),
                cloud: None,
            },
        );
        composer.take_effects();
        press(&mut composer, &mut converter, SPACE);
        let view = shown(&mut composer);
        assert_eq!(view.text, "今日は");
        assert_eq!(view.candidates, page(&["今日", "共"], 0));
        assert!(converter.converted.is_empty());
        assert!(converter.candidate_requests.is_empty());
    }

    #[test]
    fn cloud_after_clause_navigation_does_not_prevent_loading_local_alternatives() {
        let (mut composer, mut converter) = converted();
        composer.set_cloud_enabled(true);
        press(&mut composer, &mut converter, Key::Right { shift: false });
        composer.take_effects();
        composer.receive_lookup("kyouha", cloud(&["今日は晴れ"], "kyouha"));
        assert!(composer.take_effects().is_empty());
        press(&mut composer, &mut converter, DOWN);
        let candidates = shown(&mut composer).candidates.unwrap();
        assert!(candidates.texts.iter().any(|text| text == "は"));
        assert!(candidates.texts.iter().any(|text| text == "今日は晴れ"));
        assert_eq!(
            converter.candidate_requests.last(),
            Some(&("ha".into(), "今日".into()))
        );
    }

    #[test]
    fn editing_or_cancelling_predictions_discards_stale_candidates() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyou");
        composer.receive_lookup("kyou", local(&["今日"], "kyou"));
        press(&mut composer, &mut converter, DOWN);
        composer.take_effects();
        press(&mut composer, &mut converter, Key::Escape);
        let view = shown(&mut composer);
        assert_eq!(view.text, "kyou");
        assert!(!view.candidates.unwrap().selecting);
        press(&mut composer, &mut converter, Key::Backspace);
        assert_eq!(composer.take_effects()[1], Effect::Lookup("kyo".into()));
        composer.receive_lookup("kyou", local(&["stale"], "kyou"));
        assert!(composer.take_effects().is_empty());
        press(&mut composer, &mut converter, Key::Escape);
        assert!(!composer.is_composing());
        assert!(composer.view().candidates.is_none());
    }

    #[test]
    fn tab_converts_the_reading_when_no_suggestions_arrived_yet() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyouha");
        composer.take_effects();
        press(&mut composer, &mut converter, Key::Tab { shift: false });
        assert_eq!(shown(&mut composer).text, "今日は");
        press(&mut composer, &mut converter, UP);
        assert_eq!(shown(&mut composer).text, "今日は");
        assert_eq!(converter.converted, ["kyouha"]);
    }

    /// Returns romaji unchanged as the reading and the same two clauses for
    /// every reading.
    struct Fake {
        alternatives: Vec<(&'static str, Vec<&'static str>)>,
        candidate_requests: Vec<(String, String)>,
        converted: Vec<String>,
        fail: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                alternatives: vec![
                    ("kyou", vec!["今日", "京", "強"]),
                    ("ha", vec!["は", "葉"]),
                    ("kyo", vec!["居"]),
                    ("uha", vec!["右派"]),
                    ("kyouh", vec!["今日h"]),
                    ("a", vec!["亜"]),
                ],
                candidate_requests: Vec::new(),
                converted: Vec::new(),
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
        fn convert_sentence(&mut self, reading: &str) -> Result<SentenceConversion, WorkerError> {
            self.converted.push(reading.into());
            let clauses = [("kyou", "今日"), ("ha", "は")];
            Ok(SentenceConversion {
                candidates: vec![candidate("今日は", reading)],
                clauses: clauses
                    .iter()
                    .map(|(reading, text)| SentenceClause {
                        text: (*text).into(),
                        reading: (*reading).into(),
                    })
                    .collect(),
            })
        }
        fn clause_candidates(
            &mut self,
            reading: &str,
            context: &str,
        ) -> Result<Vec<Candidate>, WorkerError> {
            self.candidate_requests
                .push((reading.into(), context.into()));
            Ok(self
                .alternatives
                .iter()
                .find(|(clause, _)| *clause == reading)
                .map(|(_, texts)| texts.iter().map(|text| candidate(text, reading)).collect())
                .unwrap_or_default())
        }
    }

    const SPACE: Key = Key::Space { shift: false };
    const LEFT: Key = Key::Left { shift: false };
    const RIGHT: Key = Key::Right { shift: false };
    const UP: Key = Key::Up { shift: false };
    const DOWN: Key = Key::Down { shift: false };
    const PRINTABLE: Key = Key::Printable {
        candidate_number: None,
    };

    fn press(composer: &mut Composer, converter: &mut Fake, key: Key) -> bool {
        composer.press(key, || None, converter).unwrap()
    }

    fn type_text(composer: &mut Composer, converter: &mut Fake, text: &str) {
        for character in text.chars() {
            let typed = || Some(character.to_string());
            assert!(composer.press(PRINTABLE, typed, converter).unwrap());
        }
    }

    fn typing(text: &str, caret: usize) -> CompositionView {
        CompositionView {
            text: text.into(),
            converted: false,
            focus: None,
            caret,
            candidates: None,
        }
    }

    fn page(items: &[&str], selected: usize) -> Option<CandidateView> {
        Some(CandidateView {
            texts: items.iter().map(|item| (*item).into()).collect(),
            selected,
            cloud: vec![false; items.len()],
            selecting: true,
        })
    }

    fn learning(candidate: Candidate, previous: Option<Candidate>) -> Learning {
        Learning {
            candidate,
            previous,
        }
    }

    /// Types "kyouha" and converts it to 今日|は with 今日 focused.
    fn converted() -> (Composer, Fake) {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyouha");
        assert!(press(&mut composer, &mut converter, SPACE));
        composer.take_effects();
        (composer, converter)
    }

    fn shown(composer: &mut Composer) -> CompositionView {
        match composer.take_effects().as_slice() {
            [Effect::Show(view)] => view.clone(),
            effects => panic!("expected one view, got {effects:?}"),
        }
    }

    #[test]
    fn typing_shows_the_reading_and_starts_lookups() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        assert!(!composer.is_composing());
        type_text(&mut composer, &mut converter, "ka");
        assert_eq!(
            composer.take_effects(),
            [
                Effect::Show(typing("k", 1)),
                Effect::Show(typing("ka", 2)),
                Effect::Lookup("ka".into()),
            ]
        );
    }

    #[test]
    fn backspace_deletes_one_shown_character_without_the_worker() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "ab");
        composer.take_effects();
        converter.fail = true;
        press(&mut composer, &mut converter, Key::Backspace);
        // "a" was looked up while typing it.
        assert_eq!(composer.take_effects(), [Effect::Show(typing("a", 1))]);
        press(&mut composer, &mut converter, Key::Backspace);
        assert_eq!(composer.take_effects(), [Effect::Cancel]);
        assert!(!composer.is_composing());
    }

    #[test]
    fn the_caret_moves_inside_the_reading_and_text_is_typed_there() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "abc");
        composer.take_effects();
        press(&mut composer, &mut converter, LEFT);
        assert_eq!(shown(&mut composer), typing("abc", 2));
        type_text(&mut composer, &mut converter, "x");
        assert_eq!(composer.take_effects()[0], Effect::Show(typing("abxc", 3)));
        press(&mut composer, &mut converter, Key::Home);
        assert_eq!(shown(&mut composer).caret, 0);
        press(&mut composer, &mut converter, Key::Delete);
        assert_eq!(composer.take_effects()[0], Effect::Show(typing("bxc", 0)));
        // Backspace at the start has nothing to delete.
        press(&mut composer, &mut converter, Key::Backspace);
        assert!(composer.take_effects().is_empty());
        press(&mut composer, &mut converter, RIGHT);
        assert_eq!(shown(&mut composer).caret, 1);
        press(&mut composer, &mut converter, Key::End);
        assert_eq!(shown(&mut composer), typing("bxc", 3));
    }

    #[test]
    fn a_pending_n_becomes_kana_when_the_input_ends() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kan");
        composer.take_effects();
        press(&mut composer, &mut converter, LEFT);
        assert_eq!(composer.take_effects(), [Effect::Show(typing("kaん", 2))]);
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "nihon");
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(converter.converted, ["nihoん"]);
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kan");
        composer.take_effects();
        press(&mut composer, &mut converter, Key::Enter);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "kaん".into(),
                learn: Vec::new(),
            }]
        );
    }

    #[test]
    fn the_first_space_converts_and_opens_the_candidate_window() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyouha");
        composer.take_effects();
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(
            shown(&mut composer),
            CompositionView {
                text: "今日は".into(),
                converted: true,
                focus: Some(0..2),
                caret: 3,
                candidates: page(&["今日", "京", "強"], 0),
            }
        );
        assert_eq!(
            converter.candidate_requests,
            [("kyou".into(), String::new())]
        );
        press(&mut composer, &mut converter, Key::Enter);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "今日は".into(),
                learn: vec![learning(candidate("今日は", "kyouha"), None)],
            }]
        );
    }

    #[test]
    fn the_second_space_moves_to_the_next_candidate() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, SPACE);
        let view = shown(&mut composer);
        assert_eq!(view.text, "京は");
        assert_eq!(view.candidates, page(&["今日", "京", "強"], 1));
        assert_eq!(
            converter.candidate_requests,
            [("kyou".into(), String::new())]
        );
        press(&mut composer, &mut converter, Key::Space { shift: true });
        assert_eq!(shown(&mut composer).text, "今日は");
        press(&mut composer, &mut converter, UP);
        assert_eq!(shown(&mut composer).text, "強は");
        press(&mut composer, &mut converter, DOWN);
        assert_eq!(shown(&mut composer).text, "今日は");
        assert_eq!(converter.candidate_requests.len(), 1);
    }

    #[test]
    fn up_opens_the_window_on_the_previous_candidate_and_shift_pages() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, UP);
        assert_eq!(shown(&mut composer).text, "強は");
        press(&mut composer, &mut converter, Key::Down { shift: true });
        assert_eq!(shown(&mut composer).text, "強は");
    }

    #[test]
    fn moving_between_clauses_closes_the_window() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, SPACE);
        composer.take_effects();
        press(&mut composer, &mut converter, RIGHT);
        let view = shown(&mut composer);
        assert_eq!((view.focus, view.candidates), (Some(1..2), None));
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(shown(&mut composer).text, "京葉");
        assert_eq!(converter.candidate_requests[1], ("ha".into(), "京".into()));
        press(&mut composer, &mut converter, Key::Home);
        assert_eq!(shown(&mut composer).focus, Some(0..1));
        press(&mut composer, &mut converter, Key::End);
        assert_eq!(shown(&mut composer).focus, Some(1..2));
        press(&mut composer, &mut converter, RIGHT);
        assert_eq!(shown(&mut composer).focus, Some(1..2));
    }

    #[test]
    fn shift_left_and_right_resize_the_focused_clause() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, Key::Left { shift: true });
        let view = shown(&mut composer);
        assert_eq!((view.text.as_str(), view.focus), ("居右派", Some(0..1)));
        assert_eq!(
            converter.candidate_requests,
            [
                ("kyou".into(), String::new()),
                ("kyo".into(), String::new()),
                ("uha".into(), "居".into())
            ]
        );
        press(&mut composer, &mut converter, Key::Right { shift: true });
        assert_eq!(shown(&mut composer).text, "今日は");
        press(&mut composer, &mut converter, Key::Right { shift: true });
        assert_eq!(shown(&mut composer).text, "今日h亜");
        press(&mut composer, &mut converter, Key::Enter);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "今日h亜".into(),
                learn: vec![
                    learning(candidate("今日h", "kyouh"), None),
                    learning(candidate("亜", "a"), Some(candidate("今日h", "kyouh"))),
                ],
            }]
        );
    }

    #[test]
    fn number_keys_commit_the_candidate_with_that_number_in_the_window() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, SPACE);
        composer.take_effects();
        let missing = Key::Printable {
            candidate_number: Some(5),
        };
        assert!(press(&mut composer, &mut converter, missing));
        assert!(composer.take_effects().is_empty());
        let third = Key::Printable {
            candidate_number: Some(3),
        };
        assert!(press(&mut composer, &mut converter, third));
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "強は".into(),
                learn: vec![learning(candidate("強", "kyou"), None)],
            }]
        );
    }

    #[test]
    fn number_keys_type_digits_while_the_window_is_closed() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "a");
        composer.take_effects();
        let key = Key::Printable {
            candidate_number: Some(1),
        };
        assert!(
            composer
                .press(key, || Some("1".into()), &mut converter)
                .unwrap()
        );
        let effects = composer.take_effects();
        assert_eq!(effects[0], Effect::Show(typing("a1", 2)));
    }

    #[test]
    fn escape_backspace_and_delete_return_to_the_reading() {
        for key in [Key::Escape, Key::Backspace, Key::Delete] {
            let (mut composer, mut converter) = converted();
            press(&mut composer, &mut converter, key);
            assert_eq!(shown(&mut composer), typing("kyouha", 6));
        }
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "a");
        composer.take_effects();
        press(&mut composer, &mut converter, Key::Escape);
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
                    learn: vec![learning(candidate("今日は", "kyouha"), None)],
                },
                Effect::Show(typing("a", 1)),
                Effect::Lookup("a".into()),
            ]
        );
    }

    #[test]
    fn every_chosen_clause_is_learned_after_the_one_before_it() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, SPACE);
        press(&mut composer, &mut converter, RIGHT);
        press(&mut composer, &mut converter, SPACE);
        composer.take_effects();
        press(&mut composer, &mut converter, Key::Enter);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "京葉".into(),
                learn: vec![
                    learning(candidate("京", "kyou"), None),
                    learning(candidate("葉", "ha"), Some(candidate("京", "kyou"))),
                ],
            }]
        );
    }

    #[test]
    fn keys_that_type_nothing_go_to_the_application() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        assert!(!composer.accepts(LEFT));
        assert!(!press(&mut composer, &mut converter, PRINTABLE));
        let (mut composer, mut converter) = converted();
        assert!(!press(&mut composer, &mut converter, PRINTABLE));
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
        assert!(press(&mut composer, &mut converter, Key::ToggleInputMode));
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "a".into(),
                learn: Vec::new(),
            }]
        );
        assert!(!composer.accepts(PRINTABLE));
        assert!(!press(&mut composer, &mut converter, PRINTABLE));
        assert!(press(&mut composer, &mut converter, Key::ToggleInputMode));
        assert!(composer.accepts(PRINTABLE));
        assert!(composer.take_effects().is_empty());
    }

    #[test]
    fn switching_the_mode_from_outside_commits_the_composition() {
        let (mut composer, mut converter) = converted();
        composer.set_mode(InputMode::Japanese);
        assert!(composer.take_effects().is_empty());
        composer.set_mode(InputMode::Direct);
        assert!(matches!(
            composer.take_effects().as_slice(),
            [Effect::Commit { .. }]
        ));
        assert_eq!(composer.mode(), InputMode::Direct);
        assert!(!composer.accepts(PRINTABLE));
        assert!(!press(&mut composer, &mut converter, PRINTABLE));
    }

    #[test]
    fn long_input_is_committed_before_it_grows_further() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, &"a".repeat(MAX_INPUT_BYTES));
        composer.take_effects();
        type_text(&mut composer, &mut converter, "b");
        assert_eq!(
            composer.take_effects()[..2],
            [
                Effect::Commit {
                    text: "a".repeat(MAX_INPUT_BYTES),
                    learn: Vec::new(),
                },
                Effect::Show(typing("b", 1)),
            ]
        );
    }

    #[test]
    fn a_worker_failure_keeps_the_commit_that_preceded_it() {
        let (mut composer, mut converter) = converted();
        converter.fail = true;
        let result = composer.press(PRINTABLE, || Some("a".into()), &mut converter);
        assert!(result.is_err());
        assert!(matches!(
            composer.take_effects().as_slice(),
            [Effect::Commit { .. }]
        ));
    }

    #[test]
    fn cloud_sentences_join_the_candidates_and_are_not_learned() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        composer.set_cloud_enabled(true);
        type_text(&mut composer, &mut converter, "kyouha");
        composer.receive_lookup("kyou", cloud(&["今日は晴れ"], "kyouha"));
        composer.receive_lookup("kyouha", cloud(&["今日は晴れ"], "kyouha"));
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(composer.pending_lookup(), None);
        composer.take_effects();
        press(&mut composer, &mut converter, SPACE);
        let view = shown(&mut composer);
        assert_eq!((view.text.as_str(), view.focus), ("今日は晴れ", None));
        assert_eq!(
            view.candidates.unwrap().texts,
            ["今日", "今日は晴れ", "京", "強"]
        );
        press(&mut composer, &mut converter, Key::Enter);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "今日は晴れ".into(),
                learn: Vec::new(),
            }]
        );
    }

    #[test]
    fn cloud_polling_continues_after_conversion_until_results_arrive() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        composer.set_cloud_enabled(true);
        assert_eq!(composer.pending_lookup(), None);
        type_text(&mut composer, &mut converter, "kyouha");
        press(&mut composer, &mut converter, SPACE);
        assert_eq!(composer.pending_lookup().as_deref(), Some("kyouha"));
    }

    #[test]
    fn applications_with_their_own_candidate_ui_select_commit_and_cancel() {
        let (mut composer, mut converter) = converted();
        press(&mut composer, &mut converter, SPACE);
        composer.take_effects();
        composer.act(CandidateAction::Select(2));
        assert_eq!(shown(&mut composer).text, "強は");
        composer.act(CandidateAction::Select(9));
        assert!(composer.take_effects().is_empty());
        composer.act(CandidateAction::Cancel);
        assert_eq!(shown(&mut composer), typing("kyouha", 6));
        composer.act(CandidateAction::Cancel);
        assert!(composer.take_effects().is_empty());
        composer.act(CandidateAction::Commit);
        assert_eq!(
            composer.take_effects(),
            [Effect::Commit {
                text: "kyouha".into(),
                learn: Vec::new(),
            }]
        );
        composer.act(CandidateAction::Commit);
        assert!(composer.take_effects().is_empty());
    }

    #[test]
    fn touch_keyboard_page_keys_convert_and_turn_pages() {
        let (mut composer, mut converter) = (Composer::new(), Fake::new());
        type_text(&mut composer, &mut converter, "kyouha");
        composer.take_effects();
        press(&mut composer, &mut converter, Key::NextCandidatePage);
        assert_eq!(shown(&mut composer).text, "今日は");
        press(&mut composer, &mut converter, Key::NextCandidatePage);
        assert!(shown(&mut composer).candidates.is_some());
    }

    #[test]
    fn a_snapshot_restores_the_composition() {
        let (composer, mut converter) = converted();
        let mut changed = composer.clone();
        press(&mut changed, &mut converter, Key::Escape);
        assert_eq!(changed.text(), "kyouha");
        assert_eq!(composer.text(), "今日は");
    }
}
