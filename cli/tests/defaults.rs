use std::io;
use std::process::Command;

#[test]
fn version_is_offline_and_matches_the_package() -> io::Result<()> {
    for flag in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_jio"))
            .arg(flag)
            .env_remove("JIO_API_KEY")
            .env("JIO_ENDPOINT", "invalid endpoint")
            .output()?;
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            concat!("jio ", env!("CARGO_PKG_VERSION"), "\n").as_bytes()
        );
        assert!(output.stderr.is_empty());
    }
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_jio"))
            .args(["--version", "extra"])
            .output()?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn no_connection_setup_is_required_but_an_api_key_still_is() -> io::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_jio"))
        .arg("usage")
        .env_remove("JIO_ENDPOINT")
        .env_remove("JIO_HOST")
        .env_remove("JIO_CA_CERT")
        .env_remove("JIO_API_KEY")
        .output()?;
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "jio: JIO_API_KEY is required\n"
    );
    assert!(output.stdout.is_empty());
    Ok(())
}
