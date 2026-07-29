#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::schema::{ErrorBody, ResponseResult, RunStartParams};
use crate::app::App;

use super::responses;

impl App {
    pub(super) fn handle_run_start(&mut self, id: String, params: RunStartParams) -> String {
        let caller = match self.agent_info_for_target(&params.caller_pane) {
            Ok(caller) => caller,
            Err(err) => return responses::encode_error_body(id, self.agent_target_error_body(err)),
        };
        let caller_agent = match crate::app::msg::mailbox_agent_name(&caller) {
            Some(caller_agent) => caller_agent,
            None => {
                return responses::encode_error(
                    id,
                    "agent_not_found",
                    "caller has no mailbox identity",
                )
            }
        };
        let caller_pane = caller.global_pane_id;
        let cwd = std::path::PathBuf::from(&params.cwd);
        if !cwd.is_absolute() {
            return responses::encode_error(id, "invalid_cwd", "--cwd must be an absolute path");
        }
        if !cwd.is_dir() {
            return responses::encode_error(
                id,
                "invalid_cwd",
                format!("--cwd is not a directory: {}", cwd.display()),
            );
        }
        if params.label.trim().is_empty() {
            return responses::encode_error(id, "invalid_label", "--label must not be empty");
        }
        if params
            .argv
            .first()
            .is_none_or(|program| program.trim().is_empty())
        {
            return responses::encode_error(id, "invalid_command", "command must not be empty");
        }
        if !matches!(params.completion.as_str(), "summary" | "full" | "none") {
            return responses::encode_error(
                id,
                "invalid_completion",
                "completion must be summary, full, or none",
            );
        }

        let job = crate::job::new_job_id();
        let log_path = crate::session::data_dir()
            .join("job-logs")
            .join(format!("{job}.log"));
        let record = crate::job::JobRecord {
            id: job.clone(),
            label: params.label.clone(),
            command: params
                .argv
                .iter()
                .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
                .collect::<Vec<_>>()
                .join(" "),
            cwd: cwd.display().to_string(),
            caller_pane: caller_pane.clone(),
            caller_agent,
            completion: params.completion.clone(),
            status: "queued".into(),
            runner_pid: None,
            exit_code: None,
            started_unix_ms: None,
            finished_unix_ms: None,
            log_path: log_path.display().to_string(),
        };
        let store = match crate::job::JobStore::open_active() {
            Ok(store) => store,
            Err(err) => {
                return responses::encode_error_body(
                    id,
                    ErrorBody {
                        code: "job_store_unavailable".into(),
                        message: err.to_string(),
                    },
                )
            }
        };
        if let Err(err) = store.insert(&record) {
            return responses::encode_error_body(
                id,
                ErrorBody {
                    code: "job_start_failed".into(),
                    message: err.to_string(),
                },
            );
        }

        let mut runner = match std::env::current_exe() {
            Ok(exe) => Command::new(exe),
            Err(err) => {
                let finished_unix_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let _ = store.mark_start_failed(&job, 127, finished_unix_ms);
                return responses::encode_error_body(
                    id,
                    ErrorBody {
                        code: "runner_spawn_failed".into(),
                        message: err.to_string(),
                    },
                );
            }
        };
        runner
            .arg("__background-run")
            .arg("--job-id")
            .arg(&job)
            .arg("--completion")
            .arg(&params.completion)
            .arg("--")
            .args(&params.argv)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        unsafe {
            runner.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        if let Err(err) = runner.spawn() {
            let finished_unix_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let _ = store.mark_start_failed(&job, 127, finished_unix_ms);
            return responses::encode_error_body(
                id,
                ErrorBody {
                    code: "runner_spawn_failed".into(),
                    message: err.to_string(),
                },
            );
        }

        responses::encode_success(
            id,
            ResponseResult::RunStarted {
                job,
                label: record.label,
                mode: "background".into(),
            },
        )
    }
}
