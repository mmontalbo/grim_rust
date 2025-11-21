use std::path::Path;
use std::rc::Rc;

use crate::stream::StreamServer;
use grim_stream::MovieStart;

use super::cutscenes::FullscreenMoviePlayback;

pub(super) fn select_playback(
    stream: Option<Rc<StreamServer>>,
    install_root: &Path,
    movie: &str,
) -> Option<FullscreenMoviePlayback> {
    let stream = stream?;
    prepare_viewer_playback(stream, install_root, movie)
}

pub(super) fn viewer_ready(stream: Option<&Rc<StreamServer>>, expected_generation: u64) -> bool {
    match stream {
        Some(stream) => {
            stream.current_generation() == expected_generation && stream.viewer_gate().is_ready()
        }
        None => false,
    }
}

fn prepare_viewer_playback(
    stream: Rc<StreamServer>,
    install_root: &Path,
    movie: &str,
) -> Option<FullscreenMoviePlayback> {
    if !stream.viewer_gate().is_ready() {
        eprintln!("[grim_engine] viewer gate not ready for movie {movie}");
        return None;
    }
    let relative_path = resolve_remastered_movie(install_root, movie).or_else(|| {
        eprintln!(
            "[grim_engine] remastered movie asset missing for {movie} under {}",
            install_root.display()
        );
        None
    })?;
    let generation = stream.current_generation();
    let start = MovieStart {
        name: movie.to_string(),
        relative_path: Some(relative_path.clone()),
    };
    if let Err(err) = stream.send_movie_start(start) {
        eprintln!("[grim_engine] failed to send MovieStart for {movie}: {err:?}");
        return None;
    }
    // TODO: add explicit MovieReady/Error handshake so viewer-side failures are visible sooner.
    Some(FullscreenMoviePlayback::Viewer { generation })
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
