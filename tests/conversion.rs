use simeji_windows::engine::Engine;

#[test]
fn converts_readings_with_the_bundled_dictionary() {
    let learn = std::env::temp_dir().join(format!("simeji-conversion-test-{}", std::process::id()));
    let dict = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/dict");
    let mut engine = Engine::open(&dict, &learn).unwrap();
    assert_eq!(engine.transliterate("nihongo").unwrap(), "にほんご");
    for (reading, expected) in [
        ("にほんご", "日本語"),
        ("とうきょう", "東京"),
        ("ねこ", "猫"),
    ] {
        let candidates = engine.convert(reading).unwrap();
        assert_eq!(candidates[0].text, expected);
    }
    assert!(engine.convert("abc\0def").is_err());
    assert!(engine.convert("").unwrap().is_empty());
    let alternatives = engine.clause_candidates("ねこ", "").unwrap();
    assert!(alternatives.len() > 1);
    assert!(alternatives.iter().any(|c| c.text == "猫"));
    assert!(alternatives.iter().any(|c| c.text == "ネコ"));
    for _ in 0..5 {
        assert_eq!(engine.convert("にほんご").unwrap()[0].text, "日本語");
    }
    let sentence = engine.convert_sentence("きょうはいいてんきですね").unwrap();
    assert!(sentence.clauses.len() > 1);
    assert_eq!(
        sentence
            .clauses
            .iter()
            .map(|c| c.text.as_str())
            .collect::<String>(),
        sentence.candidates[0].text
    );
    assert_eq!(
        sentence
            .clauses
            .iter()
            .map(|c| c.reading.as_str())
            .collect::<String>(),
        sentence.candidates[0].reading
    );
    let learned = alternatives.iter().find(|c| c.text == "根子").unwrap();
    engine.learn(learned, None).unwrap();
    engine.add_word("こーでっくす", "私用検証語").unwrap();
    assert!(
        engine
            .clause_candidates("こーでっくす", "")
            .unwrap()
            .iter()
            .any(|c| c.text == "私用検証語")
    );
    drop(engine);
    let mut reopened = Engine::open(&dict, &learn).unwrap();
    assert!(
        reopened
            .clause_candidates("こーでっくす", "")
            .unwrap()
            .iter()
            .any(|c| c.text == "私用検証語")
    );
    assert_eq!(
        reopened.clause_candidates("ねこ", "").unwrap()[0].text,
        "根子"
    );
    drop(reopened);
    std::fs::remove_dir_all(learn).unwrap();
}
