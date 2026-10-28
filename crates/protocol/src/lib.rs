//! Messages between the TSF text service and the conversion worker. Each
//! pipe connection carries one [`Request`] and its [`Response`].

use serde::{Deserialize, Serialize};

/// The worker's pipe within the user's session.
pub const WORKER_PIPE: &str = "Worker";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub text: String,
    pub reading: String,
    /// The engine's part-of-speech property bits.
    pub property: u32,
    /// Connection ID towards the preceding word.
    pub left_id: u16,
    /// Connection ID towards the following word.
    pub right_id: u16,
}

/// One clause (文節) of a converted sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    pub text: String,
    pub reading: String,
}

/// Sentence candidates for a reading; `clauses` splits the first one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Conversion {
    pub candidates: Vec<Candidate>,
    pub clauses: Vec<Clause>,
}

/// A cloud conversion candidate for a whole reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudCandidate {
    pub text: String,
    pub reading: String,
    pub property: u32,
    pub kind: u32,
    /// Non-empty for candidates meant for one scene, such as chat.
    pub scene: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Answered with [`Response::Ready`].
    Ping,
    /// Romaji to hiragana. Answered with [`Response::Reading`].
    Transliterate { romaji: String },
    /// Answered with [`Response::Conversion`].
    ConvertSentence { reading: String },
    /// Alternatives for one clause after the text of the clauses before it.
    /// Answered with [`Response::Candidates`].
    ClauseCandidates { reading: String, context: String },
    /// Learns a committed candidate, as following `previous` if given.
    /// Answered with [`Response::Done`].
    Learn {
        candidate: Candidate,
        previous: Option<Candidate>,
    },
    /// Answered with [`Response::Done`].
    AddWord { reading: String, text: String },
    /// Starts a background cloud lookup; an empty reading cancels it.
    /// Answered with [`Response::CloudEnabled`].
    QueueCloud { reading: String },
    /// The finished cloud lookup, empty while pending.
    /// Answered with [`Response::CloudCandidates`].
    PollCloud { reading: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ready,
    Done,
    Reading(String),
    Conversion(Conversion),
    Candidates(Vec<Candidate>),
    CloudEnabled(bool),
    CloudCandidates(Vec<CloudCandidate>),
    Error(String),
}
