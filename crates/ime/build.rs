/// COM looks these up with `GetProcAddress`; marking them PRIVATE keeps
/// them out of the import library, as the linker expects for COM exports.
const COM_EXPORTS: [&str; 4] = [
    "DllCanUnloadNow",
    "DllGetClassObject",
    "DllRegisterServer",
    "DllUnregisterServer",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        for export in COM_EXPORTS {
            println!("cargo:rustc-cdylib-link-arg=/EXPORT:{export},PRIVATE");
        }
    }
}
