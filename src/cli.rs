use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "rlv-forge",
    about = env!("CARGO_PKG_DESCRIPTION"),
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RLV_FORGE_GIT_SHA"), ")")
)]
pub struct Args {
    /// Open a saved project
    #[arg(long, value_name = "PROJECT", conflicts_with_all = ["fit_gpx", "video", "path", "video_path"])]
    pub gpxproj: Option<PathBuf>,

    /// Import a GPX or FIT track
    #[arg(long, value_name = "TRACK", conflicts_with = "path")]
    pub fit_gpx: Option<PathBuf>,

    /// Open a prepared video
    #[arg(long, value_name = "VIDEO", conflicts_with = "video_path")]
    pub video: Option<PathBuf>,

    /// Track or project to open (legacy positional argument)
    #[arg(value_name = "TRACK_OR_PROJECT")]
    pub path: Option<PathBuf>,

    /// Prepared video to open (legacy positional argument)
    #[arg(value_name = "VIDEO")]
    pub video_path: Option<PathBuf>,
}

impl Args {
    pub fn startup_paths(self) -> (Option<PathBuf>, Option<PathBuf>) {
        (
            self.gpxproj.or(self.fit_gpx).or(self.path),
            self.video.or(self.video_path),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn accepts_startup_files() {
        for (args, path, video) in [
            (vec!["rlv-forge"], None, None),
            (
                vec!["rlv-forge", "--gpxproj", "ride.gpxproj"],
                Some("ride.gpxproj"),
                None,
            ),
            (
                vec!["rlv-forge", "--fit-gpx", "ride.fit"],
                Some("ride.fit"),
                None,
            ),
            (
                vec!["rlv-forge", "--video", "ride.mp4"],
                None,
                Some("ride.mp4"),
            ),
            (
                vec!["rlv-forge", "--video", "ride.mp4", "--fit-gpx", "ride.gpx"],
                Some("ride.gpx"),
                Some("ride.mp4"),
            ),
            (
                vec!["rlv-forge", "ride.gpx", "ride.mp4"],
                Some("ride.gpx"),
                Some("ride.mp4"),
            ),
            (
                vec!["rlv-forge", "ride.gpxproj"],
                Some("ride.gpxproj"),
                None,
            ),
        ] {
            assert_eq!(
                Args::try_parse_from(args).unwrap().startup_paths(),
                (path.map(PathBuf::from), video.map(PathBuf::from))
            );
        }
    }

    #[test]
    fn rejects_project_conflicts_in_either_order() {
        for option in ["--fit-gpx", "--video"] {
            for args in [
                vec!["rlv-forge", "--gpxproj", "ride.gpxproj", option, "file"],
                vec!["rlv-forge", option, "file", "--gpxproj", "ride.gpxproj"],
            ] {
                assert_eq!(
                    Args::try_parse_from(args).unwrap_err().kind(),
                    ErrorKind::ArgumentConflict
                );
            }
        }
    }

    #[test]
    fn rejects_missing_values_and_unknown_options() {
        for option in ["--gpxproj", "--fit-gpx", "--video"] {
            assert_eq!(
                Args::try_parse_from(["rlv-forge", option])
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidValue
            );
        }
        assert_eq!(
            Args::try_parse_from(["rlv-forge", "--unknown"])
                .unwrap_err()
                .kind(),
            ErrorKind::UnknownArgument
        );
    }
}
