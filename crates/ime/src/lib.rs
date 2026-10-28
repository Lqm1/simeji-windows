//! Simeji Windows text input processor: a TSF in-process COM server.
//!
//! Windows loads this DLL into every application that receives text, so it
//! keeps only the state of the current composition and leaves conversion to
//! the worker process (`simeji-worker`), reached over a local pipe.
//!
//! The crate has two layers:
//!
//! - Input logic without TSF: `keys` maps virtual keys, `composer` is the
//!   composition state machine and `candidates` builds the candidate list.
//!   `worker` is their client for the conversion worker.
//! - Windows integration: `dll` holds the DLL entry points and class
//!   factory, `text_service` the TSF text input processor that applies the
//!   composer's effects through `edit_session`, plus `display_attributes`,
//!   the `candidate_window` and the `candidate_element` for applications
//!   that draw candidates themselves, `functions` for the touch keyboard
//!   and search, `input_scope` checks, the `cloud_timer` and
//!   [`registration`] for `simeji-setup`.

mod candidate_accessibility;
mod candidate_element;
mod candidate_window;
mod candidates;
mod cloud_timer;
mod composer;
mod display_attributes;
mod dll;
mod edit_session;
mod functions;
mod input_mode;
mod input_scope;
mod keys;
mod mode_icon;
pub mod registration;
mod text_service;
mod worker;

pub use dll::{
    DllCanUnloadNow, DllGetClassObject, DllMain, DllRegisterServer, DllUnregisterServer,
};

use windows::core::GUID;

pub const CLSID_TEXT_SERVICE: GUID = GUID::from_u128(0xeac9f5f0_91b4_4f85_93d8_7a1669e5d7bd);
/// The Japanese keyboard profile of the text service.
pub const GUID_PROFILE: GUID = GUID::from_u128(0x549ae946_7705_4af7_a1bd_37936d432991);
/// Name shown in the Windows input selector.
pub const DISPLAY_NAME: &str = "Simeji Windows";
pub const LANGID_JAPANESE: u16 = 0x0411;
