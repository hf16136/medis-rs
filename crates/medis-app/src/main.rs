//! Medis: native egui front end.

mod app;
mod instance;
#[cfg(test)]
mod shots;
#[cfg(test)]
mod smoke_tests;
mod ui;

fn main() -> eframe::Result {
    medis_core::install_crypto_provider();

    // One runtime for every connection of this process; the app owns it.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("medis-worker")
        .build()
        .expect("could not start the async runtime");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Medis")
            .with_app_id("medis")
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([840.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Medis",
        options,
        Box::new(move |cc| Ok(Box::new(app::MedisApp::new(cc, rt)))),
    )
}
