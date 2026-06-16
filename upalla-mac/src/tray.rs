use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::sel;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSControlStateValueOn, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSSize, NSString};

use crate::icon;

#[derive(Debug)]
pub struct Tray {
    pub item: Retained<NSStatusItem>,
    pub enabled_item: Retained<NSMenuItem>,
}

pub fn create_tray(mtm: MainThreadMarker, target: &AnyObject) -> Tray {
    let bar = NSStatusBar::systemStatusBar();
    let item = bar.statusItemWithLength(NSVariableStatusItemLength);
    item.setVisible(true);

    let button = item.button(mtm).expect("status item must have button");
    let image = icon::tray_image();
    image.setSize(NSSize::new(18.0, 18.0));
    button.setImage(Some(&image));

    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    item.setMenu(Some(&menu));

    let show_item = make_item(mtm, "Show Upalla", target, sel!(showWindow:));
    menu.addItem(&show_item);

    let enabled_item = make_item(mtm, "Enabled", target, sel!(toggleEnabled:));
    enabled_item.setState(NSControlStateValueOn);
    menu.addItem(&enabled_item);

    let sep = NSMenuItem::separatorItem(mtm);
    menu.addItem(&sep);

    let quit_item = make_item(mtm, "Quit Upalla", target, sel!(quitApp:));
    menu.addItem(&quit_item);

    Tray {
        item,
        enabled_item,
    }
}

fn make_item(mtm: MainThreadMarker, title: &str, target: &AnyObject, action: Sel) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(""),
        )
    };
    unsafe { item.setTarget(Some(target)) };
    item
}
