mod terminal;
mod wm;

use eframe::egui;
use terminal::Shell;
use wm::WindowManager;

struct App {
    desktop: WindowManager,
    started: bool,
}
impl Default for App {
    fn default() -> Self {
        Self {
            desktop: WindowManager::new(),
            started: false,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.started {
            // Desktop hosts project windows; each project is its own sandbox.
            self.desktop.add_project(Shell::PowerShell, &ctx);
            self.started = true;
        }

        ui.horizontal(|ui| {
            ui.strong("FOREMAN");
            ui.separator();
            if ui.button("+ project").clicked() {
                self.desktop.add_project(Shell::PowerShell, &ctx);
            }
            ui.separator();
            ui.label("+ terminal in project:");
            if ui.button("powershell").clicked() {
                self.desktop.add_terminal_to_focused(Shell::PowerShell, &ctx);
            }
            if ui.button("cmd").clicked() {
                self.desktop.add_terminal_to_focused(Shell::Cmd, &ctx);
            }
            if ui.button("bash").clicked() {
                self.desktop.add_terminal_to_focused(Shell::Bash, &ctx);
            }
        });
        ui.separator();

        let area = ui.available_rect_before_wrap();
        self.desktop.show(ui, area, true, egui::Id::new("desktop"));

        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native("Foreman", opts, Box::new(|_cc| Ok(Box::new(App::default()))))
}
