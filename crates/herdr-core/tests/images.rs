#![cfg(unix)] // Windows: tests/windows.rs

//! Kitty images: what a viewer gets however and whenever it arrives.

mod common;

use common::framed::Viewer;
use common::*;
use ghostty_vt::testing::{chunked, noise, placed, placeholder_cells, png_filled, same_image};
use herdr_core::wire;
use serde_json::json;
use std::time::{Duration, Instant};

/// A program that waits for `go` before doing `then`.
fn after_go(s: &Session, then: &str) -> String {
    format!("while [ ! -e {} ]; do sleep 0.02; done; {then}", s.root.join("go").display())
}

fn go(s: &Session) {
    std::fs::write(s.root.join("go"), "").unwrap();
}

/// As `chafa` frames an image: its keys in an empty first command, an empty last one.
fn gfx_framed(control: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = format!("\x1b_G{control},m=1;\x1b\\").into_bytes();
    for c in ghostty_vt::testing::base64(payload).as_bytes().chunks(4096) {
        out.extend(b"\x1b_Gm=1;");
        out.extend_from_slice(c);
        out.extend(b"\x1b\\");
    }
    out.extend(b"\x1b_Gm=0;\x1b\\");
    out
}
fn ids(v: &Viewer) -> Vec<u32> {
    placed(&v.screen).iter().map(|p| p.0).collect()
}
/// A program that prints `bytes` once `go` exists, then DONE, then waits.
fn printer(s: &Session, bytes: &[u8]) -> String {
    let file = s.root.join("emit");
    std::fs::write(&file, bytes).unwrap();
    let flood = after_go(s, &format!("cat {}; printf 'DONE\\r\\n'; sleep 100", file.display()));
    run(s, &["sh", "-c", &flood])
}
/// A program that sends `first`, then `then` once `go` exists, and leaves the terminal raw.
fn script(s: &Session, first: &[u8], then: &[u8]) -> String {
    let (first_file, second_file) = (s.root.join("first"), s.root.join("second"));
    std::fs::write(&first_file, first).unwrap();
    std::fs::write(&second_file, then).unwrap();
    let then = format!("cat {}", second_file.display());
    let program = format!("stty raw -echo; cat {}; {}; cat >/dev/null", first_file.display(), after_go(s, &then));
    run(s, &["sh", "-c", &program])
}
fn printed(s: &Session, terminal: &str, needle: &str) {
    wait_until(needle, || s.json(&["read", terminal])["text"].as_str().unwrap_or("").contains(needle));
}
fn watch(s: &Session, terminal: &str) -> Viewer {
    Viewer::attach(s, terminal, 80, 24, json!({}))
}
fn observe(s: &Session, terminal: &str) -> Viewer {
    Viewer::attach(s, terminal, 80, 24, json!({"observe": true}))
}
/// Gives back all the credit a viewer was holding.
fn grant(v: &mut Viewer) {
    v.credit = true;
    v.conn.send(v.channel, wire::CREDIT, &(64u32 << 20).to_le_bytes());
}
/// Where the fourth command of a chunked image ends.
fn cut_after_four(whole: &[u8]) -> usize {
    whole.windows(2).enumerate().filter(|(_, w)| *w == b"\x1b\\").nth(3).unwrap().0 + 2
}
fn mixed() -> Vec<u8> {
    let mut b = vec![];
    b.extend(b"direct:\r\n");
    b.extend(chunked("a=T,f=100,i=1,c=8,r=2,q=2", &png_filled(16, 8, [255, 0, 0, 255])));
    b.extend(b"\r\nunsized:\r\n");
    b.extend(chunked("a=T,f=100,i=4,q=2", &png_filled(100, 80, [9, 9, 200, 255])));
    b.extend(b"\r\nimplicit: ");
    b.extend(chunked("a=T,f=100,c=2,r=1,C=1,q=2", &png_filled(4, 2, [0, 0, 255, 255])));
    b.extend(b"\r\nplaceholder:\r\n");
    b.extend(chunked("a=T,f=100,U=1,i=2,c=4,r=1,q=2", &png_filled(8, 2, [0, 255, 0, 255])));
    b.extend(placeholder_cells().as_bytes());
    b.extend(b"\r\nrgb:\r\n");
    b.extend(chunked("a=T,f=24,s=2,v=2,i=3,c=2,r=2,q=2", &[9u8; 12]));
    b.extend(b"\r\n");
    b
}

#[test]
fn a_late_viewer_sees_what_an_early_viewer_saw() {
    let s = Session::new();
    let t = printer(&s, &mixed());
    let mut early = watch(&s, &t);
    early.drain(Duration::from_millis(100));
    go(&s);
    early.shows("DONE");
    let mut late = observe(&s, &t);
    late.shows("DONE");
    late.until("its images", |v| placed(&v.screen).len() == 5);
    assert_eq!(placed(&early.screen), placed(&late.screen));
    assert_eq!(early.screen.screen_rows(), late.screen.screen_rows(), "text in the same places");
    for id in [1, 2, 3, 4] {
        let (shown, replayed) = (early.screen.image_pixels(id).unwrap(), late.screen.image_pixels(id).unwrap());
        assert!(same_image(&shown, &replayed), "pixels of {id}");
    }
}

#[test]
fn a_viewer_attached_after_the_program_printed_alone_gets_its_images() {
    let s = Session::new();
    let t = printer(&s, &mixed());
    go(&s);
    printed(&s, &t, "DONE");
    let mut late = watch(&s, &t);
    late.shows("DONE");
    assert_eq!(ids(&late).iter().filter(|id| [1, 2, 3, 4].contains(id)).count(), 4, "{:?}", placed(&late.screen));
}

#[test]
fn a_stalled_viewer_gets_images_back_including_the_one_that_overflowed_it() {
    let s = Session::new();
    let mut b = vec![];
    b.extend(chunked("a=T,f=100,i=1,c=4,r=1,q=2", &png_filled(8, 2, [255, 0, 0, 255])));
    b.extend(b"\r\n");
    b.extend(chunked("a=T,f=100,U=1,i=2,c=4,r=1,q=2", &png_filled(8, 2, [0, 255, 0, 255])));
    b.extend(placeholder_cells().as_bytes());
    b.extend(b"\r\n");
    // 6 MiB of pixels, 8 MiB of base64, in 4 KiB chunks: bigger than any backlog.
    b.extend(chunked("a=T,f=32,s=1024,v=1536,i=9,c=8,r=2,q=2", &vec![7u8; 1024 * 1536 * 4]));
    b.extend(b"\r\n");
    let t = printer(&s, &b);
    let mut stalled = watch(&s, &t);
    stalled.credit = false;
    let mut live = observe(&s, &t);
    go(&s);
    live.shows("DONE");
    live.until("its images", |v| ids(v) == [1, 2, 9]);
    stalled.drain(Duration::from_millis(100));
    grant(&mut stalled);
    stalled.shows("DONE");
    let expected = placed(&live.screen);
    stalled.until("the images", |v| placed(&v.screen) == expected);
    assert!(stalled.received < 8 << 20, "replayed the backlog: {}", stalled.received);
}

#[test]
fn a_slow_but_moving_viewer_ends_with_the_image_that_overflowed_it() {
    let s = Session::new();
    // Bigger than the backlog limit as it arrives, small enough once compressed to replay.
    let mut b = chunked("a=T,f=32,s=1000,v=1500,i=9,c=8,r=2,q=2", &vec![7u8; 6_000_000]);
    b.extend(b"\r\n");
    let t = printer(&s, &b);
    let mut slow = watch(&s, &t);
    slow.credit = false;
    let mut live = observe(&s, &t);
    go(&s);
    // A little credit at a time while the image is still arriving: repaints happen mid-load.
    let started = Instant::now();
    while !live.text().contains("DONE") && started.elapsed() < Duration::from_secs(8) {
        slow.conn.send(slow.channel, wire::CREDIT, &(16u32 << 10).to_le_bytes());
        slow.drain(Duration::from_millis(5));
        live.drain(Duration::from_millis(5));
    }
    live.shows("DONE");
    grant(&mut slow);
    slow.until("the image", |v| v.screen.image_pixels(9).is_some() && !v.screen.image_placements().is_empty());
    live.until("the image", |v| v.screen.image_pixels(9).is_some());
    assert_eq!(slow.screen.image_pixels(9), live.screen.image_pixels(9));
}

#[test]
fn an_image_framed_with_empty_first_and_last_commands_survives_a_recovery() {
    let s = Session::new();
    let mut a = chunked("a=T,f=32,s=1024,v=1536,i=9,c=8,r=2,q=2", &vec![7u8; 1024 * 1536 * 4]);
    a.extend(b"\r\n");
    let (fa, fb, next) = (s.root.join("a"), s.root.join("b"), s.root.join("next"));
    std::fs::write(&fa, &a).unwrap();
    std::fs::write(&fb, gfx_framed("a=T,f=32,s=277,v=277,i=10,c=8,r=2", &vec![8u8; 277 * 277 * 4])).unwrap();
    let program = after_go(
        &s,
        &format!(
            "cat {}; printf 'FIRST\\r\\n'; while [ ! -e {} ]; do sleep 0.01; done; cat {}; printf '\\r\\nDONE\\r\\n'; sleep 100",
            fa.display(),
            next.display(),
            fb.display()
        ),
    );
    let t = run(&s, &["sh", "-c", &program]);
    let mut stalled = watch(&s, &t);
    stalled.credit = false;
    let mut live = observe(&s, &t);
    go(&s);
    live.shows("FIRST");
    stalled.drain(Duration::from_millis(100));
    grant(&mut stalled);
    stalled.shows("FIRST");
    stalled.drain(Duration::from_millis(300));
    std::fs::write(&next, "").unwrap();
    live.shows("DONE");
    stalled.shows("DONE");
    stalled.drain(Duration::from_millis(300));
    assert_eq!(ids(&live), [9, 10]);
    assert_eq!(ids(&stalled), [9, 10]);
}

#[test]
fn a_stream_of_images_does_not_repaint_a_viewer_for_every_image() {
    let s = Session::new();
    let frame = chunked("a=T,f=32,s=300,v=300,i=8,c=4,r=2,C=1,q=2", &vec![5u8; 300 * 300 * 4]);
    let file = s.root.join("frame");
    std::fs::write(&file, &frame).unwrap();
    let rest = after_go(
        &s,
        &format!(
            "i=0; while [ $i -lt 19 ]; do cat {}; sleep 0.01; i=$((i+1)); done; printf 'DONE\\r\\n'",
            file.display()
        ),
    );
    let program = format!("printf 'FIRST\\r\\n'; cat {}; {rest}; sleep 100", file.display());
    let t = run(&s, &["sh", "-c", &program]);
    let mut v = watch(&s, &t);
    v.shows("FIRST");
    go(&s);
    let mut paints = 0;
    let start = Instant::now();
    while !v.text().contains("DONE") && start.elapsed() < Duration::from_secs(30) {
        let before = v.raw.len();
        v.drain(Duration::from_millis(20));
        paints += v.raw[before..].iter().filter(|b| **b == 0x18).count();
        v.raw.clear();
    }
    assert!(v.text().contains("DONE"));
    // How many depends on how busy the machine is; the unit tests pin the rule down.
    assert!(paints < 10, "painted {paints} times, for 20 images");
    v.until("the image", |v| !v.screen.image_placements().is_empty());
}

#[test]
fn a_large_first_paint_outlives_chatty_output() {
    let s = Session::new();
    // Over the backlog a viewer may fall behind by, so only the paint's own allowance gets it through.
    let mut b = chunked("a=T,f=32,s=1000,v=825,i=1,c=8,r=2,q=2", &noise(3_300_000));
    b.extend(b"\r\n");
    let file = s.root.join("emit");
    std::fs::write(&file, &b).unwrap();
    // On one line: scrolling would rightly take the image off the screen.
    let program = format!("cat {}; while :; do printf 'tick\\r'; sleep 0.002; done", file.display());
    let t = run(&s, &["sh", "-c", &program]);
    printed(&s, &t, "tick");
    let mut v = watch(&s, &t);
    v.until("the image", |v| !v.screen.image_placements().is_empty());
    assert_eq!(v.screen.image_pixels(1).map(|p| p.len()), Some(3_300_000));
    assert!(v.received < 6 << 20, "painted more than once: {}", v.received);
}

#[test]
fn a_viewer_gets_the_text_before_a_large_direct_image_arrives() {
    let s = Session::new();
    let mut b = b"HELLO\r\n".to_vec();
    b.extend(chunked("a=T,f=32,s=800,v=750,i=1,c=8,r=2,q=2", &noise(2_400_000)));
    b.extend(b"\r\n");
    let t = printer(&s, &b);
    go(&s);
    printed(&s, &t, "DONE");
    // Never gives credit back: all it ever gets is the first window of the paint.
    let mut v = watch(&s, &t);
    v.credit = false;
    v.shows("HELLO");
    assert!(v.received <= wire::WINDOW);
    assert!(ids(&v).is_empty(), "the image comes after");
    grant(&mut v);
    v.until("the image", |v| !v.screen.image_placements().is_empty());
}

#[test]
fn reading_a_full_screen_app_keeps_its_images_on_attached_viewers() {
    let s = Session::new();
    let mut b = vec![];
    b.extend(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[H");
    b.extend(chunked("a=T,f=100,i=1,c=4,r=2,C=1,q=2", &png_filled(8, 4, [255, 0, 0, 255])));
    b.extend(b"screen one\r\n");
    let file = s.root.join("emit");
    std::fs::write(&file, &b).unwrap();
    let program = format!("cat {}; stty raw -echo; printf 'DONE\\r\\n'; cat >/dev/null", file.display());
    let t = run(&s, &["sh", "-c", &program]);
    let mut v = watch(&s, &t);
    v.shows("DONE");
    assert_eq!(ids(&v), [1]);
    v.raw.clear();
    let _ = s.json(&["read", &t, "--lines", "200"]);
    v.drain(Duration::from_millis(300));
    assert!(v.raw.contains(&0x18), "the read scrolled and repainted");
    assert_eq!(ids(&v), [1]);
}

#[test]
fn an_image_too_big_for_a_paint_leaves_a_viewer_its_text_and_the_others_that_fit() {
    let s = Session::new();
    let mut b = b"HELLO\r\n".to_vec();
    b.extend(chunked("a=T,f=32,s=1000,v=1500,i=1,c=8,r=2,q=2", &noise(6_000_000)));
    b.extend(b"\r\n");
    b.extend(chunked("a=T,f=100,i=2,c=4,r=1,q=2", &png_filled(8, 2, [0, 255, 0, 255])));
    b.extend(b"\r\n");
    let t = printer(&s, &b);
    let mut live = observe(&s, &t);
    go(&s);
    // Under load it can fall behind too, and then it's in the late viewer's position.
    live.shows("DONE");
    let mut late = watch(&s, &t);
    late.shows("HELLO");
    late.until("the small image", |v| ids(v) == [2]);
}

#[test]
fn a_viewer_attached_during_an_image_gets_it_when_a_stream_of_them_rests() {
    let s = Session::new();
    let frame =
        |id: u32| chunked(&format!("a=T,f=32,s=300,v=300,i={id},c=4,r=2,C=1,q=2"), &vec![id as u8; 300 * 300 * 4]);
    let first = frame(1);
    let cut = cut_after_four(&first);
    std::fs::write(s.root.join("half"), &first[..cut]).unwrap();
    std::fs::write(s.root.join("rest"), &first[cut..]).unwrap();
    let names: Vec<String> = (2..=8)
        .map(|id| {
            let file = s.root.join(format!("frame{id}"));
            std::fs::write(&file, frame(id)).unwrap();
            file.display().to_string()
        })
        .collect();
    let root = s.root.display();
    let program = format!(
        "cat {root}/half; touch {root}/halfway; while [ ! -e {root}/resume ]; do sleep 0.01; done; cat {root}/rest; \
         for f in {}; do cat $f; sleep 0.03; done; printf 'DONE\\r\\n'; sleep 100",
        names.join(" ")
    );
    let t = run(&s, &["sh", "-c", &program]);
    wait_until("the first image to be half sent", || s.root.join("halfway").exists());
    let mut v = watch(&s, &t);
    std::fs::write(s.root.join("resume"), "").unwrap();
    v.shows("DONE");
    v.until("all of them", |v| ids(v) == (1..=8).collect::<Vec<u32>>());
    v.drain(Duration::from_millis(300));
    let cursor = |v: &mut Viewer| {
        v.screen.take_responses();
        v.screen.write(b"\x1b[6n");
        v.screen.take_responses()
    };
    let mut live = observe(&s, &t);
    live.until("all of them", |v| ids(v) == (1..=8).collect::<Vec<u32>>());
    live.drain(Duration::from_millis(300)); // the text after the images sets the cursor
    assert_eq!(cursor(&mut v), cursor(&mut live));
}

#[test]
fn a_clear_dropped_by_a_resize_does_not_leave_a_viewer_its_image() {
    let s = Session::new();
    let mut first = chunked("a=T,f=100,i=1,c=4,r=2,C=1,q=2", &png_filled(8, 4, [255, 0, 0, 255]));
    first.extend(b"\r\nFIRST\r\n");
    // A status line redrawn in place never scrolls the image away. Then what clear(1) prints.
    let mut second: Vec<u8> =
        (0..12000).flat_map(|i| format!("\x1b[10;1Hprogress {i} ........").into_bytes()).collect();
    second.extend(b"\x1b[H\x1b[2J\x1b[3JDONE\r\n");
    let t = script(&s, &first, &second);
    let mut observer = Viewer::attach(&s, &t, 80, 24, json!({"observe": true, "resize_notifications": true}));
    let mut leader = watch(&s, &t);
    observer.shows("FIRST");
    observer.until("the image", |v| v.screen.image_placements().len() == 1);
    observer.drain(Duration::from_millis(100));
    observer.credit = false;
    go(&s);
    leader.shows("DONE");
    observer.drain(Duration::from_millis(200));
    assert!(!observer.screen.plain_text().contains("DONE"), "it's meant to be behind");
    let mut bigger = Viewer::attach(&s, &t, 100, 30, json!({}));
    bigger.drain(Duration::from_millis(300));
    grant(&mut observer);
    observer.shows("DONE");
    observer.until("the image gone", |v| v.screen.image_placements().is_empty());
}

#[test]
fn a_viewer_told_only_the_last_size_of_a_drag_has_no_image_the_screen_lost_on_the_way() {
    let s = Session::new();
    // A full window with a picture near its top, as after `icat x.png; ls`.
    let mut first = b"$ a\r\n$ b\r\n$ icat\r\n".to_vec();
    first.extend(chunked("a=T,f=100,i=1,c=4,r=1,q=2", &png_filled(8, 4, [255, 0, 0, 255])));
    first.extend(b"\r\n");
    first.extend((0..19).flat_map(|i| format!("below {i}\r\n").into_bytes()));
    first.extend(b"FIRST");
    let t = script(&s, &first, b"");
    let mut leader = watch(&s, &t);
    let mut observer = Viewer::attach(&s, &t, 80, 24, json!({"observe": true, "resize_notifications": true}));
    leader.shows("FIRST");
    observer.shows("FIRST");
    observer.until("the image", |v| v.screen.image_placements().len() == 1);
    observer.drain(Duration::from_millis(100));
    for (w, h) in [(78u16, 18u16), (76, 22)] {
        leader.conn.send(leader.channel, wire::RESIZE, &[w.to_le_bytes(), h.to_le_bytes()].concat());
    }
    observer.drain(Duration::from_millis(600));
    let visible = |v: &Viewer| v.screen.image_placements().iter().filter(|p| p.visible).count();
    let mut late = Viewer::attach(&s, &t, 76, 22, json!({"observe": true}));
    late.drain(Duration::from_millis(300));
    assert_eq!(visible(&late), 0, "or this isn't the case it's about");
    assert_eq!(visible(&observer), 0);
}

#[test]
fn a_late_viewer_resized_with_nothing_else_going_on_gets_its_image_where_the_screen_has_it() {
    let s = Session::new();
    let mut first = vec![b'x'; 100];
    first.extend(chunked("a=T,f=100,i=1,c=4,r=1,C=1,q=2", &png_filled(8, 4, [255, 0, 0, 255])));
    first.extend(b"\r\nREADY");
    let t = script(&s, b"", &first);
    // There from the start, so the lines are printed at this width.
    let mut early = watch(&s, &t);
    go(&s);
    early.shows("READY");
    let mut viewer = watch(&s, &t);
    viewer.until("the image", |v| v.screen.image_placements().len() == 1);
    assert_eq!(viewer.screen.image_placements()[0].row, 1, "or this isn't the case it's about");
    viewer.conn.send(viewer.channel, wire::RESIZE, &[120u16.to_le_bytes(), 24u16.to_le_bytes()].concat());
    viewer.screen.resize(120, 24);
    viewer.until("the image where the screen has it", |v| {
        v.screen.image_placements().first().is_some_and(|p| (p.row, p.col) == (0, 100))
    });
}
