use simeji_worker::cloud::{CloudClient, ENDPOINT};
use std::{error::Error, path::PathBuf};

const USAGE: &str =
    "Usage: simeji-cloud <user-data-directory> status | enable | disable | query <reading>";

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut client = CloudClient::open(&directory)?;
    match args.next().as_deref() {
        Some("status") => {
            let status = if client.is_enabled() {
                "enabled"
            } else {
                "disabled"
            };
            println!("Cloud conversion: {status}");
        }
        Some("enable") => {
            client.set_enabled(true)?;
            println!("Enabled: readings will be sent to {ENDPOINT}");
        }
        Some("disable") => {
            client.set_enabled(false)?;
            println!("Cloud conversion disabled");
        }
        Some("query") => {
            for word in client.query(&args.next().ok_or("Missing reading")?)? {
                println!("{}\t{}", word.text, word.reading);
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}
