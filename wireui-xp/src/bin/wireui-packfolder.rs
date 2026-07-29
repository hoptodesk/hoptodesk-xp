use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut positional: Vec<String> = Vec::new();
    let mut patterns: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-i" => {
                if let Some(list) = args.next() {
                    patterns = list.split(';').map(|s| s.to_string()).collect();
                }
            }
            "-v" => {
                let _ = args.next();
            }
            "-binary" => {}
            other => positional.push(other.to_string()),
        }
    }
    if positional.len() != 2 {
        eprintln!("usage: wireui-packfolder <folder> <out.rc> [-i \"*.html;*.css\"] [-v name] [-binary]");
        return ExitCode::from(2);
    }
    let root = PathBuf::from(&positional[0]);
    let out = PathBuf::from(&positional[1]);
    let entries = match sciter::engine::archive::pack_dir(&root, &patterns) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("pack failed: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let bytes = sciter::engine::archive::Archive::write(&entries);
    if let Err(e) = std::fs::write(&out, &bytes) {
        eprintln!("write {}: {}", out.display(), e);
        return ExitCode::FAILURE;
    }
    println!(
        "packed {} entries ({} bytes) from {} to {}",
        entries.len(),
        bytes.len(),
        root.display(),
        out.display()
    );
    ExitCode::SUCCESS
}
