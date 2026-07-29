fn main() {

    let out_dir = std::env::var("OUT_DIR").unwrap();
    protobuf_codegen::Codegen::new()
        .pure()
        .out_dir(&out_dir)
        .inputs(&["protos/rendezvous.proto", "protos/message.proto"])
        .include("protos")
        .run()
        .expect("protobuf codegen failed");

    for name in &["rendezvous.rs", "message.rs"] {
        let path = std::path::Path::new(&out_dir).join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            let filtered: String = content
                .lines()
                .filter(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with("#!") && !trimmed.starts_with("//!")
                })
                .collect::<Vec<_>>()
                .join("\n");
            let _ = std::fs::write(&path, filtered);
        }
    }

    println!("cargo:rustc-link-lib=user32");
    println!("cargo:rustc-link-lib=gdi32");
    println!("cargo:rustc-link-lib=kernel32");
    println!("cargo:rustc-link-lib=shell32");
    println!("cargo:rustc-link-lib=advapi32");

    let vcpkg_root = std::env::var("VCPKG_ROOT").unwrap_or_else(|_| "C:/vcpkg".to_string());
    println!("cargo:rustc-link-search=native={}/installed/x86-windows-static/lib", vcpkg_root);
    println!("cargo:rustc-link-lib=static=vpx");

    #[cfg(target_os = "windows")]
    {
        let vcpkg_inc = format!("{}/installed/x86-windows-static/include", vcpkg_root);
        cc::Build::new()
            .file("src/vpx_helper.c")
            .include(&vcpkg_inc)
            .compile("vpx_helper");
    }

    #[cfg(target_os = "windows")]
    {
        cc::Build::new()
            .file("src/bcrypt_shim.c")
            .file("src/bcrypt_imp.asm")
            .compile("bcrypt_shim");
    }

    #[cfg(target_os = "windows")]
    {
        cc::Build::new()
            .file("src/fls_shim.c")
            .file("src/fls_imp.asm")
            .compile("fls_shim");
    }

    #[cfg(feature = "packui")]
    {
        let rc_path = std::path::PathBuf::from("target/resources.rc");
        if rc_path.exists() {
            println!("cargo:warning=target/resources.rc already exists, skipping packfolder");
        } else {
            let pf_path = std::path::PathBuf::from("target/packfolder.exe");
            if pf_path.exists() {
                std::process::Command::new(pf_path)
                    .args(["src/ui", "target/resources.rc", "-i", "*.html;*.css;*.tis", "-v", "resources", "-binary"])
                    .output()
                    .expect("packfolder failed!");
            } else {
                panic!("packfolder.exe not found at target/packfolder.exe");
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("res/icon.ico");
        res.compile().unwrap_or_default();
    }
}
