use simeji_protocol::{Request, Response};
use simeji_transport::{Pipe, exchange};
use simeji_windows::cloud_worker::CloudWorker;
use simeji_windows::{arm::ArmError, engine::Engine};
use std::{error::Error, path::PathBuf, time::Duration};

fn respond(
    engine: &mut Engine,
    cloud: &CloudWorker,
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
fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("serve")=>{
            let dict=PathBuf::from(args.next().ok_or("Missing dictionary directory")?);
            let learn=PathBuf::from(args.next().ok_or("Missing learning directory")?);
            let pipe=Pipe::server()?;let mut engine=Engine::open(&dict,&learn)?;
            let cloud=CloudWorker::new(&learn);
            loop {
                pipe.accept()?;
                let result=(||->Result<(),Box<dyn Error+Send+Sync>> {
                    let request=pipe.receive::<Request>(Duration::from_secs(2))?;
                    let response=respond(&mut engine,&cloud,request).unwrap_or_else(|e|Response::Error(e.to_string()));
                    pipe.send(&response,Duration::from_secs(2))?;
                    if pipe.receive::<u8>(Duration::from_secs(2))?!=0xa5 {return Err("Invalid worker acknowledgement".into());}
                    Ok(())
                })();
                if let Err(error)=result {eprintln!("Worker connection: {error}");}
                pipe.disconnect()?;
                if engine.is_faulted() {engine=Engine::open(&dict,&learn)?;}
            }
        }
        Some("ping")=>println!("{:?}",exchange::<_,Response>(&Request::Ping,Duration::from_secs(1))?),
        Some("convert")=>{
            let reading=args.next().ok_or("Missing reading")?;
            match exchange::<_,Response>(&Request::Candidates{reading,context:String::new()},Duration::from_secs(5))? {
                Response::Candidates(candidates)=>for candidate in candidates {println!("{}\t{}",candidate.text,candidate.reading);},
                Response::Error(error)=>return Err(error.into()),response=>return Err(format!("Unexpected worker response: {response:?}").into())
            }
        }
        Some("add-word")=>{
            let reading=args.next().ok_or("Missing reading")?;let text=args.next().ok_or("Missing word")?;
            match exchange::<_,Response>(&Request::AddWord{reading,text},Duration::from_secs(5))? {Response::Ready=>{},response=>return Err(format!("Unexpected worker response: {response:?}").into())}
        }
        _=>return Err("Usage: simeji-worker serve <dict> <learn> | ping | convert <reading> | add-word <reading> <word>".into())
    }
    Ok(())
}
