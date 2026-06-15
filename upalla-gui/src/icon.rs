
static ICON_64_PNG: &[u8] = include_bytes!("icon_64.png");
static ICON_256_PNG: &[u8] = include_bytes!("icon_256.png");

pub fn tray_rgba() -> Vec<u8> {
    let img = image::load_from_memory(ICON_64_PNG).expect("icon_64.png");
    img.to_rgba8().into_raw()
}

pub fn window_icon() -> egui::IconData {
    let img = image::load_from_memory(ICON_256_PNG).expect("icon_256.png");
    let w = img.width();
    let h = img.height();
    egui::IconData {
        rgba: img.to_rgba8().into_raw(),
        width: w,
        height: h,
    }
}
