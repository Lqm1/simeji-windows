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

/// The conversions search needs from the worker.
pub(crate) trait SearchConverter {
    /// All candidates for a reading that converts as one word, or else the
    /// converted sentence.
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
    /// A failed conversion gives no candidates rather than failing the
    /// search.
    fn GetSearchCandidates(&self, query: &BSTR, _application: &BSTR) -> Result<ITfCandidateList> {
        let query = query.to_string();
        let candidates = if query.is_empty() {
            Vec::new()
        } else {
            self.converter
                .borrow_mut()
                .search_candidates(&query)
                .unwrap_or_default()
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
