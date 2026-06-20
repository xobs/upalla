static ICON_64_PNG: &[u8] = include_bytes!("icon_64.png");

pub fn tray_icon() -> ksni::Icon {
    let img = image::load_from_memory(ICON_64_PNG).expect("icon_64.png");
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for pixel in rgba.pixels() {
        data.push(pixel[3]); // a
        data.push(pixel[0]); // r
        data.push(pixel[1]); // g
        data.push(pixel[2]); // b
    }
    ksni::Icon {
        width: w as i32,
        height: h as i32,
        data,
    }
}
