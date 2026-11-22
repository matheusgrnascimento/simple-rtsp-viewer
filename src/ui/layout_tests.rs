//! Layout tests: the whole interface is drawn without a window, at many window
//! sizes, display scales and numbers of cameras, and every component's
//! rectangle (recorded by the `probe!` calls) is checked: nothing overlaps
//! anything else, everything stays inside its own area, and the sidebar and
//! the content never share space.
//!
//! These guard the bug class the redesign removed (controls drawn over each
//! other or over the video) without needing a screen.

use egui::{pos2, vec2, Rect, Vec2};
use tokio::sync::{mpsc, watch};

use super::*;
use crate::store::{CameraAuth, ScanSettings, StoreData, StoredCamera};

/// Slack for float noise when comparing rectangles.
const EPS: f32 = 0.51;

/// Window sizes, in points: the smallest the app allows and common ones.
const WINDOW_SIZES: [(f32, f32); 4] = [
    (720.0, 480.0),
    (1024.0, 600.0),
    (1280.0, 760.0),
    (1920.0, 1080.0),
];
/// Display scales: 100%, 150%, 200%.
const SCALES: [f32; 3] = [1.0, 1.5, 2.0];
/// Numbers of cameras: none, one, an incomplete grid, a prime, a full wall.
const CAMERA_COUNTS: [usize; 5] = [0, 1, 5, 13, 64];

fn stored_camera(index: usize, name_length: usize) -> StoredCamera {
    StoredCamera {
        id: format!("camera-{index}"),
        name: Some(format!("{} {index}", "Camera".repeat(name_length.max(1)))),
        // 192.0.2.0/24 is reserved for documentation (RFC 5737) and never
        // routed: even if a stream were opened by mistake it could not reach a
        // real device. These tests do not open any (see `app_with_cameras`).
        host: format!("192.0.2.{}", 1 + index % 250),
        port: 554,
        main_path: format!("/stream{index}"),
        sub_path: Some(format!("/sub{index}")),
        auth: CameraAuth::Shared,
    }
}

/// An app with `count` cameras laid out, WITHOUT opening any stream: the app
/// is built with an empty library (so `new` has nothing to connect to) and the
/// cameras are set afterwards, directly, bypassing `rebuild_cameras`. These
/// tests are about layout; they must never touch the network.
fn app_with_cameras(count: usize, name_length: usize) -> SimpleRtspViewerApp {
    let handle = StreamManagerHandle::new();
    let library = CameraLibrary {
        path: std::env::temp_dir().join("layout_tests_nao_usado.json"),
        data: StoreData {
            cameras: Vec::new(),
            scan: ScanSettings::default(),
        },
        shared: Some(crate::credentials::Credentials {
            username: "user".to_owned(),
            password: "password".to_owned(),
        }),
        own: HashMap::new(),
    };
    let mut app = SimpleRtspViewerApp::new(library, handle);
    app.audio.open = || Ok(AudioOutput::detached(48_000));

    app.library.data.cameras = (0..count).map(|i| stored_camera(i, name_length)).collect();
    app.cameras = app.library.cameras();
    for camera in &app.cameras {
        app.statuses
            .insert(camera.id.clone(), ConnectionStatus::Reconnecting);
    }
    assert!(app.streams.is_empty(), "layout tests must not open streams");
    app
}

/// Gives camera `id` a stream entry that reports audio, so the sound controls
/// show. The returned senders must stay alive while the app is drawn.
fn give_audio(
    app: &mut SimpleRtspViewerApp,
    id: &str,
) -> (watch::Sender<bool>, mpsc::Sender<CameraFrame>) {
    let (has_audio_tx, has_audio_rx) = watch::channel(true);
    let (frame_tx, frame_rx) = mpsc::channel(1);
    let (_status_tx, status_rx) = watch::channel(ConnectionStatus::Connected);
    let (display_tx, _display_rx) = watch::channel(None);
    let (audio_tx, _audio_rx) = watch::channel(None);
    app.streams.insert(
        id.to_owned(),
        ActiveStream {
            url: "rtsp://127.0.0.1:1/x".to_owned(),
            frames: frame_rx,
            status: status_rx,
            display_size: display_tx,
            audio: audio_tx,
            has_audio: has_audio_rx,
        },
    );
    (has_audio_tx, frame_tx)
}

/// Draws a few frames at `window` size and `ppp` scale and returns the
/// rectangles recorded in the last one.
fn draw(
    app: &mut SimpleRtspViewerApp,
    ctx: &egui::Context,
    window: Vec2,
    ppp: f32,
) -> Vec<(String, Rect)> {
    ctx.set_pixels_per_point(ppp);
    let mut recorded = Vec::new();
    // Several frames: egui settles sizes (scroll areas, panels) over a couple of
    // passes, and the last one is what the user keeps seeing.
    for _ in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), window)),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            app.ui(ctx);
        });
        recorded = app.probe.take();
    }
    recorded
}

/// A context with the app's theme and fonts applied, as at startup.
fn themed_context(app: &mut SimpleRtspViewerApp, dark: bool) -> egui::Context {
    let ctx = egui::Context::default();
    app.look.icons = theme::install_fonts(&ctx);
    app.apply_theme(&ctx, dark);
    ctx
}

fn named<'a>(rects: &'a [(String, Rect)], name: &str) -> Option<&'a Rect> {
    rects.iter().find(|(n, _)| n == name).map(|(_, r)| r)
}

fn overlap(a: Rect, b: Rect) -> f32 {
    let i = a.intersect(b);
    if i.width() > 0.0 && i.height() > 0.0 {
        i.area()
    } else {
        0.0
    }
}

/// `inner` lies inside `outer` (with float slack) on the x axis.
fn within_x(inner: Rect, outer: Rect) -> bool {
    inner.min.x >= outer.min.x - EPS && inner.max.x <= outer.max.x + EPS
}

fn within(inner: Rect, outer: Rect) -> bool {
    within_x(inner, outer) && inner.min.y >= outer.min.y - EPS && inner.max.y <= outer.max.y + EPS
}

/// Every check on one frame. `context` describes the scenario for the message.
fn check_layout(
    context: &str,
    window: Vec2,
    rects: &[(String, Rect)],
    expected_sidebar_width: f32,
    camera_count: usize,
) {
    let fail = |message: String| panic!("{context}: {message}");
    let screen = Rect::from_min_size(pos2(0.0, 0.0), window);

    // --- Sidebar: exactly the designed width, at the left edge, full height.
    let sidebar = *named(rects, "container:sidebar")
        .unwrap_or_else(|| fail("the sidebar was not drawn".to_owned()));
    if (sidebar.width() - expected_sidebar_width).abs() > EPS {
        fail(format!(
            "sidebar is {} wide, expected {expected_sidebar_width}",
            sidebar.width()
        ));
    }
    if sidebar.min.x.abs() > EPS
        || sidebar.min.y.abs() > EPS
        || (sidebar.max.y - window.y).abs() > 1.0
    {
        fail(format!(
            "sidebar is not flush with the window edges: {sidebar:?}"
        ));
    }

    // --- Content: right of the sidebar, inside the window.
    let content = *named(rects, "container:content")
        .unwrap_or_else(|| fail("the content area was not drawn".to_owned()));
    if content.min.x < sidebar.max.x - EPS {
        fail(format!(
            "content {content:?} overlaps the sidebar {sidebar:?}"
        ));
    }
    if !within(content, screen) {
        fail(format!("content {content:?} leaves the window {screen:?}"));
    }
    if content.width() <= 0.0 || content.height() <= 0.0 {
        fail(format!("content area is empty: {content:?}"));
    }
    if let Some(notice) = named(rects, "notice") {
        if notice.min.x < sidebar.max.x - EPS || notice.max.x > window.x + EPS {
            fail(format!(
                "notice bar {notice:?} is not inside the content column"
            ));
        }
        if content.min.y < notice.max.y - EPS {
            fail(format!(
                "content {content:?} starts above the notice bar {notice:?}"
            ));
        }
    }

    // --- Sidebar components: horizontally inside it, never over each other.
    let leaves: Vec<&(String, Rect)> = rects
        .iter()
        .filter(|(name, _)| name.starts_with("sidebar:"))
        .collect();
    for (name, rect) in &leaves {
        if !within_x(*rect, sidebar) {
            fail(format!(
                "{name} {rect:?} sticks out of the sidebar {sidebar:?}"
            ));
        }
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            fail(format!("{name} has no size: {rect:?}"));
        }
    }
    for (i, (name_a, a)) in leaves.iter().enumerate() {
        for (name_b, b) in &leaves[i + 1..] {
            if overlap(*a, *b) > 0.5 {
                fail(format!("{name_a} {a:?} overlaps {name_b} {b:?}"));
            }
        }
    }
    if let Some(card) = named(rects, "container:sidebar:viewing-card") {
        for (name, rect) in leaves
            .iter()
            .filter(|(n, _)| n.starts_with("sidebar:viewing:"))
        {
            if !within(*rect, *card) {
                fail(format!("{name} {rect:?} is outside its card {card:?}"));
            }
        }
    }
    let camera_rows = leaves
        .iter()
        .filter(|(name, _)| name.starts_with("sidebar:camera:"))
        .count();
    if camera_rows != camera_count {
        fail(format!(
            "{camera_rows} camera rows in the sidebar, expected {camera_count}"
        ));
    }

    // --- Tiles: inside the content, disjoint; overlays inside their tile.
    let tiles: Vec<&(String, Rect)> = rects
        .iter()
        .filter(|(name, _)| name.starts_with("tile:") && name.matches(':').count() == 1)
        .collect();
    // One tile per camera in the grid, a single one when a camera is enlarged.
    let expected_tiles = if named(rects, "tile:enlarged").is_some() {
        1
    } else {
        camera_count
    };
    if tiles.len() != expected_tiles {
        fail(format!(
            "{} tiles drawn, expected {expected_tiles}",
            tiles.len()
        ));
    }
    for (name, rect) in &tiles {
        if !within(*rect, content) {
            fail(format!(
                "{name} {rect:?} leaves the content area {content:?}"
            ));
        }
    }
    for (i, (name_a, a)) in tiles.iter().enumerate() {
        for (name_b, b) in &tiles[i + 1..] {
            if overlap(*a, *b) > 0.01 {
                fail(format!("{name_a} {a:?} overlaps {name_b} {b:?}"));
            }
        }
    }
    for (name, tile) in &tiles {
        let overlays: Vec<&(String, Rect)> = rects
            .iter()
            .filter(|(overlay, _)| overlay.starts_with(&format!("{name}:")))
            .collect();
        for (overlay, rect) in &overlays {
            if !within(*rect, *tile) {
                fail(format!(
                    "{overlay} {rect:?} sticks out of its tile {tile:?}"
                ));
            }
        }
        for (i, (name_a, a)) in overlays.iter().enumerate() {
            for (name_b, b) in &overlays[i + 1..] {
                if overlap(*a, *b) > 0.5 {
                    fail(format!("{name_a} {a:?} overlaps {name_b} {b:?}"));
                }
            }
        }
    }

    // --- Toast: inside the window and the content column, clear of the sidebar.
    if let Some(toast) = named(rects, "toast") {
        if !within(*toast, screen) {
            fail(format!("toast {toast:?} leaves the window"));
        }
        if toast.min.x < sidebar.max.x - EPS {
            fail(format!("toast {toast:?} covers the sidebar {sidebar:?}"));
        }
    }
}

/// Runs `check_layout` over every window size and scale for one scenario.
fn check_everywhere(
    name: &str,
    camera_count: usize,
    name_length: usize,
    collapsed: bool,
    scenario: impl Fn(&mut SimpleRtspViewerApp),
) {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = runtime.enter();

    for dark in [true, false] {
        for &(width, height) in &WINDOW_SIZES {
            for &ppp in &SCALES {
                let mut app = app_with_cameras(camera_count, name_length);
                app.sidebar_collapsed = collapsed;
                let ctx = themed_context(&mut app, dark);
                scenario(&mut app);
                let rects = draw(&mut app, &ctx, vec2(width, height), ppp);
                let expected = if collapsed {
                    sidebar::COLLAPSED_WIDTH
                } else {
                    sidebar::EXPANDED_WIDTH
                };
                check_layout(
                    &format!(
                        "{name}, {camera_count} cameras, {width}x{height} at {ppp}x, {}",
                        if dark { "dark" } else { "light" }
                    ),
                    vec2(width, height),
                    &rects,
                    expected,
                    camera_count,
                );
            }
        }
    }
    drop(_guard);
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn grid_layout_has_no_overlaps_at_any_size() {
    for count in CAMERA_COUNTS {
        check_everywhere("grid", count, 1, false, |_| {});
    }
}

#[test]
fn collapsed_sidebar_layout_has_no_overlaps_at_any_size() {
    for count in CAMERA_COUNTS {
        check_everywhere("collapsed grid", count, 1, true, |_| {});
    }
}

#[test]
fn enlarged_camera_layout_has_no_overlaps_at_any_size() {
    for collapsed in [false, true] {
        check_everywhere("enlarged", 5, 1, collapsed, |app| {
            app.enlarged = Some("camera-0".to_owned());
            app.enlarged_since = Some(Instant::now());
        });
    }
}

#[test]
fn enlarged_camera_with_every_control_shown_has_no_overlaps() {
    // High resolution on, sound controls shown (the stream reports audio), and
    // a camera that is also marked disconnected so its status pill competes
    // with the name label for room.
    for collapsed in [false, true] {
        let senders = std::cell::RefCell::new(Vec::new());
        check_everywhere("enlarged with sound", 3, 1, collapsed, |app| {
            app.enlarged = Some("camera-1".to_owned());
            app.enlarged_since = Some(Instant::now());
            app.high_resolution = true;
            senders.borrow_mut().push(give_audio(app, "camera-1"));
            app.statuses
                .insert("camera-1".to_owned(), ConnectionStatus::Disconnected);
        });
    }
}

#[test]
fn very_long_names_errors_and_toasts_do_not_break_the_layout() {
    // A 150-character camera name, a long error in the notice bar and a long
    // toast, all at once, in the smallest window too.
    check_everywhere("long texts", 5, 25, false, |app| {
        app.enlarged = Some("camera-2".to_owned());
        app.enlarged_since = Some(Instant::now());
        app.show_notice(NoticeKind::Error, "The login was not saved: ".repeat(14));
        app.show_notice(
            NoticeKind::Success,
            "Imported many cameras at once. ".repeat(8),
        );
    });
    check_everywhere("long texts, grid", 13, 25, false, |app| {
        app.show_notice(NoticeKind::Error, "Gerenciador de Credenciais: ".repeat(12));
        app.show_notice(NoticeKind::Success, "Done. ".repeat(40));
    });
}

#[test]
fn missing_login_bar_does_not_overlap_the_content() {
    check_everywhere("missing login", 5, 1, false, |app| {
        app.library.shared = None;
    });
}

#[test]
fn dialogs_do_not_change_the_layout_behind_them() {
    // With a dialog open the sidebar and the grid are drawn the same (inert),
    // so the layout checks still hold.
    check_everywhere("dialog open", 5, 1, false, |app| {
        app.dialog = Some(Dialog::ConfirmRemove {
            id: "camera-0".to_owned(),
            label: "Camera 0".to_owned(),
        });
    });
}

#[test]
fn the_grid_fills_the_content_area_with_the_best_fit() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = runtime.enter();

    // 5 cameras: the grid follows `best_grid` for the content area, the tiles
    // have equal sizes and the incomplete last row is centered.
    let mut app = app_with_cameras(5, 1);
    let ctx = themed_context(&mut app, true);
    let rects = draw(&mut app, &ctx, vec2(1600.0, 900.0), 1.0);
    let tile = |i: usize| *named(&rects, &format!("tile:{i}")).expect("tile drawn");
    let content = *named(&rects, "container:content").unwrap();

    let (columns, rows) = grid::best_grid(5, content.width(), content.height(), GAP);
    assert!(columns * rows >= 5);
    // Reading order: the first row has `columns` tiles at the same height.
    for i in 1..columns.min(5) {
        assert!(
            (tile(i).min.y - tile(0).min.y).abs() < EPS,
            "tile {i} is not in the first row"
        );
        assert!(tile(i - 1).max.x <= tile(i).min.x + EPS);
    }
    // The next row starts below the first one.
    if rows > 1 {
        assert!(tile(columns).min.y >= tile(0).max.y);
    }
    // Equal sizes (to the pixel).
    assert!((tile(0).width() - tile(4).width()).abs() <= 1.0);
    assert!((tile(0).height() - tile(4).height()).abs() <= 1.0);
    // The last row is centered under the first one.
    let last_row_first = columns * (rows - 1);
    let in_last_row = 5 - last_row_first;
    let first_row_center = (tile(0).min.x + tile(columns.min(5) - 1).max.x) / 2.0;
    let last_row_center = (tile(last_row_first).min.x + tile(4).max.x) / 2.0;
    assert!(
        (first_row_center - last_row_center).abs() <= 1.0,
        "{in_last_row} tiles in the last row are not centered: {first_row_center} vs {last_row_center}"
    );

    drop(_guard);
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn collapsing_the_sidebar_gives_its_space_to_the_video() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = runtime.enter();

    let mut app = app_with_cameras(4, 1);
    let ctx = themed_context(&mut app, true);
    let expanded = draw(&mut app, &ctx, vec2(1280.0, 760.0), 1.0);
    app.sidebar_collapsed = true;
    let collapsed = draw(&mut app, &ctx, vec2(1280.0, 760.0), 1.0);

    let expanded_content = *named(&expanded, "container:content").unwrap();
    let collapsed_content = *named(&collapsed, "container:content").unwrap();
    let gained = sidebar::EXPANDED_WIDTH - sidebar::COLLAPSED_WIDTH;
    assert!(
        (collapsed_content.width() - expanded_content.width() - gained).abs() <= 1.0,
        "{} -> {}",
        expanded_content.width(),
        collapsed_content.width()
    );

    drop(_guard);
    runtime.shutdown_timeout(Duration::from_secs(1));
}

// ---------------------------------------------------------------------------
// The checker itself: deliberate overlaps must be caught.
// ---------------------------------------------------------------------------

/// A valid frame by hand: the sidebar, the content with two tiles side by
/// side, and one overlay in each tile.
fn valid_frame() -> Vec<(String, Rect)> {
    let r = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(x, y), vec2(w, h));
    vec![
        ("container:sidebar".to_owned(), r(0.0, 0.0, 296.0, 600.0)),
        ("sidebar:add".to_owned(), r(12.0, 60.0, 272.0, 36.0)),
        ("sidebar:Import".to_owned(), r(12.0, 100.0, 272.0, 36.0)),
        (
            "sidebar:camera:camera-0".to_owned(),
            r(12.0, 200.0, 272.0, 56.0),
        ),
        (
            "sidebar:camera:camera-1".to_owned(),
            r(12.0, 260.0, 272.0, 56.0),
        ),
        ("container:content".to_owned(), r(296.0, 0.0, 704.0, 600.0)),
        ("tile:0".to_owned(), r(304.0, 8.0, 340.0, 584.0)),
        ("tile:0:status".to_owned(), r(374.0, 280.0, 200.0, 32.0)),
        ("tile:0:name".to_owned(), r(312.0, 560.0, 120.0, 24.0)),
        ("tile:1".to_owned(), r(652.0, 8.0, 340.0, 584.0)),
    ]
}

fn check_valid(rects: &[(String, Rect)], camera_count: usize) {
    check_layout(
        "hand-made frame",
        vec2(1000.0, 600.0),
        rects,
        296.0,
        camera_count,
    );
}

#[test]
fn the_checker_accepts_a_valid_frame() {
    check_valid(&valid_frame(), 2);
}

#[test]
#[should_panic(expected = "overlaps")]
fn the_checker_catches_overlapping_sidebar_components() {
    let mut frame = valid_frame();
    // The second camera row slides up over the first one.
    frame[4].1 = frame[4].1.translate(vec2(0.0, -30.0));
    check_valid(&frame, 2);
}

#[test]
#[should_panic(expected = "overlaps")]
fn the_checker_catches_overlapping_tiles() {
    let mut frame = valid_frame();
    frame[9].1 = frame[9].1.translate(vec2(-40.0, 0.0));
    check_valid(&frame, 2);
}

#[test]
#[should_panic(expected = "overlaps")]
fn the_checker_catches_a_name_label_over_the_status_pill() {
    let mut frame = valid_frame();
    frame[8].1 = frame[8].1.translate(vec2(100.0, -260.0));
    check_valid(&frame, 2);
}

#[test]
#[should_panic(expected = "overlaps the sidebar")]
fn the_checker_catches_content_under_the_sidebar() {
    let mut frame = valid_frame();
    frame[5].1 = Rect::from_min_size(pos2(250.0, 0.0), vec2(750.0, 600.0));
    check_valid(&frame, 2);
}

#[test]
#[should_panic(expected = "sticks out of the sidebar")]
fn the_checker_catches_a_component_wider_than_the_sidebar() {
    let mut frame = valid_frame();
    frame[1].1 = Rect::from_min_size(pos2(12.0, 60.0), vec2(400.0, 36.0));
    check_valid(&frame, 2);
}

#[test]
#[should_panic(expected = "covers the sidebar")]
fn the_checker_catches_a_toast_over_the_sidebar() {
    let mut frame = valid_frame();
    frame.push((
        "toast".to_owned(),
        Rect::from_min_size(pos2(100.0, 540.0), vec2(400.0, 40.0)),
    ));
    check_valid(&frame, 2);
}

#[test]
#[should_panic(expected = "expected 3")]
fn the_checker_catches_missing_tiles_and_rows() {
    check_valid(&valid_frame(), 3);
}

/// Manual benchmark: the cost of one frame of the interface (building it and
/// tessellating it into triangles), with 5 and 64 cameras, in the grid and
/// enlarged. Run with:
/// `cargo test --release ui_frame_benchmark -- --ignored --nocapture`
#[test]
#[ignore = "manual benchmark"]
fn ui_frame_benchmark() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = runtime.enter();

    println!(
        "\n  {:<34} {:>10}",
        "scenario (1920x1080, 150%)", "ms / frame"
    );
    for (title, count, enlarged) in [
        ("grid, 5 cameras", 5, false),
        ("grid, 64 cameras", 64, false),
        ("one camera enlarged (of 5)", 5, true),
    ] {
        let mut app = app_with_cameras(count, 1);
        let ctx = themed_context(&mut app, true);
        if enlarged {
            app.enlarged = Some("camera-0".to_owned());
        }
        ctx.set_pixels_per_point(1.5);

        let frame = |app: &mut SimpleRtspViewerApp| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                app.ui(ctx);
            });
            let _ = ctx.tessellate(output.shapes, 1.5);
            app.probe.take();
        };
        for _ in 0..20 {
            frame(&mut app);
        }
        let runs = 200;
        let started = Instant::now();
        for _ in 0..runs {
            frame(&mut app);
        }
        let millis = started.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
        println!("  {title:<34} {millis:>10.2}");
    }

    drop(_guard);
    runtime.shutdown_timeout(Duration::from_secs(1));
}
