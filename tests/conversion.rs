use simeji_windows::engine::Engine;

#[test]
fn converts_readings_with_the_bundled_dictionary() {
    let learn = std::env::temp_dir().join(format!("simeji-conversion-test-{}", std::process::id()));
    let dict = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/dict");
    let mut engine = Engine::open(&dict, &learn).unwrap();
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
    drop(engine);
    std::fs::remove_dir_all(learn).unwrap();
}
