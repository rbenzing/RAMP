use crate::events::Event;
use crate::logger::SharedLog;
use crate::state::{AppState, Service, ServiceState};
use crossbeam_channel::Sender;
use eframe::egui;
use std::sync::{Arc, Mutex};

pub struct RampApp {
    state: Arc<Mutex<AppState>>,
    tx: Sender<Event>,
    log: SharedLog,
    show_window_rx: crossbeam_channel::Receiver<()>,
}

impl RampApp {
    pub fn new(
        state: Arc<Mutex<AppState>>,
        tx: Sender<Event>,
        log: SharedLog,
        show_window_rx: crossbeam_channel::Receiver<()>,
    ) -> Self {
        Self {
            state,
            tx,
            log,
            show_window_rx,
        }
    }
}

impl eframe::App for RampApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Show window if tray requested it
        if self.show_window_rx.try_recv().is_ok() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        ctx.request_repaint_after(std::time::Duration::from_secs(1));

        let state = self.state.lock().unwrap().clone();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("RAMP");
            ui.separator();

            service_row(ui, &self.tx, Service::Apache, &state.apache);
            service_row(ui, &self.tx, Service::Mysql, &state.mysql);

            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("Start All").clicked() {
                    let _ = self.tx.send(Event::StartService(Service::Apache));
                    let _ = self.tx.send(Event::StartService(Service::Mysql));
                }
                if ui.button("Stop All").clicked() {
                    let _ = self.tx.send(Event::StopService(Service::Apache));
                    let _ = self.tx.send(Event::StopService(Service::Mysql));
                }
            });

            ui.separator();
            ui.label("Log");

            let lines = self.log.tail(100);
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .max_height(300.0)
                .show(ui, |ui| {
                    for line in &lines {
                        ui.monospace(line);
                    }
                });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.tx.send(Event::ShutdownAll);
    }
}

fn service_row(
    ui: &mut egui::Ui,
    tx: &Sender<Event>,
    svc: Service,
    status: &crate::state::ServiceStatus,
) {
    ui.horizontal(|ui| {
        let (dot_color, _label) = state_indicator(status.state);
        ui.colored_label(dot_color, "●");
        ui.label(format!("{svc}"));
        ui.label(format!("[{}]", status.state));

        if let Some(err) = &status.last_error {
            ui.colored_label(egui::Color32::RED, format!("⚠ {err}"));
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Stop").clicked() {
                let _ = tx.send(Event::StopService(svc));
            }
            if ui.button("Restart").clicked() {
                let _ = tx.send(Event::RestartService(svc));
            }
            if ui.button("Start").clicked() {
                let _ = tx.send(Event::StartService(svc));
            }
        });
    });
}

fn state_indicator(state: ServiceState) -> (egui::Color32, &'static str) {
    match state {
        ServiceState::Running => (egui::Color32::GREEN, "Running"),
        ServiceState::Starting | ServiceState::Stopping => (egui::Color32::YELLOW, "…"),
        ServiceState::Crashed | ServiceState::Error => (egui::Color32::RED, "!"),
        ServiceState::Stopped => (egui::Color32::GRAY, "Stopped"),
    }
}
