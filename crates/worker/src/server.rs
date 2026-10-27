//! Serves one request per pipe connection with the engine and the cloud queue.

use crate::cloud::CloudQueue;
use simeji_engine::{Engine, EngineError};
use simeji_protocol::{Request, Response};
use simeji_transport::Pipe;
use std::{error::Error, path::Path, time::Duration};

const IO_TIMEOUT: Duration = Duration::from_secs(2);

pub fn dispatch(
    engine: &mut Engine,
    cloud: &CloudQueue,
    request: Request,
) -> Result<Response, EngineError> {
    Ok(match request {
        Request::Ping => Response::Ready,
        Request::QueueCloud { reading } => Response::CloudEnabled(cloud.queue(reading)),
        Request::PollCloud { reading } => Response::CloudCandidates(cloud.poll(&reading)),
        Request::Transliterate { romaji } => Response::Text(engine.transliterate(&romaji)?),
        Request::Convert { reading } => Response::Conversion(engine.convert_sentence(&reading)?),
        Request::Candidates { reading, context } => {
            Response::Candidates(engine.clause_candidates(&reading, &context)?)
        }
        Request::Learn {
            candidate,
            previous,
        } => {
            engine.learn(&candidate, previous.as_ref())?;
            Response::Ready
        }
        Request::AddWord { reading, text } => {
            engine.add_word(&reading, &text)?;
            Response::Ready
        }
    })
}

/// Runs until the pipe fails. A native execution fault fails only the
/// current request; the engine is reopened before the next one.
pub fn serve(dictionary: &Path, user_data: &Path) -> Result<(), Box<dyn Error + Send + Sync>> {
    let pipe = Pipe::server()?;
    let mut engine = Engine::open(dictionary, user_data)?;
    let cloud = CloudQueue::new(user_data);
    loop {
        pipe.accept()?;
        let answered = pipe.answer(IO_TIMEOUT, |request| {
            dispatch(&mut engine, &cloud, request)
                .unwrap_or_else(|error| Response::Error(error.to_string()))
        });
        if let Err(error) = answered {
            eprintln!("Worker connection: {error}");
        }
        pipe.disconnect()?;
        if engine.is_faulted() {
            engine = Engine::open(dictionary, user_data)?;
        }
    }
}
