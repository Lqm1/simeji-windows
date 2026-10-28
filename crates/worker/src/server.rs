//! Serves one request per pipe connection with the engine and the
//! background lookups.

use crate::{cloud::CloudQueue, suggestions::SuggestionQueue, user_data::UserData};
use simeji_engine::{Engine, EngineError, EnginePaths};
use simeji_protocol::{Lookup, Request, Response, WORKER_PIPE};
use simeji_transport::Pipe;
use std::{error::Error, path::Path, time::Duration};

const IO_TIMEOUT: Duration = Duration::from_secs(2);

pub fn open_engine(dictionary: &Path, user_data: &UserData) -> Result<Engine, EngineError> {
    Engine::open(EnginePaths {
        dictionary,
        learning: &user_data.learning_directory(),
        user_dictionary: &user_data.user_dictionary_file(),
    })
}

/// Lookups for the reading being typed, answered without the main engine.
pub struct Lookups {
    pub suggestions: SuggestionQueue,
    pub cloud: CloudQueue,
}

impl Lookups {
    pub fn new(dictionary: &Path, user_data: &UserData) -> Self {
        Self {
            suggestions: SuggestionQueue::new(dictionary, user_data),
            cloud: CloudQueue::new(user_data),
        }
    }
}

pub fn dispatch(
    engine: &mut Engine,
    lookups: &Lookups,
    request: Request,
) -> Result<Response, EngineError> {
    Ok(match request {
        Request::Ping => Response::Ready,
        Request::Transliterate { romaji } => Response::Reading(engine.transliterate(&romaji)?),
        Request::ConvertSentence { reading } => {
            Response::Conversion(engine.convert_sentence(&reading)?)
        }
        Request::ClauseCandidates { reading, context } => {
            Response::Candidates(engine.clause_candidates(&reading, &context)?)
        }
        Request::Learn {
            candidate,
            previous,
        } => {
            engine.learn(&candidate, previous.as_ref())?;
            lookups.suggestions.learned(candidate, previous);
            Response::Done
        }
        Request::AddWord { reading, text } => {
            engine.add_word(&reading, &text)?;
            lookups.suggestions.user_dictionary_changed();
            Response::Done
        }
        Request::StartLookup { reading, cloud } => {
            lookups.suggestions.queue(reading.clone());
            let reading = if cloud { reading } else { String::new() };
            Response::CloudEnabled(lookups.cloud.queue(reading))
        }
        Request::PollLookup { reading } => {
            let suggestion = lookups.suggestions.poll(&reading);
            Response::Lookup(Lookup {
                first_clause: suggestion
                    .as_ref()
                    .and_then(|suggestion| suggestion.first_clause.clone()),
                conversion: suggestion.map(|suggestion| suggestion.conversion),
                cloud: lookups.cloud.poll(&reading),
            })
        }
    })
}

/// Runs until the pipe fails. A native execution fault fails only the
/// current request; the engine is reopened before the next one.
pub fn serve(dictionary: &Path, user_data: &UserData) -> Result<(), Box<dyn Error + Send + Sync>> {
    let pipe = Pipe::listen(WORKER_PIPE)?;
    let mut engine = open_engine(dictionary, user_data)?;
    let lookups = Lookups::new(dictionary, user_data);
    loop {
        pipe.accept()?;
        let answered = pipe.answer(IO_TIMEOUT, |request| {
            dispatch(&mut engine, &lookups, request)
                .unwrap_or_else(|error| Response::Error(error.to_string()))
        });
        if let Err(error) = answered {
            eprintln!("Worker connection: {error}");
        }
        pipe.disconnect()?;
        if engine.is_faulted() {
            engine = open_engine(dictionary, user_data)?;
        }
    }
}
