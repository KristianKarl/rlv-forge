mod map;
mod project;
mod studio;
mod ui;

#[cfg(test)]
fn test_ui(
    ctx: &eframe::egui::Context,
    input: eframe::egui::RawInput,
    ui: impl FnMut(&mut eframe::egui::Ui),
) -> eframe::egui::FullOutput {
    let mut output = ctx.run_ui(input, ui);
    output.textures_delta.clear();
    output
}

fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("RLV Forge")
            .with_inner_size([1180.0, 940.0])
            .with_min_inner_size([760.0, 640.0]),
        ..Default::default()
    };
    let mut args = std::env::args_os().skip(1);
    let path = args.next().map(std::path::PathBuf::from);
    let video = args.next().map(std::path::PathBuf::from);
    eframe::run_native(
        "RLV Forge",
        options,
        Box::new(move |cc| Ok(Box::new(ui::RlvForge::new(cc, path, video)))),
    )
}
