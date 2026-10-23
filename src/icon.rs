//! The window icon (the game's car on a hill, `icon/icon.png`), at the three sizes miniquad
//! takes: 16, 32 and 64 px as raw RGBA, made from the PNG with ImageMagick:
//! `magick icon/icon.png -filter Box -resize 64x64 -depth 8 rgba:icon/64.rgba` (16, 32 alike).
//! macOS uses the 64 px one in the dock; Windows and X11 the window and taskbar.

use macroquad::miniquad::conf::Icon;

pub fn icon() -> Icon {
    Icon {
        small: *include_bytes!("../icon/16.rgba"),
        medium: *include_bytes!("../icon/32.rgba"),
        big: *include_bytes!("../icon/64.rgba"),
    }
}
