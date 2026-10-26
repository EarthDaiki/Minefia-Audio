use std::{
    env,
    path::{Path, PathBuf},
    sync::OnceLock,
};

static RESOURCE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Save Tauri's resource root once so every media path uses the bundled tools first.
pub fn initialize(resource_dir: PathBuf) {
    let _ = RESOURCE_DIR.set(resource_dir);
}

pub fn find_binary(name: &str) -> Option<PathBuf> {
    let override_name = format!("MINEFIA_{}", name.replace('-', "_").to_uppercase());
    if let Some(path) = env::var_os(override_name)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(path);
    }

    if let Some(resource_dir) = RESOURCE_DIR.get() {
        if let Some(path) = bundled_path(resource_dir, name).filter(|path| path.is_file()) {
            return Some(path);
        }
    }

    let executable = executable_name(name);
    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths) {
            let candidate = directory.join(&executable);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
        .iter()
        .map(|directory| Path::new(directory).join(&executable))
        .find(|path| path.is_file())
}

fn bundled_path(resource_dir: &Path, name: &str) -> Option<PathBuf> {
    let root = resource_dir.join("tools");
    let executable = executable_name(name);
    let relative = match name {
        "ffmpeg" | "ffprobe" => Path::new("ffmpeg").join("bin").join(executable),
        "yt-dlp" => Path::new("yt-dlp").join(executable),
        _ => return None,
    };
    Some(root.join(relative))
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_paths_match_the_staged_resource_layout() {
        let root = Path::new("/app/resources");
        assert_eq!(
            bundled_path(root, "ffmpeg"),
            Some(root.join("tools/ffmpeg/bin/ffmpeg"))
        );
        assert_eq!(
            bundled_path(root, "ffprobe"),
            Some(root.join("tools/ffmpeg/bin/ffprobe"))
        );
        assert_eq!(
            bundled_path(root, "yt-dlp"),
            Some(root.join("tools/yt-dlp/yt-dlp"))
        );
        assert_eq!(bundled_path(root, "unknown"), None);
    }
}
