use portable_pty::CommandBuilder;

use crate::layout::PaneId;

pub(crate) const HERDR_PANE_ID_ENV_VAR: &str = "HERDR_PANE_ID";

pub(crate) fn apply_pane_env(cmd: &mut CommandBuilder, pane_id: PaneId) {
    cmd.env(crate::api::SOCKET_PATH_ENV_VAR, crate::api::socket_path());
    cmd.env(HERDR_PANE_ID_ENV_VAR, format!("p_{}", pane_id.raw()));
}
