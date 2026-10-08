//! Software-renderer timing and allocation benchmarks.
use awob_core::{
    bindings::{self, Value},
    render::Renderer,
    theme,
};
use divan::{AllocProfiler, Bencher, black_box};
use std::{path::Path, time::Duration};

#[global_allocator]
static ALLOC: AllocProfiler = AllocProfiler::system();

fn main() {
    divan::main();
}

fn setup(name: &str) -> (Renderer, theme::Theme, bindings::Bindings) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../themes")
        .join(name);
    let theme = theme::parse_with_base(
        &std::fs::read_to_string(dir.join("scene.kdl")).unwrap(),
        Some(&dir),
    )
    .unwrap();
    let mut payload = awob_protocol::SendPayload::new("volume", 75.0);
    // Inline icon avoids machine-dependent icon-theme discovery.
    payload.icon = Some("data:image/svg+xml,<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><rect width=\"16\" height=\"16\"/></svg>".into());
    let mut bindings = bindings::build(&payload, Some(25.0), Some(100.0), None);
    bindings.palette = theme.palette.clone();
    theme::apply_style(&theme, &mut bindings, "normal").unwrap();
    bindings.set("transitionProgress", Value::Number(0.5));
    let mut renderer = Renderer::new();
    renderer.set_theme_dir(Some(dir));
    for _ in 0..20 {
        black_box(
            renderer
                .render(&theme, &bindings, Some(Duration::from_millis(500)))
                .unwrap(),
        );
    }
    (renderer, theme, bindings)
}

#[divan::bench(args = ["default", "wob", "console", "minimal"])]
fn warm_frame(bencher: Bencher, name: &str) {
    let (mut renderer, theme, bindings) = setup(name);
    bencher.bench_local(|| {
        black_box(
            renderer
                .render(&theme, &bindings, Some(Duration::from_millis(500)))
                .unwrap(),
        )
    });
}

#[divan::bench(args = ["default", "wob", "console", "minimal"])]
fn warm_cached_frame(bencher: Bencher, name: &str) {
    let (mut renderer, theme, bindings) = setup(name);
    bencher.bench_local(|| {
        black_box(
            renderer
                .render_cached(&theme, &bindings, Some(Duration::from_millis(500)))
                .unwrap(),
        );
    });
}
