//! Debug helper: print where the icon resolver finds common OSD icon
//! names on this machine. Honours $AWOB_ICON_THEME / $GTK_THEME /
//! gsettings, so it answers "why is awob showing THAT icon":
//!
//!   cargo run -p awob-core --example icon_probe
//!   AWOB_ICON_THEME=breeze cargo run -p awob-core --example icon_probe

fn main() {
    for name in [
        "audio-volume-high",
        "audio-volume-muted",
        "display-brightness",
        "battery",
        "battery-low-charging",
        "microphone-disabled",
        "microphone-sensitivity-high",
        "input-keyboard",
        "power-profile-balanced-symbolic",
        "image-missing",
    ] {
        for size in [24u32, 48] {
            let r = awob_core::paths::find_icon_file(name, size);
            println!("  {name:35} @{size:<3} -> {r:?}");
        }
    }
}
