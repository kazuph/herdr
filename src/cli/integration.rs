pub(super) fn run_integration_command(_args: &[String]) -> std::io::Result<i32> {
    eprintln!(
        "agent integration commands are disabled in the kazuph/herdr fork; use trusted local tools to report agent state"
    );
    Ok(1)
}
