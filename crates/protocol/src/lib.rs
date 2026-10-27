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

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Ping,
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
    Text(String),
    Conversion(Conversion),
    Candidates(Vec<Candidate>),
    Error(String),
}
