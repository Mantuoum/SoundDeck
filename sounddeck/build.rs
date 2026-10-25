fn main() {
    // Embed the application icon so the exe stays fully portable (single file).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/app.ico");
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "SoundDeck");
        res.set("FileDescription", "SoundDeck - lightweight soundboard");
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        if let Err(err) = res.compile() {
            eprintln!("warning: could not embed icon/version resource: {err}");
        }
    }
}
