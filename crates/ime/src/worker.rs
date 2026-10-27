//! Requests to the conversion worker process.

use simeji_protocol::{Candidate, CloudCandidate, Conversion, Request, Response, WORKER_PIPE};
use std::{fmt, time::Duration};
use windows::Win32::Foundation::{E_FAIL, E_UNEXPECTED};

/// Bound for requests made while the user waits for a key to take effect.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Polling runs on a timer in the application's UI thread and must not stall it.
const POLL_TIMEOUT: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerError {
    /// The worker was unreachable or reported an error.
    Failed(String),
    /// The worker answered with a response of the wrong kind.
    Unexpected,
}

impl fmt::Display for WorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(message) => f.write_str(message),
            Self::Unexpected => f.write_str("Unexpected conversion worker response"),
        }
    }
}

impl From<WorkerError> for windows::core::Error {
    fn from(error: WorkerError) -> Self {
        match error {
            WorkerError::Failed(message) => Self::new(E_FAIL, message),
            WorkerError::Unexpected => E_UNEXPECTED.into(),
        }
    }
}

/// The conversions the composer needs while handling a key.
pub(crate) trait Converter {
    /// Romaji to the hiragana reading.
    fn transliterate(&mut self, romaji: &str) -> Result<String, WorkerError>;
    /// The best sentence for a reading, split into clauses.
    fn convert_sentence(&mut self, reading: &str) -> Result<Conversion, WorkerError>;
    /// Alternatives for one clause, given the text of the clauses before it.
    fn clause_candidates(
        &mut self,
        reading: &str,
        context: &str,
    ) -> Result<Vec<Candidate>, WorkerError>;
}

/// The worker reached over its local pipe; every request uses a new connection.
pub(crate) struct Worker;

impl Worker {
    fn request(request: &Request, timeout: Duration) -> Result<Response, WorkerError> {
        simeji_transport::call(WORKER_PIPE, request, timeout)
            .map_err(|error| WorkerError::Failed(error.to_string()))
    }

    /// Records the candidate the user committed. Learning is best effort and
    /// never fails the commit.
    pub fn learn(&self, candidate: Candidate, previous: Option<Candidate>) {
        let _ = Self::request(
            &Request::Learn {
                candidate,
                previous,
            },
            REQUEST_TIMEOUT,
        );
    }

    /// Starts a background cloud lookup for `reading` (empty cancels) and
    /// reports whether cloud conversion is enabled.
    pub fn queue_cloud(&self, reading: String) -> bool {
        matches!(
            Self::request(&Request::QueueCloud { reading }, REQUEST_TIMEOUT),
            Ok(Response::CloudEnabled(true))
        )
    }

    /// The finished cloud result for `reading`. Never performs HTTP itself.
    pub fn poll_cloud(&self, reading: String) -> Option<Vec<CloudCandidate>> {
        match Self::request(&Request::PollCloud { reading }, POLL_TIMEOUT) {
            Ok(Response::CloudCandidates(candidates)) => Some(candidates),
            _ => None,
        }
    }
}

fn unexpected_response(response: Response) -> WorkerError {
    match response {
        Response::Error(message) => WorkerError::Failed(message),
        _ => WorkerError::Unexpected,
    }
}

impl Converter for Worker {
    fn transliterate(&mut self, romaji: &str) -> Result<String, WorkerError> {
        let request = Request::Transliterate {
            romaji: romaji.to_owned(),
        };
        match Self::request(&request, REQUEST_TIMEOUT)? {
            Response::Reading(reading) => Ok(reading),
            response => Err(unexpected_response(response)),
        }
    }

    fn convert_sentence(&mut self, reading: &str) -> Result<Conversion, WorkerError> {
        let request = Request::ConvertSentence {
            reading: reading.to_owned(),
        };
        match Self::request(&request, REQUEST_TIMEOUT)? {
            Response::Conversion(conversion) => Ok(conversion),
            response => Err(unexpected_response(response)),
        }
    }

    fn clause_candidates(
        &mut self,
        reading: &str,
        context: &str,
    ) -> Result<Vec<Candidate>, WorkerError> {
        let request = Request::ClauseCandidates {
            reading: reading.to_owned(),
            context: context.to_owned(),
        };
        match Self::request(&request, REQUEST_TIMEOUT)? {
            Response::Candidates(candidates) => Ok(candidates),
            response => Err(unexpected_response(response)),
        }
    }
}
