use std::fmt;
use std::path::Path;

use crate::api::schema::{ErrorBody, ErrorResponse};

#[derive(Debug)]
pub(super) struct ServerNotRunningReported {
    pub(super) response: ErrorResponse,
}

impl fmt::Display for ServerNotRunningReported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.response.error.message)
    }
}

impl std::error::Error for ServerNotRunningReported {}

pub(super) fn response(request_id: &str, socket_path: &Path) -> ErrorResponse {
    let shell = native_recovery_shell();
    let attach_command = startup_command(socket_path, shell);
    let socket = socket_path.display().to_string();
    ErrorResponse {
        id: request_id.to_string(),
        error: ErrorBody {
            code: "server_not_running".into(),
            message: format_recovery_message(&socket, &attach_command, shell),
        },
    }
}

fn startup_command(socket_path: &Path, shell: RecoveryShell) -> String {
    let session_name = crate::session::active_name();
    let session_socket = crate::session::api_socket_path_for(session_name.as_deref());
    format_recovery_command(
        &executable_path(),
        session_name.as_deref(),
        socket_path == session_socket,
        shell,
    )
}

fn executable_path() -> String {
    std::env::args_os()
        .next()
        .filter(|path| !path.is_empty())
        .or_else(|| std::env::current_exe().ok().map(Into::into))
        .map(|path| Path::new(&path).display().to_string())
        .unwrap_or_else(|| "<current executable>".to_string())
}

#[derive(Clone, Copy)]
enum RecoveryShell {
    Posix,
    PowerShell,
}

fn native_recovery_shell() -> RecoveryShell {
    if cfg!(windows) {
        RecoveryShell::PowerShell
    } else {
        RecoveryShell::Posix
    }
}

fn format_recovery_command(
    executable: &str,
    session_name: Option<&str>,
    socket_is_session_socket: bool,
    shell: RecoveryShell,
) -> String {
    match shell {
        RecoveryShell::Posix => {
            let command = posix_shell_word(executable);
            match session_name.filter(|_| socket_is_session_socket) {
                Some(name) => format!("{command} session attach {name}"),
                None => command,
            }
        }
        RecoveryShell::PowerShell => {
            let command = format!("& {}", powershell_single_quoted(executable));
            match session_name.filter(|_| socket_is_session_socket) {
                Some(name) => format!(
                    "{command} session attach {}",
                    powershell_single_quoted(name)
                ),
                None => command,
            }
        }
    }
}

fn format_recovery_message(socket: &str, command: &str, shell: RecoveryShell) -> String {
    match shell {
        RecoveryShell::Posix => {
            format!("no herdr server is running at {socket}; run `{command}` to start or attach it")
        }
        RecoveryShell::PowerShell => format!(
            "no herdr server is running at {socket}; run this in PowerShell: `{command}` to start or attach it"
        ),
    }
}

fn posix_shell_word(value: &str) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
                )
        })
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn powershell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(super) fn reported_error(response: ErrorResponse) -> std::io::Error {
    std::io::Error::other(ServerNotRunningReported { response })
}

pub(super) fn was_reported(err: &std::io::Error) -> bool {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<ServerNotRunningReported>())
        .is_some()
}

pub(super) fn reported_response(err: &std::io::Error) -> Option<&ErrorResponse> {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<ServerNotRunningReported>())
        .map(|reported| &reported.response)
}

#[cfg(test)]
mod tests {
    use super::{format_recovery_command, format_recovery_message, RecoveryShell};

    #[test]
    fn formats_recovery_commands_for_posix_and_powershell() {
        let posix_command = format_recovery_command(
            "/tmp/herdr next's",
            Some("team-1"),
            true,
            RecoveryShell::Posix,
        );
        assert_eq!(
            posix_command,
            "'/tmp/herdr next'\\''s' session attach team-1"
        );
        assert_eq!(
            format_recovery_message("/tmp/herdr.sock", &posix_command, RecoveryShell::Posix),
            "no herdr server is running at /tmp/herdr.sock; run `'/tmp/herdr next'\\''s' session attach team-1` to start or attach it"
        );
        let powershell_command = format_recovery_command(
            r"C:\Program Files\100%'\herdr.exe",
            Some("team-1"),
            true,
            RecoveryShell::PowerShell,
        );
        assert_eq!(
            powershell_command,
            "& 'C:\\Program Files\\100%''\\herdr.exe' session attach 'team-1'"
        );
        assert_eq!(
            format_recovery_message(
                r"\\.\pipe\herdr",
                &powershell_command,
                RecoveryShell::PowerShell
            ),
            r"no herdr server is running at \\.\pipe\herdr; run this in PowerShell: `& 'C:\Program Files\100%''\herdr.exe' session attach 'team-1'` to start or attach it"
        );
    }
}
