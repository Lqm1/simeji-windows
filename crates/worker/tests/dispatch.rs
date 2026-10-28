use simeji_protocol::{Request, Response};
use simeji_worker::{
    cloud::CloudQueue,
    server::{dispatch, open_engine},
    user_data::UserData,
};
use std::path::Path;

#[test]
fn answers_each_request_with_the_engine_and_cloud_queue() {
    let user_data = UserData::at(
        std::env::temp_dir().join(format!("simeji-dispatch-test-{}", std::process::id())),
    );
    let dictionary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/dictionary");
    let mut engine = open_engine(&dictionary, &user_data).unwrap();
    let cloud = CloudQueue::new(&user_data);
    let mut ask = |request| dispatch(&mut engine, &cloud, request).unwrap();

    assert!(matches!(ask(Request::Ping), Response::Ready));
    assert!(matches!(
        ask(Request::Transliterate { romaji: "nihongo".into() }),
        Response::Reading(reading) if reading == "にほんご"
    ));
    assert!(matches!(
        ask(Request::ConvertSentence { reading: "にほんご".into() }),
        Response::Conversion(conversion) if conversion.candidates[0].text == "日本語"
    ));
    assert!(matches!(
        ask(Request::ClauseCandidates { reading: "ねこ".into(), context: String::new() }),
        Response::Candidates(candidates) if candidates.iter().any(|c| c.text == "猫")
    ));
    assert!(matches!(
        ask(Request::QueueCloud {
            reading: "ねこ".into()
        }),
        Response::CloudEnabled(false)
    ));
    assert!(matches!(
        ask(Request::PollCloud { reading: "ねこ".into() }),
        Response::CloudCandidates(candidates) if candidates.is_empty()
    ));
    let invalid = Request::ConvertSentence {
        reading: "a\0b".into(),
    };
    assert!(dispatch(&mut engine, &cloud, invalid).is_err());
    assert!(!engine.is_faulted());

    drop(cloud);
    drop(engine);
    std::fs::remove_dir_all(user_data.root()).unwrap();
}
