fn main() {
    // tauri-build reads tauri.conf.json, sets up the Windows app manifest, and
    // generates the ACL. It does not read `frontendDist`; the static UI in
    // `ui/` is embedded by the tauri crate itself at compile time.
    tauri_build::build();
}
