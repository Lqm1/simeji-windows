use simeji_protocol::{Request, Response};
use simeji_transport::exchange;
use std::{error::Error, path::PathBuf, time::Duration};

const USAGE: &str = "Usage: simeji-worker serve <dict> <learn> | ping | convert <reading> | add-word <reading> <word>";

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("serve") => {
            let dictionary = PathBuf::from(args.next().ok_or("Missing dictionary directory")?);
            let user_data = PathBuf::from(args.next().ok_or("Missing learning directory")?);
            simeji_worker::server::serve(&dictionary, &user_data)?;
        }
        Some("ping") => println!(
            "{:?}",
            exchange::<_, Response>(&Request::Ping, Duration::from_secs(1))?
        ),
        Some("convert") => {
            let reading = args.next().ok_or("Missing reading")?;
            let request = Request::Candidates {
                reading,
                context: String::new(),
            };
            match exchange::<_, Response>(&request, Duration::from_secs(5))? {
                Response::Candidates(candidates) => {
                    for candidate in candidates {
                        println!("{}\t{}", candidate.text, candidate.reading);
                    }
                }
                Response::Error(error) => return Err(error.into()),
                response => return Err(format!("Unexpected worker response: {response:?}").into()),
            }
        }
        Some("add-word") => {
            let reading = args.next().ok_or("Missing reading")?;
            let text = args.next().ok_or("Missing word")?;
            match exchange::<_, Response>(
                &Request::AddWord { reading, text },
                Duration::from_secs(5),
            )? {
                Response::Ready => {}
                response => return Err(format!("Unexpected worker response: {response:?}").into()),
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}
