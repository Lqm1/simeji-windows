//! Serves one request per pipe connection with the engine and the cloud queue.

use crate::cloud::CloudQueue;
use simeji_engine::{ArmError, Engine};
use simeji_protocol::{Request, Response};
use simeji_transport::Pipe;
use std::{error::Error, path::Path, time::Duration};

type BoxError = Box<dyn Error + Send + Sync>;

/// Upper bound for each read and write on a client connection.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The client sends this after reading the reply, so the server keeps the
/// connection open until the complete response was consumed.
const ACKNOWLEDGEMENT: u8 = 0xa5;

pub fn respond(
    engine: &mut Engine,
    cloud: &CloudQueue,
    request: Request,
) -> Result<Response, ArmError> {
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

/// Runs the worker until the pipe fails. A native execution fault only
/// fails the current request; the engine is reopened before the next one.
pub fn serve(dictionary: &Path, user_data: &Path) -> Result<(), BoxError> {
    let pipe = Pipe::server()?;
    let mut engine = Engine::open(dictionary, user_data)?;
    let cloud = CloudQueue::new(user_data);
    loop {
        pipe.accept()?;
        if let Err(error) = exchange(&pipe, &mut engine, &cloud) {
            eprintln!("Worker connection: {error}");
        }
        pipe.disconnect()?;
        if engine.is_faulted() {
            engine = Engine::open(dictionary, user_data)?;
        }
    }
}

fn exchange(pipe: &Pipe, engine: &mut Engine, cloud: &CloudQueue) -> Result<(), BoxError> {
    let request = pipe.receive::<Request>(IO_TIMEOUT)?;
    let response =
        respond(engine, cloud, request).unwrap_or_else(|error| Response::Error(error.to_string()));
    pipe.send(&response, IO_TIMEOUT)?;
    if pipe.receive::<u8>(IO_TIMEOUT)? != ACKNOWLEDGEMENT {
        return Err("Invalid worker acknowledgement".into());
    }
    Ok(())
}
