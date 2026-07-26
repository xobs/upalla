const ICON_64_PNG: &[u8] = include_bytes!("icon_64.png");

#[cfg(target_os = "macos")]
pub fn tray_image() -> objc2::rc::Retained<objc2_app_kit::NSImage> {
    use objc2::AnyThread;
    use objc2_app_kit::NSImage;
    use objc2_foundation::NSData;

    let data = NSData::from_vec(ICON_64_PNG.to_vec());
    NSImage::initWithData(NSImage::alloc(), &data).expect("failed to create NSImage from PNG")
}

