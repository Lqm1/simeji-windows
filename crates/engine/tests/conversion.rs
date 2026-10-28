use simeji_engine::{Engine, EnginePaths};
use std::path::{Path, PathBuf};

struct TestData(PathBuf);

impl TestData {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("simeji-conversion-test-{}", std::process::id())))
    }

    fn open(&self) -> Engine {
        let dictionary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/dictionary");
        Engine::open(EnginePaths {
            dictionary: &dictionary,
            learning: &self.0.join("Learning"),
            user_dictionary: &self.0.join("user-dictionary.json"),
        })
        .unwrap()
    }
}

impl Drop for TestData {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn converts_readings_with_the_bundled_dictionary() {
    let data = TestData::new();
    let mut engine = data.open();
    assert_eq!(engine.transliterate("nihongo").unwrap(), "にほんご");
    // The composer keeps kana left of the caret and appends romaji to it.
    assert_eq!(engine.transliterate("きゃku").unwrap(), "きゃく");
    assert_eq!(engine.transliterate("nihon").unwrap(), "にほn");
    for (reading, expected) in [
        ("にほんご", "日本語"),
        ("とうきょう", "東京"),
        ("ねこ", "猫"),
    ] {
        let candidates = engine.convert_sentence(reading).unwrap().candidates;
        assert_eq!(candidates[0].text, expected);
    }
    assert!(engine.convert_sentence("abc\0def").is_err());
    assert!(engine.convert_sentence("").unwrap().candidates.is_empty());
    let alternatives = engine.clause_candidates("ねこ", "").unwrap();
    assert!(alternatives.len() > 1);
    assert!(alternatives.iter().any(|c| c.text == "猫"));
    assert!(alternatives.iter().any(|c| c.text == "ネコ"));
    assert!(
        engine
            .clause_candidates("にほんご", "")
            .unwrap()
            .iter()
            .any(|c| c.text == "日本語")
    );
    for _ in 0..5 {
        assert_eq!(
            engine.convert_sentence("にほんご").unwrap().candidates[0].text,
            "日本語"
        );
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
    let mut reopened = data.open();
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
}
