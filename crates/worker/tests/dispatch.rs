use simeji_protocol::{Request, Response};
use simeji_worker::{
    server::{Lookups, dispatch, open_engine},
    user_data::UserData,
};
use std::path::Path;

#[test]
fn answers_each_request_with_the_engine_and_lookups() {
    let user_data = UserData::at(
        std::env::temp_dir().join(format!("simeji-dispatch-test-{}", std::process::id())),
    );
    let dictionary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/dictionary");
    let mut engine = open_engine(&dictionary, &user_data).unwrap();
    let lookups = Lookups::new(&dictionary, &user_data);
    let mut ask = |request| dispatch(&mut engine, &lookups, request).unwrap();

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
        ask(Request::StartLookup {
            reading: "ねこ".into(),
            cloud: true,
        }),
        Response::CloudEnabled(false)
    ));
    let start = std::time::Instant::now();
    let conversion = loop {
        let Response::Lookup(lookup) = ask(Request::PollLookup {
            reading: "ねこ".into(),
        }) else {
            panic!("expected a lookup");
        };
        assert_eq!(lookup.cloud, None);
        if let Some(conversion) = lookup.conversion {
            break conversion;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(conversion.candidates[0].text, "猫");
    let invalid = Request::ConvertSentence {
        reading: "a\0b".into(),
    };
    assert!(dispatch(&mut engine, &lookups, invalid).is_err());
    assert!(!engine.is_faulted());

    drop(lookups);
    drop(engine);
    std::fs::remove_dir_all(user_data.root()).unwrap();
}
