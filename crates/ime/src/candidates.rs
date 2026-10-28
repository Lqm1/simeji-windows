//! The candidate list for the focused clause, merged with cloud sentences.

use simeji_protocol::{Candidate, CloudCandidate};

/// Candidates per page of the candidate window, numbered 1–9.
pub(crate) const PAGE_SIZE: usize = 9;
/// Cloud sentences placed right after the engine's first candidate; any
/// further ones follow the engine candidates.
const LEADING_CLOUD_SENTENCES: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    pub candidate: Candidate,
    /// A cloud result that replaces the whole sentence instead of one clause.
    pub whole_sentence: bool,
}

/// All candidates and the selected one, shown a page at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateView {
    pub texts: Vec<String>,
    pub selected: usize,
}

impl CandidateView {
    pub fn page_start(&self) -> usize {
        self.selected / PAGE_SIZE * PAGE_SIZE
    }

    pub fn page(&self) -> &[String] {
        let start = self.page_start();
        &self.texts[start..(start + PAGE_SIZE).min(self.texts.len())]
    }

    pub fn selected_on_page(&self) -> usize {
        self.selected - self.page_start()
    }

    pub fn page_count(&self) -> usize {
        self.texts.len().div_ceil(PAGE_SIZE)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CandidateList {
    choices: Vec<Choice>,
    selected: usize,
}

impl CandidateList {
    /// Builds the list from the engine's candidates for a clause and the
    /// cloud sentences for the whole `reading`, selecting the clause's
    /// `current` text.
    pub fn new(
        engine: Vec<Candidate>,
        cloud: &[CloudCandidate],
        reading: &str,
        current: &str,
    ) -> Self {
        let mut choices = engine
            .into_iter()
            .map(|candidate| Choice {
                candidate,
                whole_sentence: false,
            })
            .collect::<Vec<_>>();
        let mut sentences = Vec::new();
        for word in cloud.iter().filter(|word| word.scene.is_empty()) {
            if choices
                .iter()
                .chain(&sentences)
                .any(|choice: &Choice| choice.candidate.text == word.text)
            {
                continue;
            }
            sentences.push(Choice {
                candidate: Candidate {
                    text: word.text.clone(),
                    reading: reading.to_owned(),
                    property: word.property,
                    left_id: 0,
                    right_id: 0,
                },
                whole_sentence: true,
            });
        }
        let later = sentences.split_off(sentences.len().min(LEADING_CLOUD_SENTENCES));
        let position = choices.len().min(1);
        choices.splice(position..position, sentences);
        choices.extend(later);
        let selected = choices
            .iter()
            .position(|choice| !choice.whole_sentence && choice.candidate.text == current)
            .unwrap_or(0);
        Self { choices, selected }
    }

    pub fn is_empty(&self) -> bool {
        self.choices.is_empty()
    }

    pub fn clear(&mut self) {
        self.choices.clear();
        self.selected = 0;
    }

    pub fn selected(&self) -> Option<&Choice> {
        self.choices.get(self.selected)
    }

    /// Moves the selection by `step`, wrapping around at both ends.
    pub fn move_selection(&mut self, step: isize) -> Option<&Choice> {
        let count = self.choices.len() as isize;
        if count == 0 {
            return None;
        }
        self.selected = (self.selected as isize + step).rem_euclid(count) as usize;
        self.selected()
    }

    /// Moves by `step` pages, wrapping around, at the same place on the page.
    pub fn move_page(&mut self, step: isize) -> Option<&Choice> {
        let count = self.choices.len();
        if count == 0 {
            return None;
        }
        let pages = count.div_ceil(PAGE_SIZE) as isize;
        let page = (self.page_start() / PAGE_SIZE) as isize;
        let target = (page + step).rem_euclid(pages) as usize * PAGE_SIZE;
        self.selected = (target + self.selected % PAGE_SIZE).min(count - 1);
        self.selected()
    }

    /// Selects the candidate shown with `number` (1-based) on the current page.
    pub fn select_number(&mut self, number: usize) -> Option<&Choice> {
        let index = self.page_start() + number.checked_sub(1)?;
        if index >= self.choices.len() {
            return None;
        }
        self.selected = index;
        self.selected()
    }

    /// Selects the candidate at `index` among all candidates.
    pub fn select(&mut self, index: usize) -> Option<&Choice> {
        if index >= self.choices.len() {
            return None;
        }
        self.selected = index;
        self.selected()
    }

    pub fn view(&self) -> Option<CandidateView> {
        (!self.choices.is_empty()).then(|| CandidateView {
            texts: self
                .choices
                .iter()
                .map(|choice| choice.candidate.text.clone())
                .collect(),
            selected: self.selected,
        })
    }

    fn page_start(&self) -> usize {
        self.selected / PAGE_SIZE * PAGE_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(text: &str) -> Candidate {
        Candidate {
            text: text.into(),
            reading: "clause".into(),
            property: 0,
            left_id: 1,
            right_id: 2,
        }
    }

    fn cloud(text: &str, scene: &str) -> CloudCandidate {
        CloudCandidate {
            text: text.into(),
            reading: "sentence".into(),
            property: 7,
            kind: 0,
            scene: scene.into(),
        }
    }

    fn texts(list: &CandidateList) -> Vec<(&str, bool)> {
        list.choices
            .iter()
            .map(|choice| (choice.candidate.text.as_str(), choice.whole_sentence))
            .collect()
    }

    #[test]
    fn places_cloud_sentences_after_the_first_engine_candidate() {
        let engine = ["a", "b", "c"].map(candidate).to_vec();
        let cloud_candidates =
            ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"].map(|t| cloud(t, ""));
        let list = CandidateList::new(engine, &cloud_candidates, "reading", "b");
        assert_eq!(
            texts(&list),
            [
                ("a", false),
                ("s1", true),
                ("s2", true),
                ("s3", true),
                ("s4", true),
                ("s5", true),
                ("s6", true),
                ("b", false),
                ("c", false),
                ("s7", true),
                ("s8", true),
            ]
        );
        assert_eq!(list.selected().unwrap().candidate.text, "b");
        let sentence = &list.choices[1].candidate;
        assert_eq!(
            (
                sentence.reading.as_str(),
                sentence.property,
                sentence.left_id
            ),
            ("reading", 7, 0)
        );
    }

    #[test]
    fn skips_duplicate_and_scene_specific_cloud_candidates() {
        let engine = ["a", "b"].map(candidate).to_vec();
        let cloud_candidates = [
            cloud("a", ""),
            cloud("x", "chat"),
            cloud("y", ""),
            cloud("y", ""),
        ];
        let list = CandidateList::new(engine, &cloud_candidates, "reading", "missing");
        assert_eq!(texts(&list), [("a", false), ("y", true), ("b", false)]);
        // Without a matching clause text the first candidate is selected.
        assert_eq!(list.selected, 0);
    }

    #[test]
    fn steps_and_pages_wrap_and_numbers_pick_on_the_current_page() {
        let engine = (0..20).map(|n| candidate(&n.to_string())).collect();
        let mut list = CandidateList::new(engine, &[], "reading", "0");
        assert_eq!(list.move_selection(-1).unwrap().candidate.text, "19");
        let view = list.view().unwrap();
        assert_eq!(view.page(), ["18", "19"]);
        assert_eq!((view.selected, view.selected_on_page()), (19, 1));
        assert_eq!(view.page_count(), 3);
        assert_eq!(list.select_number(1).unwrap().candidate.text, "18");
        assert!(list.select_number(3).is_none());
        assert_eq!(list.move_selection(2).unwrap().candidate.text, "0");
        assert_eq!(list.select_number(9).unwrap().candidate.text, "8");
        assert_eq!(list.view().unwrap().page().len(), PAGE_SIZE);
        assert_eq!(list.move_page(1).unwrap().candidate.text, "17");
        assert_eq!(list.move_page(1).unwrap().candidate.text, "19");
        assert_eq!(list.move_page(1).unwrap().candidate.text, "1");
        assert_eq!(list.move_page(-1).unwrap().candidate.text, "19");
        assert_eq!(list.select(12).unwrap().candidate.text, "12");
        assert!(list.select(20).is_none());
    }

    #[test]
    fn an_empty_list_has_no_page_or_selection() {
        let mut list = CandidateList::default();
        assert!(list.view().is_none());
        assert!(list.move_selection(1).is_none());
        assert!(list.move_page(1).is_none());
        assert!(list.select_number(1).is_none());
    }
}
