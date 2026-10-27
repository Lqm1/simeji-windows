use simeji_windows::cloud::CloudClient;
use std::{error::Error, path::PathBuf};
fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(args.next().ok_or(
        "Usage: simeji-cloud <user-data-directory> status | enable | disable | query <reading>",
    )?);
    let mut client = CloudClient::open(&directory)?;
    match args.next().as_deref() {
        Some("status") => println!(
            "Cloud conversion: {}",
            if client.enabled() {
                "enabled"
            } else {
                "disabled"
            }
        ),
        Some("enable") => {
            client.set_enabled(true)?;
            println!("Enabled: readings will be sent to https://cloud.simeji.me/py");
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
        _ => return Err("Expected status, enable, disable or query".into()),
    }
    Ok(())
}
