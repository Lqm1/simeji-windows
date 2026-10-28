use simeji_protocol::{Request, Response, WORKER_PIPE};
use simeji_worker::{installed_dictionary, server, user_data::UserData};
use std::{error::Error, time::Duration};

const USAGE: &str =
    "Usage: simeji-worker serve | ping | convert <reading> | add-word <reading> <word>";
const TIMEOUT: Duration = Duration::from_secs(5);

type BoxError = Box<dyn Error + Send + Sync>;

fn call(request: &Request) -> Result<Response, BoxError> {
    match simeji_transport::call(WORKER_PIPE, request, TIMEOUT)? {
        Response::Error(error) => Err(error.into()),
        response => Ok(response),
    }
}

fn main() -> Result<(), BoxError> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("serve") => {
            let user_data = UserData::locate().ok_or("LOCALAPPDATA is not set")?;
            server::serve(&installed_dictionary()?, &user_data)?;
        }
        Some("ping") => println!("{:?}", call(&Request::Ping)?),
        Some("convert") => {
            let reading = args.next().ok_or("Missing reading")?;
            let request = Request::ClauseCandidates {
                reading,
                context: String::new(),
            };
            let Response::Candidates(candidates) = call(&request)? else {
                return Err("Unexpected worker response".into());
            };
            for candidate in candidates {
                println!("{}\t{}", candidate.text, candidate.reading);
            }
        }
        Some("add-word") => {
            let reading = args.next().ok_or("Missing reading")?;
            let text = args.next().ok_or("Missing word")?;
            let Response::Done = call(&Request::AddWord { reading, text })? else {
                return Err("Unexpected worker response".into());
            };
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}
