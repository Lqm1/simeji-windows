//! Functions Windows asks the active text service for: the touch keyboard
//! layout to show, and conversion candidates for search without a
//! composition.

use crate::{
    CLSID_TEXT_SERVICE, DISPLAY_NAME,
    dll::DllRef,
    worker::{Converter, Worker, WorkerError},
};
use simeji_protocol::Candidate;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use windows::{
    Win32::{Foundation::*, UI::TextServices::*},
    core::*,
};

pub(crate) trait SearchConverter {
    fn search_candidates(
        &mut self,
        reading: &str,
    ) -> std::result::Result<Vec<Candidate>, WorkerError>;
    fn learn_choice(&mut self, candidate: Candidate);
}

impl SearchConverter for Worker {
    fn search_candidates(
        &mut self,
        reading: &str,
    ) -> std::result::Result<Vec<Candidate>, WorkerError> {
        let conversion = self.convert_sentence(reading)?;
        if conversion.clauses.len() == 1 {
            return self.clause_candidates(reading, "");
        }
        Ok(conversion.candidates)
    }

    fn learn_choice(&mut self, candidate: Candidate) {
        self.learn(candidate, None);
    }
}

/// Distinct conversions of the whole reading, as Microsoft asks of search
/// candidates: a prediction, or a candidate that extends another one, would
/// only narrow the results.
fn search_alternatives(reading: &str, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut distinct: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        if candidate.reading == reading && distinct.iter().all(|kept| kept.text != candidate.text) {
            distinct.push(candidate);
        }
    }
    let texts: Vec<String> = distinct.iter().map(|kept| kept.text.clone()).collect();
    distinct
        .into_iter()
        .filter(|candidate| {
            !texts
                .iter()
                .any(|other| *other != candidate.text && candidate.text.starts_with(other.as_str()))
        })
        .collect()
}

#[implement(
    ITfFunctionProvider,
    ITfFnGetPreferredTouchKeyboardLayout,
    ITfFnSearchCandidateProvider
)]
pub(crate) struct Functions {
    converter: RefCell<Box<dyn SearchConverter>>,
    /// The last query and its candidates, to learn the one the user picks.
    last_search: RefCell<Option<(String, Vec<Candidate>)>>,
    _dll: DllRef,
}

impl Functions {
    pub fn new() -> Self {
        Self::with_converter(Box::new(Worker))
    }

    fn with_converter(converter: Box<dyn SearchConverter>) -> Self {
        Self {
            converter: RefCell::new(converter),
            last_search: RefCell::new(None),
            _dll: DllRef::new(),
        }
    }
}

impl ITfFunctionProvider_Impl for Functions_Impl {
    fn GetType(&self) -> Result<GUID> {
        Ok(CLSID_TEXT_SERVICE)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(DISPLAY_NAME.into())
    }

    fn GetFunction(&self, group: *const GUID, interface: *const GUID) -> Result<IUnknown> {
        if group.is_null() || interface.is_null() || unsafe { *group } != GUID::zeroed() {
            return Err(E_NOINTERFACE.into());
        }
        let unknown: IUnknown = self.to_interface();
        let mut function = std::ptr::null_mut();
        unsafe {
            unknown.query(interface, &mut function).ok()?;
            Ok(IUnknown::from_raw(function))
        }
    }
}

impl ITfFunction_Impl for Functions_Impl {
    fn GetDisplayName(&self) -> Result<BSTR> {
        Ok(DISPLAY_NAME.into())
    }
}

impl ITfFnGetPreferredTouchKeyboardLayout_Impl for Functions_Impl {
    /// Asks for the touch-optimized Japanese keyboard, which has the
    /// conversion and candidate page keys.
    fn GetLayout(&self, layout_type: *mut TKBLayoutType, layout: *const u16) -> Result<()> {
        if layout_type.is_null() || layout.is_null() {
            return Err(E_INVALIDARG.into());
        }
        unsafe {
            *layout_type = TKBLT_OPTIMIZED;
            // An output parameter that the bindings declare as `*const`.
            *layout.cast_mut() = TKBL_OPT_JAPANESE_ABC as u16;
        }
        Ok(())
    }
}

impl ITfFnSearchCandidateProvider_Impl for Functions_Impl {
    fn GetSearchCandidates(&self, query: &BSTR, _application: &BSTR) -> Result<ITfCandidateList> {
        let query = query.to_string();
        let candidates = if query.is_empty() {
            Vec::new()
        } else {
            let candidates = self
                .converter
                .borrow_mut()
                .search_candidates(&query)
                .unwrap_or_default();
            search_alternatives(&query, candidates)
        };
        let texts = candidates
            .iter()
            .map(|candidate| candidate.text.clone())
            .collect();
        *self.last_search.borrow_mut() = Some((query, candidates));
        Ok(SearchCandidates {
            texts: Rc::new(texts),
            _dll: DllRef::new(),
        }
        .into())
    }

    fn SetResult(&self, query: &BSTR, _application: &BSTR, result: &BSTR) -> Result<()> {
        let chosen = self
            .last_search
            .borrow()
            .as_ref()
            .filter(|(last_query, _)| *query == last_query.as_str())
            .and_then(|(_, candidates)| {
                candidates
                    .iter()
                    .find(|candidate| *result == candidate.text.as_str())
                    .cloned()
            });
        if let Some(candidate) = chosen {
            self.converter.borrow_mut().learn_choice(candidate);
        }
        Ok(())
    }
}

#[implement(ITfCandidateList)]
struct SearchCandidates {
    texts: Rc<Vec<String>>,
    _dll: DllRef,
}

impl ITfCandidateList_Impl for SearchCandidates_Impl {
    fn EnumCandidates(&self) -> Result<IEnumTfCandidates> {
        Ok(CandidateEnumerator {
            texts: self.texts.clone(),
            next: Cell::new(0),
            _dll: DllRef::new(),
        }
        .into())
    }

    fn GetCandidate(&self, index: u32) -> Result<ITfCandidateString> {
        candidate_string(&self.texts, index as usize).ok_or_else(|| E_INVALIDARG.into())
    }

    fn GetCandidateNum(&self) -> Result<u32> {
        Ok(self.texts.len() as u32)
    }

    /// The choice is learned through [`ITfFnSearchCandidateProvider`].
    fn SetResult(&self, _index: u32, _result: TfCandidateResult) -> Result<()> {
        Ok(())
    }
}

fn candidate_string(texts: &[String], index: usize) -> Option<ITfCandidateString> {
    let text = texts.get(index)?.clone();
    Some(
        CandidateString {
            text,
            index: index as u32,
            _dll: DllRef::new(),
        }
        .into(),
    )
}

#[implement(ITfCandidateString)]
struct CandidateString {
    text: String,
    index: u32,
    _dll: DllRef,
}

impl ITfCandidateString_Impl for CandidateString_Impl {
    fn GetString(&self) -> Result<BSTR> {
        Ok(self.text.as_str().into())
    }

    fn GetIndex(&self) -> Result<u32> {
        Ok(self.index)
    }
}

#[implement(IEnumTfCandidates)]
struct CandidateEnumerator {
    texts: Rc<Vec<String>>,
    next: Cell<usize>,
    _dll: DllRef,
}

impl IEnumTfCandidates_Impl for CandidateEnumerator_Impl {
    fn Clone(&self) -> Result<IEnumTfCandidates> {
        Ok(CandidateEnumerator {
            texts: self.texts.clone(),
            next: Cell::new(self.next.get()),
            _dll: DllRef::new(),
        }
        .into())
    }

    fn Next(
        &self,
        count: u32,
        candidates: *mut Option<ITfCandidateString>,
        fetched: *mut u32,
    ) -> Result<()> {
        if candidates.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let start = self.next.get();
        let end = (start + count as usize).min(self.texts.len());
        for (offset, index) in (start..end).enumerate() {
            unsafe {
                candidates
                    .add(offset)
                    .write(candidate_string(&self.texts, index));
            }
        }
        self.next.set(end);
        if !fetched.is_null() {
            unsafe {
                *fetched = (end - start) as u32;
            }
        }
        if end - start == count as usize {
            Ok(())
        } else {
            Err(S_FALSE.into())
        }
    }

    fn Reset(&self) -> Result<()> {
        self.next.set(0);
        Ok(())
    }

    fn Skip(&self, count: u32) -> Result<()> {
        let start = self.next.get();
        let end = (start + count as usize).min(self.texts.len());
        self.next.set(end);
        if end - start == count as usize {
            Ok(())
        } else {
            Err(S_FALSE.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        learned: Rc<RefCell<Vec<String>>>,
    }

    impl SearchConverter for Fake {
        fn search_candidates(
            &mut self,
            reading: &str,
        ) -> std::result::Result<Vec<Candidate>, WorkerError> {
            if reading == "えらー" {
                return Err(WorkerError::Unexpected);
            }
            Ok(["今日", "京"]
                .map(|text| Candidate {
                    text: text.into(),
                    reading: reading.into(),
                    property: 0,
                    left_id: 0,
                    right_id: 0,
                })
                .to_vec())
        }

        fn learn_choice(&mut self, candidate: Candidate) {
            self.learned.borrow_mut().push(candidate.text);
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

    #[test]
    fn search_gets_distinct_conversions_without_extensions_or_predictions() {
        let candidates = [
            ("北京大学", "ぺきん"),
            ("北京", "ぺきん"),
            ("ぺきん", "ぺきん"),
            ("北京", "ぺきん"),
            ("北京市", "ぺきんし"),
            ("ペキン", "ぺきん"),
        ]
        .map(|(text, reading)| candidate(text, reading));
        let texts: Vec<_> = search_alternatives("ぺきん", candidates.to_vec())
            .into_iter()
            .map(|candidate| candidate.text)
            .collect();
        assert_eq!(texts, ["北京", "ぺきん", "ペキン"]);
    }

    fn provider() -> (ITfFunctionProvider, Rc<RefCell<Vec<String>>>) {
        let learned = Rc::new(RefCell::new(Vec::new()));
        let functions = Functions::with_converter(Box::new(Fake {
            learned: learned.clone(),
        }));
        (functions.into(), learned)
    }

    #[test]
    fn asks_for_the_touch_optimized_japanese_keyboard() -> Result<()> {
        let (provider, _) = provider();
        unsafe {
            assert_eq!(provider.GetType()?, CLSID_TEXT_SERVICE);
            let function = provider
                .GetFunction(&GUID::zeroed(), &ITfFnGetPreferredTouchKeyboardLayout::IID)?;
            let layout: ITfFnGetPreferredTouchKeyboardLayout = function.cast()?;
            let mut layout_type = TKBLayoutType::default();
            let mut layout_id = 0u16;
            layout.GetLayout(&mut layout_type, &raw mut layout_id)?;
            assert_eq!(layout_type, TKBLT_OPTIMIZED);
            assert_eq!(u32::from(layout_id), TKBL_OPT_JAPANESE_ABC);
            assert!(
                provider
                    .GetFunction(&ITfFunctionProvider::IID, &ITfFunction::IID)
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn converts_search_queries_and_learns_the_chosen_candidate() -> Result<()> {
        let (provider, learned) = provider();
        unsafe {
            let search: ITfFnSearchCandidateProvider = provider
                .GetFunction(&GUID::zeroed(), &ITfFnSearchCandidateProvider::IID)?
                .cast()?;
            let query = BSTR::from("きょう");
            let application = BSTR::new();
            let list = search.GetSearchCandidates(&query, &application)?;
            assert_eq!(list.GetCandidateNum()?, 2);
            assert_eq!(list.GetCandidate(1)?.GetString()?, "京");
            assert!(list.GetCandidate(2).is_err());

            let candidates = list.EnumCandidates()?;
            let mut fetched: [Option<ITfCandidateString>; 3] = [None, None, None];
            let mut count = 0;
            let result = (Interface::vtable(&candidates).Next)(
                candidates.as_raw(),
                3,
                fetched.as_mut_ptr().cast(),
                &mut count,
            );
            assert_eq!((result, count), (S_FALSE, 2));
            assert_eq!(fetched[0].as_ref().unwrap().GetIndex()?, 0);
            assert!(fetched[2].is_none());
            candidates.Reset()?;
            candidates.Skip(1)?;
            candidates.Next(&mut fetched[..1], &mut count)?;
            assert_eq!(fetched[0].as_ref().unwrap().GetString()?, "京");

            search.SetResult(&BSTR::from("ほか"), &application, &BSTR::from("京"))?;
            search.SetResult(&query, &application, &BSTR::from("京"))?;
            assert_eq!(*learned.borrow(), ["京"]);

            let failed = search.GetSearchCandidates(&BSTR::from("えらー"), &application)?;
            assert_eq!(failed.GetCandidateNum()?, 0);
        }
        Ok(())
    }
}
