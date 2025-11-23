pub const DEFAULT_FULLSCREEN_DURATION_MS: u128 = 4_200;
pub const DEFAULT_POLL_STEP_MS: u128 = 80;

pub fn normalized_movie_label(movie: &str) -> Option<&'static str> {
    let normalized = movie.trim().trim_end_matches(".snm").to_ascii_lowercase();
    match normalized.as_str() {
        "intro" => Some("movie.intro"),
        "logos" => Some("movie.logos"),
        "mo_ts" => Some("movie.mo_ts"),
        _ => None,
    }
}

pub fn default_fullscreen_duration_ms(movie: &str) -> u128 {
    match normalized_movie_label(movie) {
        Some("movie.logos") => DEFAULT_FULLSCREEN_DURATION_MS,
        Some("movie.intro") => DEFAULT_FULLSCREEN_DURATION_MS,
        Some("movie.mo_ts") => DEFAULT_FULLSCREEN_DURATION_MS,
        _ => DEFAULT_FULLSCREEN_DURATION_MS,
    }
}
