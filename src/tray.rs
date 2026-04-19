use crate::events::Event;
use crate::state::Service;
use crossbeam_channel::Sender;
use tray_item::{IconSource, TrayItem};

pub fn run_tray(tx: Sender<Event>, show_window_tx: crossbeam_channel::Sender<()>) {
    let mut tray = match TrayItem::new("RAMP", IconSource::Resource("icon")) {
        Ok(t) => t,
        Err(e) => {
            log::error!("failed to create system tray: {e}");
            return;
        }
    };

    let show_tx = show_window_tx.clone();
    tray.add_menu_item("Open RAMP", move || {
        let _ = show_tx.send(());
    })
    .ok();

    tray.add_label("─────────────").ok();

    let tx2 = tx.clone();
    tray.add_menu_item("Start All", move || {
        let _ = tx2.send(Event::StartService(Service::Apache));
        let _ = tx2.send(Event::StartService(Service::Mysql));
    })
    .ok();

    let tx3 = tx.clone();
    tray.add_menu_item("Stop All", move || {
        let _ = tx3.send(Event::StopService(Service::Apache));
        let _ = tx3.send(Event::StopService(Service::Mysql));
    })
    .ok();

    tray.add_label("─────────────").ok();

    let tx4 = tx.clone();
    tray.add_menu_item("Exit", move || {
        let _ = tx4.send(Event::ShutdownAll);
    })
    .ok();

    // tray-item's inner loop — blocks until the tray is destroyed
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
