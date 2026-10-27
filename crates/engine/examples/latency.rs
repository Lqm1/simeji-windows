use simeji_engine::{Engine, EnginePaths};
use std::time::Instant;

fn main() {
    let scratch = std::env::temp_dir().join(format!("simeji-latency-{}", std::process::id()));
    let dictionary =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/dictionary");
    let start = Instant::now();
    let mut engine = Engine::open(EnginePaths {
        dictionary: &dictionary,
        learning: &scratch.join("Learning"),
        user_dictionary: &scratch.join("user-dictionary.json"),
    })
    .unwrap();
    println!("open           {:>9.2?}", start.elapsed());
    for _ in 0..2 {
        let start = Instant::now();
        engine.transliterate("kyouhaiitenki").unwrap();
        println!("transliterate  {:>9.2?}", start.elapsed());
        let start = Instant::now();
        let conversion = engine.convert_sentence("きょうはいいてんき").unwrap();
        println!("convert        {:>9.2?}", start.elapsed());
        let start = Instant::now();
        engine.clause_candidates("きょう", "").unwrap();
        println!("clause         {:>9.2?}", start.elapsed());
        let start = Instant::now();
        engine.learn(&conversion.candidates[0], None).unwrap();
        println!("learn          {:>9.2?}", start.elapsed());
    }
    drop(engine);
    let _ = std::fs::remove_dir_all(scratch);
}
