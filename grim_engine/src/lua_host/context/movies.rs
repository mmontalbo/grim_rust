use std::path::Path;

pub(super) fn select_playback(install_root: &Path, movie: &str, headless: bool) {
    if resolve_remastered_movie(install_root, movie).is_none() && !headless {
        eprintln!(
            "[grim_engine] remastered movie asset missing for {movie} under {}; simulating playback",
            install_root.display()
        );
    }

    if headless {
        eprintln!(
            "[grim_engine] headless: simulating fullscreen movie {} without viewer",
            movie
        );
    }
}

fn resolve_remastered_movie(install_root: &Path, movie: &str) -> Option<String> {
    let stem = Path::new(movie).file_stem()?;
    let lowered = stem.to_string_lossy().to_lowercase();
    if lowered.is_empty() {
        return None;
    }
    let relative = Path::new("MoviesHD").join(format!("{lowered}.ogv"));
    let full = install_root.join(&relative);
    if full.is_file() {
        Some(relative.to_string_lossy().to_string())
    } else {
        None
    }
}
