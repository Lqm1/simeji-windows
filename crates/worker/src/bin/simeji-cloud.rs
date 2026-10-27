use simeji_worker::{
    cloud::{CloudClient, ENDPOINT},
    user_data::UserData,
};
use std::error::Error;

const USAGE: &str = "Usage: simeji-cloud status | enable | disable | query <reading>";

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let user_data = UserData::locate().ok_or("LOCALAPPDATA is not set")?;
    let mut client = CloudClient::open(&user_data)?;
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
            for candidate in client.query(&args.next().ok_or("Missing reading")?)? {
                println!("{}\t{}", candidate.text, candidate.reading);
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}
