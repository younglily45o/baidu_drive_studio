#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

fn main() -> eframe::Result {
    eframe::run_native(
        "Baidu Drive Studio · 百度网盘",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 790.0])
                .with_min_inner_size([960.0, 650.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ui::Studio::new(&cc.egui_ctx)))),
    )
}
