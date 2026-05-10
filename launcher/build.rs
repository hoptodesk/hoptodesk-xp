
fn main() {
    #[cfg(target_os = "windows")]
    {
        cc::Build::new()
            .file("../src/fls_shim.c")
            .file("../src/fls_imp.asm")
            .compile("fls_shim_launcher");

        let mut res = winres::WindowsResource::new();
        res.set_icon("../res/icon.ico");
        res.compile().unwrap_or_default();
    }
}
