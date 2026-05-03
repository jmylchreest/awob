fn main() {
    println!("== freedesktop_icons crate ==");
    for name in [
        "audio-volume-high",
        "audio-volume-high-symbolic",
        "display-brightness",
        "battery",
    ] {
        let r = freedesktop_icons::lookup(name).with_size(22).find();
        println!("  {:35} -> {:?}", name, r);
    }
    println!("\n== awob_core::paths::find_icon_file ==");
    for name in [
        "audio-volume-high",
        "audio-volume-muted",
        "display-brightness",
        "battery",
        "microphone-disabled",
        "input-keyboard",
    ] {
        let r = awob_core::paths::find_icon_file(name, 22);
        println!("  {:35} -> {:?}", name, r);
    }
}
