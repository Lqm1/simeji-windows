use simeji_windows::elf::LibrarySet;

#[test]
fn executes_utf8_helper_from_the_original_engine() {
    let mut libraries = LibrarySet::bundled().unwrap();
    libraries.machine.map(0x1000, 4096).unwrap();
    libraries.machine.write(0x1000, "あ\0".as_bytes()).unwrap();
    assert_eq!(
        libraries
            .call("_ZN13CodeConverter17GetUTF8CharLengthEPKc", &[0x1000])
            .unwrap(),
        3
    );
    libraries.machine.write(0x1000, b"A\0").unwrap();
    assert_eq!(
        libraries
            .call("_ZN13CodeConverter17GetUTF8CharLengthEPKc", &[0x1000])
            .unwrap(),
        1
    );
}

#[test]
fn resolves_bundled_dependencies_and_retains_android_imports() {
    let libraries = LibrarySet::bundled().unwrap();
    assert!(libraries.symbol("GetConvertedCandidateList").is_ok());
    assert!(libraries.symbol("cmph_search_packed").is_ok());
    assert!(libraries.symbol("unknown_function").is_err());
    assert!(
        libraries
            .imports
            .iter()
            .any(|import| import.name == "fopen")
    );
    assert!(!libraries.initializers.is_empty());
}

#[test]
fn refuses_to_execute_an_unimplemented_android_import() {
    let mut libraries = LibrarySet::bundled().unwrap();
    assert!(libraries.call("fopen", &[0, 0]).is_err());
}
