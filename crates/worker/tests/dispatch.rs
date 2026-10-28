use simeji_engine::Engine;
use simeji_protocol::{Request, Response};
use simeji_worker::{cloud::CloudQueue, server::dispatch};

#[test]
fn answers_each_request_with_the_engine_and_cloud_queue() {
    let data = std::env::temp_dir().join(format!("simeji-dispatch-test-{}", std::process::id()));
    let dictionary = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/dict");
    let mut engine = Engine::open(&dictionary, &data).unwrap();
    let cloud = CloudQueue::new(&data);
    let mut ask = |request| dispatch(&mut engine, &cloud, request).unwrap();

    assert!(matches!(ask(Request::Ping), Response::Ready));
    assert!(matches!(
        ask(Request::Transliterate { romaji: "nihongo".into() }),
        Response::Text(text) if text == "にほんご"
    ));
    assert!(matches!(
        ask(Request::Convert { reading: "にほんご".into() }),
        Response::Conversion(conversion) if conversion.candidates[0].text == "日本語"
    ));
    assert!(matches!(
        ask(Request::Candidates { reading: "ねこ".into(), context: String::new() }),
        Response::Candidates(candidates) if candidates.iter().any(|c| c.text == "猫")
    ));
    // Cloud conversion stays off until the user enables it.
    assert!(matches!(
        ask(Request::QueueCloud {
            reading: "ねこ".into()
        }),
        Response::CloudEnabled(false)
    ));
    assert!(matches!(
        ask(Request::PollCloud { reading: "ねこ".into() }),
        Response::CloudCandidates(words) if words.is_empty()
    ));
    assert!(
        dispatch(
            &mut engine,
            &cloud,
            Request::Convert {
                reading: "a\0b".into()
            }
        )
        .is_err()
    );
    assert!(!engine.is_faulted());

    drop(cloud);
    drop(engine);
    std::fs::remove_dir_all(data).unwrap();
}
