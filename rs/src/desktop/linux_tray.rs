use super::{icon_rgba, state, Worker, ICON_SIZE};
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use std::process::{Command, Stdio};

pub(super) struct PanelIcon(Handle<StableAlmTray>);

struct StableAlmTray {
    colour: &'static str,
    status: String,
}

impl StableAlmTray {
    fn open_dashboard() {
        std::thread::spawn(|| {
            match Command::new("xdg-open")
                .arg(super::dashboard_url())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(status) if status.success() => {}
                Ok(status) => crate::warn!("desktop", "could not open the dashboard: {status}"),
                Err(e) => crate::warn!("desktop", "could not open the dashboard: {e}"),
            }
        });
    }
}

impl ksni::Tray for StableAlmTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "stable-alm".into()
    }

    fn title(&self) -> String {
        "Stable ALM".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let mut pixels = icon_rgba(self.colour);
        // Status Notifier Item expects bytes in ARGB order.
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.rotate_right(1);
        }
        vec![ksni::Icon {
            width: ICON_SIZE as i32,
            height: ICON_SIZE as i32,
            data: pixels,
        }]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: self.title(), description: self.status.clone(), ..Default::default() }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open dashboard".into(),
                activate: Box::new(|_| Self::open_dashboard()),
                ..Default::default()
            }.into(),
            StandardItem { label: self.status.clone(), enabled: false, ..Default::default() }.into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|_| super::request_quit_local()),
                ..Default::default()
            }.into(),
        ]
    }
}

pub(super) fn start() -> Option<PanelIcon> {
    let tray = StableAlmTray { colour: "grey", status: "Starting…".into() };
    match tray.assume_sni_available(true).spawn() {
        Ok(handle) => Some(PanelIcon(handle)),
        Err(e) => {
            crate::warn!("desktop", "could not start the panel icon: {e}");
            None
        }
    }
}

impl PanelIcon {
    pub(super) fn refresh(&self, worker: &Worker, thread_alive: bool, quitting: bool) {
        let (colour, status) = state(worker, thread_alive, quitting);
        self.0.update(|tray| {
            tray.colour = colour;
            tray.status = status;
        });
    }
}
