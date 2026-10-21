fn main() {
    #[cfg(target_os = "macos")]
    {
        use std::{path::Path, process::Command};

        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        if let Ok(output) = Command::new("xcrun").args(["--find", "swift"]).output() {
            let swift = String::from_utf8_lossy(&output.stdout);
            if let Some(toolchain_usr) = Path::new(swift.trim()).parent().and_then(Path::parent) {
                let compatibility_runtime = toolchain_usr.join("lib/swift-5.5/macosx");
                if compatibility_runtime.is_dir() {
                    println!(
                        "cargo:rustc-link-arg=-Wl,-rpath,{}",
                        compatibility_runtime.display()
                    );
                }
            }
        }
    }
    tauri_build::build()
}
