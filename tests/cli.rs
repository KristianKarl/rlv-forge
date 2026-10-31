use std::process::Command;

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rlv-forge"));
    command.env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY");
    command
}

#[test]
fn help_and_version_exit_without_a_graphical_desktop() {
    let help = command().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let text = String::from_utf8(help.stdout).unwrap();
    for option in ["--help", "--version", "--gpxproj", "--fit-gpx", "--video"] {
        assert!(text.contains(option), "Missing {option} in help: {text}");
    }

    let version = command().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(version.stderr.is_empty());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        format!(
            "rlv-forge {} ({})\n",
            env!("CARGO_PKG_VERSION"),
            env!("RLV_FORGE_GIT_SHA")
        )
    );
}

#[test]
fn invalid_arguments_exit_with_a_diagnostic_without_opening_the_gui() {
    for args in [
        vec!["--gpxproj", "ride.gpxproj", "--fit-gpx", "ride.gpx"],
        vec!["--video", "ride.mp4", "--gpxproj", "ride.gpxproj"],
        vec!["--fit-gpx"],
        vec!["--video"],
        vec!["--gpxproj"],
        vec!["--unknown"],
    ] {
        let output = command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("error:"), "{error}");
        assert!(error.contains("--help"), "{error}");
    }
}
