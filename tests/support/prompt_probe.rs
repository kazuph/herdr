use std::path::{Path, PathBuf};

pub fn compile(directory: &Path) -> PathBuf {
    let executable = directory.join("prompt-probe");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/prompt_probe.c");
    let output = std::process::Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("the C compiler required to build Herdr must be available to build its PTY probe");
    assert!(
        output.status.success(),
        "PTY probe compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}
