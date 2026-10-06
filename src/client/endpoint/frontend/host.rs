use super::super::runtime::QualifiedHostEffect;
use super::*;

pub(super) struct HostPresentation {
    preference: bool,
    pub(super) mouse_capture: Arc<AtomicBool>,
    pub(super) pixel_mouse: Arc<AtomicBool>,
    report_all: bool,
    endpoint_mouse_capture: bool,
}
impl HostPresentation {
    pub(super) fn new(preference: bool) -> Self {
        Self {
            preference,
            mouse_capture: Arc::new(AtomicBool::new(preference)),
            pixel_mouse: Arc::new(AtomicBool::new(false)),
            report_all: false,
            endpoint_mouse_capture: false,
        }
    }

    pub(super) fn clear(&mut self) -> io::Result<()> {
        self.endpoint_mouse_capture = false;
        self.mouse(self.preference, false)?;
        self.keyboard(false)?;
        super::super::super::write_window_title(None);
        Ok(())
    }

    pub(super) fn set_preference(&mut self, preference: bool) -> io::Result<()> {
        self.preference = preference;
        self.mouse(
            preference || self.endpoint_mouse_capture,
            self.pixel_mouse.load(Ordering::Acquire),
        )
    }

    fn mouse(&mut self, enabled: bool, pixels: bool) -> io::Result<()> {
        if self.mouse_capture.load(Ordering::Acquire) != enabled
            || self.pixel_mouse.load(Ordering::Acquire) != pixels
        {
            super::super::super::set_mouse_capture(enabled, pixels)?;
        }
        self.mouse_capture.store(enabled, Ordering::Release);
        self.pixel_mouse.store(pixels, Ordering::Release);
        Ok(())
    }

    fn keyboard(&mut self, enabled: bool) -> io::Result<()> {
        if self.report_all != enabled {
            crate::terminal_modes::set_host_kitty_keyboard_report_all(&mut io::stdout(), enabled)?;
            self.report_all = enabled;
        }
        Ok(())
    }

    pub(super) fn apply(
        &mut self,
        runtime: &EndpointRuntime,
        effect: QualifiedHostEffect,
    ) -> io::Result<()> {
        if !runtime
            .endpoints
            .accepts(&effect.endpoint_id, effect.generation)
            || runtime.endpoints.active_id() != &effect.endpoint_id
        {
            return Ok(());
        }
        match effect.message {
            wire::ServerMessage::MouseCapture {
                enabled,
                sgr_pixels,
            } => {
                self.endpoint_mouse_capture = enabled;
                self.mouse(self.preference || enabled, sgr_pixels)?;
            }
            wire::ServerMessage::ClientShellKeyboardReportAll { enabled } => {
                self.keyboard(enabled)?
            }
            wire::ServerMessage::WindowTitle { title } => {
                super::super::super::write_window_title(title.as_deref())
            }
            wire::ServerMessage::Clipboard { data } => {
                super::super::super::forward_clipboard(&data)
            }
            wire::ServerMessage::ClientShellError { message } => {
                tracing::warn!(%message, "endpoint error")
            }
            _ => {}
        }
        Ok(())
    }
}
