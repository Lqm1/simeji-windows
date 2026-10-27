//! Typed messages shared by the Windows text service and conversion worker.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub text: String,
    pub reading: String,
    pub property: u32,
    pub left_id: u16,
    pub right_id: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    pub text: String,
    pub reading: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Conversion {
    pub candidates: Vec<Candidate>,
    pub clauses: Vec<Clause>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudWord {
    pub text: String,
    pub reading: String,
    pub property: u32,
    pub kind: u32,
    pub scene: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Ping,
    QueueCloud {
        reading: String,
    },
    PollCloud {
        reading: String,
    },
    Transliterate {
        romaji: String,
    },
    Convert {
        reading: String,
    },
    Candidates {
        reading: String,
        context: String,
    },
    Learn {
        candidate: Candidate,
        previous: Option<Candidate>,
    },
    AddWord {
        reading: String,
        text: String,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ready,
    CloudCandidates(Vec<CloudWord>),
    CloudEnabled(bool),
    Text(String),
    Conversion(Conversion),
    Candidates(Vec<Candidate>),
    Error(String),
}
