use super::*;

/// As long as a test may take: only some are about running out of time.
const FOREVER: usize = usize::MAX;
use crate::testing::{base64, chunked, count, dot, noise, placed, placeholder_cells, same_image};
use kitty::tests::png_of;

/// One kitty command, base64 included, without chunks.
fn image(control: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = format!("\x1b_G{control},q=2;{}", base64(payload)).into_bytes();
    out.extend(b"\x1b\\");
    out
}

/// A 4x2 png over two cells.
fn small(id: u32) -> Vec<u8> {
    chunked(&format!("a=T,f=100,i={id},c=2,r=1,C=1,q=2"), &png_of(4, 2))
}

/// An 8x2 png behind placeholder cells, as image 2.
fn placeholder_png() -> Vec<u8> {
    chunked("a=T,f=100,U=1,i=2,c=4,r=1,q=2", &png_of(8, 2))
}

fn ids(t: &Terminal) -> Vec<u32> {
    t.image_placements().iter().map(|p| p.image).collect()
}

/// Runs the encoder dry, and says if it asks for more than `most` jobs.
#[track_caller]
fn settle(a: &mut Terminal, most: usize, why: &str) {
    let mut jobs = 0;
    while let Some(job) = a.next_encode() {
        a.encoded(job.run());
        jobs += 1;
        assert!(jobs <= most, "{why}");
    }
}

#[test]
fn png_images_move_the_cursor_like_a_viewer_does() {
    let mut t = Terminal::new(80, 24, 1 << 20);
    t.write(b"png[");
    t.write(&image("a=T,f=100,i=1,c=4,r=3", &png_of(8, 6)));
    t.write(b"]x");
    assert_eq!(t.screen_rows()[2].trim_end(), "        ]x");
}

#[test]
fn images_without_a_size_take_it_from_the_viewers_cell() {
    let shown = |cell| {
        let mut t = Terminal::new(80, 24, 1 << 20);
        t.set_display(None, None, None, cell);
        t.write(&image("a=T,f=100,i=1", &png_of(100, 80)));
        t.write(b"X");
        t.screen_rows().iter().position(|row| row.contains('X')).unwrap()
    };
    assert_eq!(shown(None), 3, "80 px over the default 20 px cell: 4 rows");
    assert_eq!(shown(Some((20, 40))), 1, "over a 40 px cell: 2 rows");
}

#[test]
fn a_screen_keeps_images_past_the_librarys_default_limit() {
    let mut t = Terminal::new(80, 24, 1 << 20);
    let pixels = vec![1u8; 2000 * 2000 * 4];
    t.write(&image("a=T,f=32,s=2000,v=2000,i=1,c=4,r=3", &pixels));
    t.write(b"X");
    assert!(t.screen_rows()[2].contains('X'), "16 MB is over the 10 MB default, and placing it moved the cursor");
}

// ---- Replay: a viewer ends up with what the screen has.

fn fresh(paint: &[u8]) -> Terminal {
    let mut viewer = Terminal::new(80, 24, 1 << 20);
    viewer.write(paint);
    viewer
}

fn cursor_of(t: &mut Terminal) -> Vec<u8> {
    t.take_responses();
    t.write(b"\x1b[6n");
    t.take_responses()
}

#[test]
fn a_painted_terminal_brings_back_its_images_and_never_makes_a_viewer_reply() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(b"direct\r\n");
    a.write(&chunked("a=T,f=100,i=1,c=8,r=2,z=-1,q=2", &png_of(16, 8)));
    a.write(b"\r\nraw\r\n");
    a.write(&chunked("a=T,f=24,s=2,v=2,i=3,c=2,r=2,q=2", &[9u8; 12]));
    a.write(b"\r\nimplicit ");
    a.write(&chunked("a=T,f=100,c=2,r=1,C=1,q=2", &png_of(4, 2)));
    a.write(b"\r\n");
    a.write(&placeholder_png());
    a.write(placeholder_cells().as_bytes());
    a.write(b"\x1b[31mred");
    let mut v = fresh(&a.paint(0));
    assert!(v.take_responses().is_empty(), "a program would read these as typing");
    assert_eq!(placed(&a), placed(&v));
    assert_eq!(a.screen_rows(), v.screen_rows());
    for id in [1, 2, 3, 2147483647] {
        assert!(same_image(&a.image_pixels(id).unwrap(), &v.image_pixels(id).unwrap()), "pixels of {id}");
    }
    assert_eq!(cursor_of(&mut a), cursor_of(&mut v));
}

#[test]
fn native_size_and_partly_sized_images_replay_at_the_size_the_screen_gives_them() {
    for control in ["", ",c=5", ",r=3", ",c=5,r=2", ",X=3,Y=7", ",c=5,Y=7"] {
        let mut a = Terminal::new(80, 24, 1 << 20);
        a.write(&chunked(&format!("a=T,f=32,s=100,v=30,i=1{control},C=1,q=2"), &vec![1u8; 100 * 30 * 4]));
        let v = fresh(&a.paint(0));
        assert_eq!(placed(&a), placed(&v), "with {control:?}");
    }
}

#[test]
fn text_goes_first_and_is_sent_once_when_no_direct_image_is_waited_for() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(b"MARK ");
    a.write(&small(1));
    a.write(&placeholder_png());
    a.write(placeholder_cells().as_bytes());
    let paint = a.paint(0);
    let at = |needle: &[u8]| paint.windows(needle.len()).position(|w| w == needle).unwrap();
    assert!(at(b"MARK") < at(b"i=1"), "text before the direct image");
    assert_eq!(count(&paint, b"MARK"), 2, "and again after it, to restore the screen's modes");
    let last_text = paint.windows(4).rposition(|w| w == b"MARK").unwrap();
    assert!(at(b"i=1") < last_text);
    assert!(last_text < at(b"i=2"), "placeholder images after everything");

    let mut placeholders_only = Terminal::new(80, 24, 1 << 20);
    placeholders_only.write(b"MARK ");
    placeholders_only.write(&placeholder_png());
    assert_eq!(count(&placeholders_only.paint(0), b"MARK"), 1, "a paint without direct images sends the text once");
}

#[test]
fn a_direct_image_keeps_a_pending_wrap_at_the_bottom_right() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&small(1));
    a.write(b"\x1b[24;1H");
    a.write(&[b'x'; 80]);
    let mut v = fresh(&a.paint(0));
    assert_eq!(placed(&a), placed(&v));
    for t in [&mut a, &mut v] {
        t.write(b"Y");
    }
    assert_eq!(a.screen_rows(), v.screen_rows(), "Y wraps and scrolls on both");
    assert_eq!(placed(&a), placed(&v));
}

#[test]
fn deleted_cleared_and_scrolled_off_images_are_not_painted() {
    let cases: [&[u8]; 4] = [b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\", b"\x1b[2J", b"\x1bc", &[b'\n'; 60]];
    for case in cases {
        let mut a = Terminal::new(80, 24, 1 << 20);
        a.write(&small(1));
        a.write(case);
        assert_eq!(count(&a.repaint(), b"\x1b_G"), 2, "only clearing what the viewer has, after {case:?}");
    }
}

#[test]
fn a_terminal_that_never_saw_an_image_paints_none_and_keeps_nothing() {
    let a = Terminal::new(80, 24, 1 << 20);
    assert_eq!(count(&a.repaint(), b"\x1b_G"), 2, "only clearing what the viewer has");
    assert_eq!(count(&a.paint(0), b"\x1b_G"), 1, "and on a first paint, nothing of what it may have");
    assert_eq!(count(&a.paint_text(0), b"\x1b_G"), 1);
    assert!(a.encoded[0].borrow().is_empty());
}

#[test]
fn a_half_received_image_on_a_viewer_swallows_nothing_that_follows() {
    let open = chunked("a=T,f=32,s=600,v=600,i=77,q=2", &vec![7u8; 600 * 600 * 4]);
    let cut = open.windows(2).enumerate().filter(|(_, w)| *w == b"\x1b\\").nth(5).unwrap().0 + 2;
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=100,i=5,c=4,r=2,C=1,q=2", &png_of(8, 4)));

    let mut v = Terminal::new(80, 24, 1 << 20);
    v.write(&open[..cut]);
    v.write(&a.paint(0));
    assert_eq!(placed(&a), placed(&v), "the replay isn't taken for its continuation");

    let empty = Terminal::new(80, 24, 1 << 20);
    let mut v = Terminal::new(80, 24, 1 << 20);
    v.write(&open[..cut]);
    v.write(&empty.paint(0));
    v.write(&chunked("a=T,f=100,i=6,c=2,r=1,C=1,q=2", &png_of(4, 2)));
    assert_eq!(ids(&v), [6], "nor the next live image");
}

#[test]
fn a_load_a_paint_happens_to_complete_leaves_nothing_behind() {
    // All its data arrived, and only the empty last command was lost with the backlog.
    let mut unfinished =
        format!("\x1b_Ga=T,f=32,s=2,v=2,i=70,c=4,r=3,q=2,m=1;{}\x1b\\", base64(&[1u8; 16])).into_bytes();
    let mut a = Terminal::new(80, 24, 1 << 20);
    for i in 0..40 {
        a.write(format!("line{i}\r\n").as_bytes());
    }
    let mut v = Terminal::new(80, 24, 1 << 20);
    v.write(&std::mem::take(&mut unfinished));
    v.write(&a.paint(40));
    assert!(placed(&v).is_empty(), "the clear after it takes the stray placement");
    assert_eq!(fresh(&a.paint(40)).plain_text(), v.plain_text(), "and nothing it moved shifts the history");
}

#[test]
fn replayed_images_keep_their_ids_so_placeholder_cells_still_name_them() {
    // The screen's id for a numbered image is what its reply told the program.
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=t,f=100,I=7,q=2", &png_of(4, 2)));
    a.write(&chunked("a=T,f=100,I=8,U=1,c=4,r=1,q=2", &png_of(8, 2)));
    assert_eq!(ids(&a), [2]);
    let v = fresh(&a.paint(0));
    assert_eq!(ids(&v), [2], "the cells on screen name image 2");
    let mut twice = fresh(&a.paint(0));
    twice.write(&a.paint(0));
    assert_eq!(ids(&twice), [2], "repainting doesn't stack copies");
}

/// Asks for a paint that image 1 can't fit in three times, and says it was encoded once.
#[track_caller]
fn tried_once(a: &Terminal) {
    kitty::tests::ENCODES.set(0);
    for _ in 0..3 {
        assert!(a.images_within(1000, FOREVER).direct.is_empty());
    }
    assert_eq!(kitty::tests::ENCODES.get(), 1);
}

#[test]
fn an_image_too_big_for_a_paint_is_encoded_once_however_many_places_it_has() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=32,s=100,v=100,i=1,c=2,r=1,C=1,q=2", &noise(40_000)));
    for row in 2..6 {
        a.write(format!("\x1b[{row};1H\x1b_Ga=p,i=1,c=2,r=1,C=1,q=2\x1b\\").as_bytes());
    }
    tried_once(&a);
    assert!(
        a.encoded[0].borrow().values().all(|e| matches!(e, kitty::Encoded::TooBig(_))),
        "and keeps no bytes for it"
    );
    kitty::tests::ENCODES.set(0);
    assert!(!a.images_within(100_000, FOREVER).direct.is_empty(), "it fits a larger paint");
    let _ = a.images_within(100_000, FOREVER);
    assert_eq!(kitty::tests::ENCODES.get(), 1, "which then reuses it");
}

#[test]
fn the_cache_forgets_images_that_are_gone() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&small(1));
    let _ = a.paint(0);
    assert_eq!(a.encoded[0].borrow().len(), 1);
    a.write(b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
    let _ = a.paint(0);
    assert!(a.encoded[0].borrow().is_empty());
}

#[test]
fn painting_a_viewer_again_puts_its_direct_images_back_once() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=100,i=1,c=4,r=2,C=1,q=2", &png_of(8, 4)));
    a.write(&placeholder_png());
    a.write(placeholder_cells().as_bytes());
    let mut v = fresh(&a.paint(0));
    v.write(&a.paint(0));
    assert_eq!(placed(&a), placed(&v));
    assert!(v.take_responses().is_empty());
}

#[test]
fn images_on_the_alternate_screen_are_painted_after_switching_to_it() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&small(1));
    a.write(b"\x1b[?1049h");
    a.write(&small(2));
    let v = fresh(&a.paint(0));
    assert!(v.alt_screen());
    assert_eq!(placed(&a), placed(&v));
}

/// How a screen repaints itself, which is all a program or viewer can tell of it.
fn looks_like(t: &Terminal) -> Vec<u8> {
    t.paint_text(0)
}

#[test]
fn a_paint_with_direct_images_leaves_a_viewer_with_the_same_state_as_one_without() {
    let image = chunked("a=T,f=100,i=1,c=4,r=2,C=1,q=2", &png_of(8, 4));
    let link = "\x1b]8;;http://x\x1b\\";
    let states: &[(&str, String)] = &[
        ("insert mode, which would double text", "hello\x1b[4h".into()),
        ("a link left open", format!("before {link}linked")),
        ("a link closed again", format!("{link}linked\x1b]8;;\x1b\\ plain")),
        ("margins and origin mode", "\x1b[3;10r\x1b[?6h\x1b[2;2Hin".into()),
        ("colors", "\x1b[41;33mred\x1b[K\x1b[0m\r\nplain".into()),
        ("tabs", "\x1b[3g\x1b[1;5H\x1bH\rA\tB".into()),
        ("no wrap", "\x1b[?7lxyz".into()),
        ("a full last row", format!("\x1b[24;1H{}", "z".repeat(80))),
        ("color to the very end", format!("\x1b[24;1H\x1b[41m{}\x1b[0m", "z".repeat(80))),
        ("color left on", "\x1b[41mstill red".into()),
        ("the alternate screen", "main\x1b[?1049halt".into()),
        ("modes and keyboard flags", "\x1b[=3;1u\x1b[?2004h\x1b[?1000hmodes".into()),
    ];
    for (what, state) in states {
        let mut a = Terminal::new(80, 24, 1 << 20);
        a.write(state.as_bytes());
        let without = looks_like(&fresh(&a.paint_text(0)));
        a.write(&image);
        assert_eq!(a.image_placements().len(), 1, "{what}");
        let paint = a.paint(0);
        assert!(count(&paint, b"a=p,i=1") == 1, "{what}");
        let viewer = fresh(&paint);
        assert_eq!(looks_like(&viewer), without, "{what}");
        assert_eq!(placed(&viewer), placed(&a), "{what}");
    }
}

#[test]
fn modes_that_report_when_turned_on_report_once_however_images_are_painted() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(b"\x1b[?2048h");
    a.take_responses();
    a.write(&chunked("a=T,f=100,i=1,c=4,r=2,C=1,q=2", &png_of(8, 4)));
    let mut viewer = fresh(&a.paint(0));
    assert_eq!(count(&viewer.take_responses(), b"\x1b[48;"), 1);
}

#[test]
fn a_repaint_closes_a_link_a_dropped_backlog_left_open() {
    let mut viewer = Terminal::new(80, 24, 1 << 20);
    viewer.write(b"\x1b]8;;http://x\x1b\\linked");
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(b"plain");
    viewer.write(&a.paint(0));
    viewer.write(b" more");
    a.write(b" more");
    assert_eq!(looks_like(&viewer), looks_like(&a));
}

#[test]
fn a_paint_without_images_leaves_them_out_and_a_viewer_keeps_placeholder_ones() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    assert!(!a.has_images());
    a.write(&chunked("a=T,f=100,i=1,c=4,r=2,C=1,q=2", &png_of(8, 4)));
    a.write(&placeholder_png());
    a.write(placeholder_cells().as_bytes());
    assert!(a.has_images());
    let mut placeholder_only = Terminal::new(80, 24, 1 << 20);
    placeholder_only.write(&placeholder_png());
    assert!(placeholder_only.has_images());
    let mut scrolled = Terminal::new(80, 24, 1 << 20);
    scrolled.write(&chunked("a=T,f=100,i=1,c=4,r=2,C=1,q=2", &png_of(8, 4)));
    scrolled.write(&b"\n".repeat(30));
    assert_eq!(scrolled.image_placements().len(), 1);
    assert!(!scrolled.has_images(), "there's nothing to paint");
    let text = a.paint_text(0);
    assert_eq!(count(&text, b"a=t,"), 0);
    assert_eq!(count(&text, b"a=p,"), 0);
    assert!(a.encoded[0].borrow().is_empty(), "and costs no encoding");
    let mut viewer = fresh(&a.paint(0));
    assert_eq!(viewer.image_placements().len(), 2);
    viewer.write(&text);
    assert_eq!(ids(&viewer), [2]);
    assert!(viewer.image_pixels(2).is_some(), "and a text paint leaves its picture");
}

#[test]
fn only_a_new_cell_size_counts_as_a_resize() {
    let mut t = Terminal::new(80, 24, 0);
    t.set_display(None, None, None, Some((9, 18)));
    t.write(b"\x1b[?2048h");
    t.take_responses();
    t.set_display(None, None, None, Some((9, 18)));
    assert!(t.take_responses().is_empty());
    t.set_display(None, None, None, Some((10, 20)));
    assert_eq!(count(&t.take_responses(), b"\x1b[48;"), 1);
}

#[test]
fn in_a_tight_paint_direct_images_go_before_placeholder_ones_and_the_newest_first() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=32,s=100,v=100,i=1,c=2,r=1,C=1,q=2", &noise(40_000)));
    a.write(&chunked("a=T,f=32,s=100,v=100,U=1,i=2,c=4,r=1,q=2", &noise(40_000)));
    let one = a.images_within(60_000, FOREVER);
    assert_eq!(count(&one.direct, b"a=t,"), 1);
    assert_eq!(count(&one.placeholder, b"a=t,"), 0);

    let mut many = Terminal::new(80, 24, 1 << 20);
    for id in 1..=8 {
        many.write(&chunked(&format!("a=T,f=32,s=100,v=100,i={id},c=2,r=1,C=1,q=2"), &noise(40_000)));
    }
    let mut viewer = Terminal::new(80, 24, 1 << 20);
    viewer.write(&many.images_within(60_000, FOREVER).direct);
    assert_eq!(ids(&viewer), [8]);
}

#[test]
fn a_paint_carries_at_most_so_many_images_and_placements() {
    let mut images = Terminal::new(80, 24, 1 << 20);
    for id in 1..=300 {
        images.write(&image(&format!("a=T,f=100,i={id},c=1,r=1,C=1"), &png_of(1, 1)));
    }
    let direct = images.images_within(1 << 20, FOREVER).direct;
    assert_eq!(count(&direct, b"a=t,"), 256);
    assert_eq!(count(&direct, b"a=p,"), 256);

    let mut places = Terminal::new(80, 24, 1 << 20);
    places.write(&image("a=T,f=100,i=1,c=1,r=1,C=1", &png_of(1, 1)));
    for _ in 0..1100 {
        places.write(b"\x1b_Ga=p,i=1,c=1,r=1,C=1,q=2\x1b\\");
    }
    assert_eq!(count(&places.images_within(1 << 20, FOREVER).direct, b"a=p,"), 1024);
}

#[test]
fn once_the_budget_is_spent_nothing_more_is_encoded() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=32,s=100,v=100,i=1,c=2,r=1,C=1,q=2", &noise(40_000)));
    a.write(&chunked("a=T,f=32,s=100,v=100,i=2,c=2,r=1,C=1,q=2", &noise(40_000)));
    let exact = a.images_within(60_000, FOREVER).direct.len();
    a.encoded[0].borrow_mut().clear();
    kitty::tests::ENCODES.set(0);
    assert_eq!(a.images_within(exact, FOREVER).direct.len(), exact);
    assert_eq!(kitty::tests::ENCODES.get(), 1);
}

#[test]
fn every_image_a_screen_replays_reaches_a_viewer_whatever_its_content() {
    let gradient = |w: u32, h: u32, channels: usize| -> Vec<u8> {
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let pixel = [(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) * 255 / (w + h)) as u8, 255];
                pixels.extend_from_slice(&pixel[..channels]);
            }
        }
        pixels
    };
    for (w, h) in [(1, 1), (8, 4), (32, 24), (64, 48), (100, 80), (200, 150), (400, 300)] {
        let area = (w * h) as usize;
        let cases = [(24, gradient(w, h, 3)), (32, gradient(w, h, 4)), (32, noise(area * 4)), (24, noise(area * 3))];
        for (format, pixels) in cases {
            let mut a = Terminal::new(80, 24, 1 << 20);
            a.write(&chunked(&format!("a=T,f={format},s={w},v={h},i=1,c=4,r=2,C=1,q=2"), &pixels));
            let viewer = fresh(&a.paint(0));
            let shown = viewer.image_pixels(1).unwrap_or_else(|| panic!("f={format} {w}x{h} never arrived"));
            let channels = pixels.len() / area;
            let rgb: Vec<&[u8]> = shown.chunks(shown.len() / area).map(|p| &p[..channels]).collect();
            assert!(rgb.iter().zip(pixels.chunks(channels)).all(|(a, b)| *a == b), "f={format} {w}x{h} changed");
        }
    }
}

#[test]
fn an_image_too_big_for_a_paint_does_not_leave_a_viewer_its_old_picture() {
    for places in ["U=1,c=4,r=1", "c=4,r=1,C=1"] {
        let mut a = Terminal::new(80, 24, 1 << 20);
        a.write(&chunked(&format!("a=T,f=100,i=1,{places},q=2"), &png_of(8, 2)));
        let mut viewer = fresh(&a.paint(0));
        assert!(viewer.image_pixels(1).is_some());
        let side = 1040u32;
        a.write(&chunked(&format!("a=T,f=32,s={side},v={side},i=1,{places},q=2"), &noise((side * side * 4) as usize)));
        a.write(b"\x1b_Ga=p,U=1,i=1,p=7,c=4,r=1,q=2\x1b\\");
        let paint = a.paint(0);
        assert_eq!(count(&paint, b"a=t,"), 0, "too big");
        viewer.write(&paint);
        assert!(viewer.image_pixels(1).is_none(), "no picture beats the wrong one");
    }
}

#[test]
fn a_retransmitted_image_replays_its_new_pixels() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    let two = |pixels: &[u8]| chunked("a=T,f=32,s=2,v=1,i=1,c=4,r=1,C=1,q=2", pixels);
    a.write(&two(&[1, 2, 3, 4, 5, 6, 7, 8]));
    let _ = a.paint(0);
    a.write(&two(&[9, 9, 9, 9, 8, 8, 8, 8]));
    assert_eq!(fresh(&a.paint(0)).image_pixels(1).unwrap(), [9, 9, 9, 9, 8, 8, 8, 8]);
}

/// The zlib stream of a PNG. Ghostty inflates `o=z` itself, and strictly, as it does a PNG
/// in a terminal with no decoder hook: every viewer in these tests has the core's lenient one.
fn idat_of(png: &[u8]) -> Vec<u8> {
    let mut zlib = Vec::new();
    let mut at = 8;
    while at < png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        if &png[at + 4..at + 8] == b"IDAT" {
            zlib.extend_from_slice(&png[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    zlib
}

/// Pixels whose filtered rows repeat no three bytes, over a small alphabet: nothing to
/// match, but worth compressing, so a block with no distance codes if an encoder allows it.
fn pixels_with_nothing_to_match(width: u32, height: u32) -> Vec<u8> {
    // A de Bruijn sequence: every three of 16 symbols, once.
    fn extend(t: usize, p: usize, a: &mut [usize], symbols: &mut Vec<u8>) {
        if t > 3 {
            if 3 % p == 0 {
                symbols.extend(a[1..=p].iter().map(|x| *x as u8 + 32));
            }
            return;
        }
        a[t] = a[t - p];
        extend(t + 1, p, a, symbols);
        for j in a[t - p] + 1..16 {
            a[t] = j;
            extend(t + 1, t, a, symbols);
        }
    }
    let mut symbols = Vec::new();
    extend(1, 1, &mut [0; 4], &mut symbols);
    let row = width as usize * 4;
    let mut pixels = Vec::<u8>::new();
    for i in 0..row * height as usize {
        // Undoes what the Paeth filter will do, so that it leaves the sequence.
        let x = i % row;
        let left = if x >= 4 { pixels[i - 4] as i32 } else { 0 };
        let up = if i >= row { pixels[i - row] as i32 } else { 0 };
        let corner = if i >= row && x >= 4 { pixels[i - row - 4] as i32 } else { 0 };
        let guess = left + up - corner;
        let (l, u, c) = ((guess - left).abs(), (guess - up).abs(), (guess - corner).abs());
        let paeth = if l <= u && l <= c {
            left
        } else if u <= c {
            up
        } else {
            corner
        };
        pixels.push(symbols[i % symbols.len()].wrapping_add(paeth as u8));
    }
    pixels
}

#[test]
fn what_a_replay_compresses_is_something_ghostty_itself_can_read() {
    for w in [4, 8, 16, 32, 64, 100, 128, 256] {
        let h = 4;
        let area = (w * h * 4) as usize;
        let gradient: Vec<u8> = (0..area as u32).map(|i| (i / 4 + i % 4 * 11) as u8).collect();
        for pixels in [pixels_with_nothing_to_match(w, h), gradient, noise(area)] {
            let mut a = Terminal::new(80, 24, 1 << 20);
            a.write(&chunked(&format!("a=T,f=32,s={w},v={h},i=1,c=4,r=2,C=1,q=2"), &pixels));
            let png = kitty::tests::png_file_of(&a.image(1).unwrap());
            // Filtered rows are one byte longer each: `h` of 4 makes them a whole number of pixels.
            let s = (w * 4 + 1) * h / 4;
            let mut strict = Terminal::new(80, 24, 1 << 20);
            strict.write(&image_command(&format!("a=T,f=32,s={s},v=1,o=z,i=1,c=4,r=1"), &idat_of(&png)));
            assert!(strict.image_pixels(1).is_some(), "{w}x{h}: Ghostty refused it");
        }
    }
}

fn image_command(control: &str, payload: &[u8]) -> Vec<u8> {
    chunked(&format!("{control},q=2"), payload)
}

#[test]
fn filtering_rows_is_what_makes_photos_fit() {
    use std::io::Write;
    // Grain over a gradient: what compresses by filtering and hardly otherwise.
    let grain = noise(128 * 128 * 4);
    let pixels: Vec<u8> = (0..128u32 * 128 * 4)
        .map(|i| {
            let (x, y) = ((i / 4) % 128, (i / 4) / 128);
            if i % 4 == 3 { 255 } else { ((x + y) as u8).wrapping_add(grain[i as usize] & 3) }
        })
        .collect();
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=32,s=128,v=128,i=1,c=4,r=2,C=1,q=2", &pixels));
    let png = kitty::tests::png_file_of(&a.image(1).unwrap());
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(2));
    zlib.write_all(&pixels).unwrap();
    let plain = zlib.finish().unwrap();
    assert!(png.len() * 5 < plain.len() * 4, "{} against {}", png.len(), plain.len());
}

#[test]
fn an_image_exactly_as_big_as_what_is_left_fits_cached_or_not() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=32,s=100,v=100,i=1,U=1,c=4,r=1,q=2", &noise(40_000)));
    let exact = {
        let replay = a.images_within(1 << 20, FOREVER);
        count(&replay.placeholder, b"a=t,");
        a.encoded[0].borrow_mut().clear();
        kitty::tests::encoded_len(&a.image(1).unwrap())
    };
    assert_eq!(count(&a.images_within(exact, FOREVER).placeholder, b"a=t,"), 1, "fresh");
    assert_eq!(count(&a.images_within(exact - 1, FOREVER).placeholder, b"a=t,"), 0, "a byte short");
    assert_eq!(count(&a.images_within(exact, FOREVER).placeholder, b"a=t,"), 1, "and known by then to be that big");
}

#[test]
fn a_paint_leaves_what_it_may_not_encode_and_carries_what_the_encoder_made() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    for id in 1..=3 {
        a.write(&small(id));
    }
    for carried in 0..3 {
        let replay = a.images_within(1 << 20, 0);
        assert_eq!((count(&replay.direct, b"a=t,"), replay.carried.deferred), (carried, true));
        a.encoded(a.next_encode().unwrap().run());
    }
    assert!(a.next_encode().is_none());
    kitty::tests::ENCODES.set(0);
    let replay = a.images_within(1 << 20, 0);
    assert_eq!((count(&replay.direct, b"a=t,"), replay.carried.deferred), (3, false));
    assert_eq!(kitty::tests::ENCODES.get(), 0, "all cached by now");
}

#[test]
fn what_the_encoder_made_of_an_image_since_replaced_goes_nowhere() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.set_inline_pixels(0);
    a.write(&dot("i=1,c=2,r=1,C=1", [1, 2, 3, 4]));
    let job = a.next_encode().unwrap();
    a.write(&dot("i=1,c=2,r=1,C=1", [9; 4]));
    a.encoded(job.run());
    a.encoded(a.next_encode().unwrap().run());
    assert_eq!(fresh(&a.paint_ready(0)).image_pixels(1).unwrap(), [9, 9, 9, 9]);
}

#[test]
fn small_images_need_no_encoder_and_large_ones_wait_for_it() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&small(1));
    let side = 300u32;
    a.write(&chunked(&format!("a=T,f=32,s={side},v={side},i=2,c=2,r=1,C=1,q=2"), &noise((side * side * 4) as usize)));
    let paint = a.paint_ready(0);
    assert_eq!(count(&paint, b"a=t,"), 1);
    assert!(a.images_carried().deferred);
    settle(&mut a, 1, "only the large one");
    let _ = a.paint_ready(0);
    assert!(!a.images_carried().deferred, "and it says so when they are done");
}

#[test]
fn a_paint_from_the_alternate_screen_leaves_the_main_screens_images_be() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=100,U=1,i=42,c=2,r=1,q=2", &png_of(2, 2)));
    let mut viewer = fresh(&a.paint(0));
    assert_eq!(viewer.image_placements().len(), 1);
    a.write(b"\x1b[?1049h");
    viewer.write(b"\x1b[?1049h");
    viewer.write(&a.paint(0));
    a.write(b"\x1b[?1049l");
    viewer.write(b"\x1b[?1049l");
    assert_eq!(viewer.image_placements().len(), a.image_placements().len());
    assert!(viewer.image_pixels(42).is_some());
}

#[test]
fn a_paint_sends_images_oldest_first_so_a_viewer_lets_the_same_ones_go() {
    let at = |paint: &[u8], id: u32| {
        let wanted = format!("i={id},");
        paint.windows(wanted.len()).position(|w| w == wanted.as_bytes()).expect("sent")
    };
    for places in ["c=2,r=1,C=1", "U=1,c=2,r=1"] {
        let mut a = Terminal::new(80, 24, 1 << 20);
        for id in 1..=3 {
            a.write(&chunked(&format!("a=T,f=100,i={id},{places},q=2"), &png_of(2, 2)));
        }
        let paint = a.paint(0);
        assert!(at(&paint, 1) < at(&paint, 2) && at(&paint, 2) < at(&paint, 3), "{places}");
    }
}

#[test]
fn a_relative_placement_all_off_the_left_stays_out_of_sight() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&image("a=T,f=32,s=1,v=1,i=1,p=1,c=1,r=1,C=1", &[255, 0, 0, 255]));
    a.write(&image("a=t,f=32,s=4,v=1,i=2", &[0, 255, 0, 255].repeat(4)));
    a.write(b"\x1b_Ga=p,i=2,p=2,P=1,Q=1,H=-6,V=1,c=4,r=1,C=1,q=2\x1b\\");
    let child = a.image_placements().into_iter().find(|p| p.image == 2).unwrap();
    assert_eq!((child.col, child.visible), (-6, false));
    let viewer = fresh(&a.paint(0));
    assert!(viewer.image_placements().iter().all(|p| p.image != 2));
}

#[test]
fn a_first_paint_with_no_image_to_bring_leaves_what_a_used_terminal_holds() {
    let mut outer = Terminal::new(80, 24, 1 << 20);
    outer.write(b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\");
    outer.write(&b"\r\n".repeat(60));
    let mut screen = Terminal::new(80, 24, 1 << 20);
    screen.write(b"text");
    outer.write(&screen.paint(0));
    assert!(outer.image_pixels(5).is_some(), "as a paint without images always did");
    screen.write(b"\x1b_Ga=T,f=32,s=1,v=1,i=6,c=2,r=1,q=2;AAAAAA==\x1b\\");
    outer.write(&screen.paint(0));
    assert!(outer.image_pixels(5).is_none(), "of one it can't bring, no picture beats an older one");
}

#[test]
fn an_image_cut_off_at_the_top_keeps_its_offset_inside_the_cell() {
    let mut screen = Terminal::new(80, 24, 1 << 20);
    screen.set_display(None, None, None, Some((10, 40)));
    let mut image = b"\x1b_Ga=T,f=32,s=40,v=80,i=5,Y=39,C=1,q=2;".to_vec();
    image.extend(crate::testing::base64(&vec![7u8; 40 * 80 * 4]).as_bytes());
    image.extend(b"\x1b\\");
    screen.write(&image);
    screen.set_display(None, None, None, Some((10, 10)));
    screen.write(b"\x1b[24;1H");
    screen.write(&[b'\n'; 4]);
    let p = screen.image_placements()[0].clone();
    assert!(p.visible && p.row < 0, "or this isn't the case it's about");
    let mut viewer = Terminal::new(80, 24, 1 << 20);
    viewer.set_display(None, None, None, Some((10, 10)));
    viewer.write(&screen.paint(0));
    let shows = viewer.image_placements()[0].source[3] as i64;
    let wanted = p.pixels.1 as i64 + p.row as i64 * 10 + p.offset.1.min(9) as i64;
    assert!(shows.abs_diff(wanted) <= 10, "shows {shows} of {wanted}");
}

#[test]
fn a_paint_stops_encoding_past_its_share_and_leaves_the_rest_for_the_next() {
    // Noise doesn't compress: three fit a paint, and trying the others would only stall the lock.
    let mut a = Terminal::new(80, 24, 1 << 20);
    for id in 1..=8 {
        a.write(&chunked(&format!("a=T,f=32,s=512,v=512,i={id},c=1,r=1,C=1,q=2"), &noise(1 << 20)));
    }
    kitty::tests::ENCODES.set(0);
    let paint = a.paint(0);
    assert_eq!(count(&paint, b"a=t,"), 3);
    assert!(kitty::tests::ENCODES.get() < 8, "not every one: {}", kitty::tests::ENCODES.get());
    assert_eq!(count(&a.paint(0), b"a=t,"), 3, "the same ones, whatever the next paint tries");
}

#[test]
fn painting_the_alternate_screen_keeps_what_the_main_one_encoded() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&small(1));
    let _ = a.paint(0);
    a.write(b"\x1b[?1049h");
    a.write(&small(2));
    let _ = a.paint(0);
    a.write(b"\x1b[?1049l");
    kitty::tests::ENCODES.set(0);
    let _ = a.paint(0);
    assert_eq!(kitty::tests::ENCODES.get(), 0);
}

#[test]
fn a_paint_says_what_it_carried_and_what_it_could_not() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=100,i=1,c=2,r=1,C=1,q=2", &png_of(2, 2)));
    let _ = a.paint(0);
    let first = a.images_carried();
    assert!(!first.refused && !first.deferred);
    let _ = a.paint(0);
    assert_eq!(a.images_carried().images, first.images, "the same ones");
    a.write(&chunked("a=T,f=100,i=2,c=2,r=1,C=1,q=2", &png_of(2, 2)));
    let _ = a.paint(0);
    assert_ne!(a.images_carried().images, first.images);
    let second = a.images_carried().images;
    a.write(&chunked("a=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,q=2", &[9; 4]));
    let _ = a.paint(0);
    assert_ne!(a.images_carried().images, second, "a replaced image is not what a viewer already has");
    a.write(&chunked("a=T,f=32,s=1040,v=1040,i=3,c=2,r=1,C=1,q=2", &noise(1040 * 1040 * 4)));
    let _ = a.paint(0);
    assert!(a.images_carried().refused, "too big, for good");
}

#[test]
fn a_repaint_at_a_marked_prompt_adds_nothing_to_a_viewers_history() {
    // On the bottom row: that's where Ghostty looks for one.
    let page = [&b"line\r\n".repeat(20)[..], b"\x1b]133;A\x07$ \x1b]133;B\x07"].concat();
    let mut screen = Terminal::new(40, 10, 1 << 20);
    screen.write(&page);
    let mut viewer = Terminal::new(40, 10, 1 << 20);
    viewer.write(&page);
    let history = viewer.history_rows();
    for _ in 0..3 {
        viewer.write(&screen.repaint());
        // Each comes with its own marks: stock's own paints leave those out too.
        viewer.write(b"\x1b[10;1H\x1b]133;A\x07$ \x1b]133;B\x07");
    }
    assert_eq!(viewer.history_rows(), history);
    let mut used = Terminal::new(40, 10, 1 << 20);
    used.write(&page);
    used.write(&screen.paint(0));
    assert_eq!(used.history_rows(), history + 10, "a first paint scrolls what was there out of the way");
}

#[test]
fn the_encoder_is_done_when_the_budget_is_spent() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.set_inline_pixels(0);
    let side = 1000u32;
    for id in 1..=3 {
        a.write(&chunked(
            &format!("a=T,f=32,s={side},v={side},i={id},c=2,r=1,C=1,q=2"),
            &noise((side * side * 4) as usize),
        ));
    }
    settle(&mut a, 3, "however often it's asked");
    assert!(!a.images_ready() || a.images_carried().refused);
    assert!(a.images_ready());
}

fn three_wide() -> Terminal {
    let mut a = Terminal::new(80, 24, 1 << 20);
    for id in 1..=3 {
        a.write(&chunked(&format!("a=T,f=32,s=4,v=2,i={id},c=2,r=1,C=1,q=2"), &[7; 32]));
    }
    a
}

#[test]
fn what_a_paint_may_encode_on_the_spot_is_for_the_whole_paint() {
    let a = three_wide();
    let replay = a.images_within(1 << 20, 64);
    assert_eq!(count(&replay.direct, b"a=t,"), 2);
    assert!(replay.carried.deferred);
}

#[test]
fn the_encoder_starts_with_what_a_paint_wants_most() {
    let a = three_wide();
    a.encoded(a.next_encode().unwrap().run());
    let replay = a.images_within(1 << 20, 0);
    assert_eq!(count(&replay.direct, b"a=t,"), 1);
    assert_eq!(count(&replay.direct, b"a=t,f=100,i=3,"), 1, "the newest");
}

/// Whether the paint a viewer would get now, from what is encoded, carries image 1.
fn carries_the_first(a: &Terminal) -> bool {
    count(&a.images_within(1 << 20, 0).direct, b"a=t,f=100,i=1,") == 1
}

/// What a later command could tell apart.
fn same_images(a: &Terminal, b: &Terminal, why: &str) {
    let places = |t: &Terminal| {
        let mut places: Vec<_> = t
            .image_placements()
            .iter()
            .map(|p| (p.image, p.id, p.virtual_, p.row, p.col, p.cells, p.source, p.z))
            .collect();
        places.sort();
        places
    };
    assert_eq!(places(a), places(b), "{why}");
    for p in a.image_placements() {
        assert_eq!(a.image_pixels(p.image), b.image_pixels(p.image), "{why}");
    }
    assert_eq!(a.plain_text(), b.plain_text(), "{why}");
    assert_eq!(a.alt_screen(), b.alt_screen(), "{why}");
}

#[test]
fn a_paint_keeping_images_changes_nothing_a_later_command_could_tell() {
    let raw = |control: &str, byte: u8| image(control, &[byte; 4]);
    let steps: Vec<Vec<u8>> = vec![
        raw("a=T,f=32,s=1,v=1,c=2,r=1,q=2", 1),
        raw("a=T,f=32,s=1,v=1,c=2,r=1,q=2", 2),
        raw("a=t,f=32,s=1,v=1,i=5,q=2", 3),
        raw("a=T,f=32,s=1,v=1,I=7,U=1,c=2,r=1,q=2", 4),
        raw("a=T,f=32,s=1,v=1,i=8,U=1,c=2,r=1,q=2", 5),
        b"\r\n\r\n".to_vec(),
        b"\x1b_Ga=p,i=5,c=2,r=1,C=1,q=2\x1b\\".to_vec(),
        b"\x1b_Ga=p,i=5,c=2,r=1,C=1,q=2\x1b\\".to_vec(),
        b"\x1b[3;1H\x1b_Ga=p,i=5,p=1,c=2,r=1,C=1,q=2\x1b\\".to_vec(),
        b"\x1b_Ga=p,i=5,p=2,P=5,Q=1,H=1,V=1,c=2,r=1,q=2\x1b\\".to_vec(),
        b"\x1b[5;1H\x1b_Ga=p,i=5,p=1,c=2,r=1,C=1,q=2\x1b\\".to_vec(),
        b"\n\n\n".repeat(10),
        b"\x1b[3J".to_vec(),
        b"\x1b_Ga=p,i=5,p=1,c=2,r=1,C=1,q=2\x1b\\".to_vec(),
        b"\x1b_Ga=d,d=N,I=7,q=2\x1b\\".to_vec(),
        b"\x1b[?1049h".to_vec(),
        raw("a=T,f=32,s=1,v=1,i=9,c=2,r=1,C=1,q=2", 6),
        b"\x1b_Ga=d,d=i,i=9,q=2\x1b\\".to_vec(),
        raw("a=T,f=32,s=1,v=1,i=10,c=2,r=1,C=1,q=2", 7),
        b"\x1b[?1049l".to_vec(),
        b"\x1b[J".to_vec(),
    ];
    for at in 0..=steps.len() {
        let (mut a, mut viewer) = (Terminal::new(80, 24, 1 << 20), Terminal::new(80, 24, 1 << 20));
        for (n, step) in steps.iter().enumerate() {
            if n == at {
                viewer.write(&a.paint_keeping_images());
                same_images(&a, &viewer, &format!("right after it, at step {at}"));
            }
            a.write(step);
            viewer.write(step);
        }
        same_images(&a, &viewer, &format!("all the way on from step {at}"));
    }
}

#[test]
fn a_paint_keeping_images_says_nothing_of_them() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&chunked("a=T,f=100,i=1,c=2,r=1,q=2", &png_of(2, 2)));
    a.write(&chunked("a=T,f=100,i=2,U=1,c=2,r=1,q=2", &png_of(2, 2)));
    a.write(b"\x1b_Ga=t,m=1,i=3,q=2;AAAA\x1b\\");
    let paint = a.paint_keeping_images();
    for image_command in [&b"\x1b_G"[..], b"\x1b[2J", b"?1049"] {
        assert_eq!(count(&paint, image_command), 0);
    }
}

#[test]
fn a_paint_keeping_images_restores_the_modes_it_resets() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    let live = b"\x1b[?1000h\x1b[?2004h\x1b[5;9Htext";
    a.write(live);
    let mut viewer = Terminal::new(80, 24, 1 << 20);
    viewer.write(live);
    viewer.write(&a.paint_keeping_images());
    assert!(viewer.dec_mode(1000) && viewer.dec_mode(2004));
    a.write(b"!");
    viewer.write(b"!");
    assert_eq!(viewer.plain_text(), a.plain_text(), "with the cursor where it was");
}

#[test]
fn a_paint_keeping_images_leaves_every_placement_however_many() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    let mut live = chunked("a=t,f=100,i=1,q=2", &png_of(2, 2));
    for placement in 1..=1025 {
        live.extend(format!("\x1b_Ga=p,i=1,p={placement},c=1,r=1,C=1,q=2\x1b\\").as_bytes());
    }
    a.write(&live);
    let mut viewer = Terminal::new(80, 24, 1 << 20);
    viewer.write(&live);
    viewer.write(&a.paint_keeping_images());
    assert_eq!(viewer.image_placements().len(), 1025);
    let paint = a.paint(0);
    assert_eq!(count(&paint, b"a=p,"), 1024);
    assert!(a.images_carried().refused, "where a replay can't, it says so");
}

#[test]
fn where_images_are_changes_when_they_scroll_and_what_they_are_does_not() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    assert_eq!(a.image_places(), 0, "none");
    a.write(&chunked("a=T,f=100,i=1,c=2,r=1,C=1,q=2", &png_of(2, 2)));
    let (places, generation) = (a.image_places(), a.images_generation());
    assert_ne!(places, 0);
    a.write(b"text");
    assert_eq!(a.image_places(), places);
    a.write(&b"\r\n".repeat(30));
    assert_ne!(a.image_places(), places);
    assert_eq!(a.images_generation(), generation);
    a.write(&chunked("a=T,f=100,i=2,c=2,r=1,C=1,q=2", &png_of(2, 2)));
    let generation = a.images_generation();
    a.write(b"\x1b[2J");
    assert_ne!(a.images_generation(), generation, "a clear does what a command would");
}

#[test]
fn an_icon_needs_no_encoder_and_anything_larger_does() {
    let paint_of = |width: u32, height: u32| {
        let mut a = Terminal::new(80, 24, 1 << 20);
        a.write(&chunked(
            &format!("a=T,f=32,s={width},v={height},i=1,c=2,r=1,C=1,q=2"),
            &noise((width * height * 4) as usize),
        ));
        let _ = a.paint_ready(0);
        a.images_carried().deferred
    };
    assert!(!paint_of(128, 128), "64 KiB of pixels");
    assert!(paint_of(128, 129));
}

#[test]
fn images_that_keep_being_replaced_do_not_keep_the_encoder_from_the_rest() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&dot("i=1,c=2,r=1,C=1", [1; 4]));
    let replace_both = |a: &mut Terminal, byte: u8| {
        a.write(&dot("i=2,c=2,r=1,C=1", [byte; 4]));
        a.write(&dot("i=3,c=2,r=1,C=1", [byte; 4]));
    };
    for turn in 0..3 {
        replace_both(&mut a, turn);
        a.encoded(a.next_encode().unwrap().run());
    }
    assert!(carries_the_first(&a), "the one that holds still");

    // And again, for one that changes just once, after all that.
    a.write(&dot("i=1,c=2,r=1,C=1", [9; 4]));
    for turn in 3..7 {
        replace_both(&mut a, turn);
        a.encoded(a.next_encode().unwrap().run());
    }
    assert!(carries_the_first(&a));
}

#[test]
fn images_that_keep_being_new_do_not_keep_the_encoder_from_one_that_holds_still() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&dot("i=1,c=2,r=1,C=1", [1; 4]));
    let mut encoded = false;
    for fresh in 10..16u32 {
        a.write(format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", fresh - 1).as_bytes());
        a.write(&dot(&format!("i={fresh},c=2,r=1,C=1"), [fresh as u8; 4]));
        a.write(&dot(&format!("i={},c=2,r=1,C=1", fresh + 100), [fresh as u8; 4]));
        a.encoded(a.next_encode().unwrap().run());
        encoded |= carries_the_first(&a);
    }
    assert!(encoded, "it gets a turn, however new the others are");
}

#[test]
fn an_image_between_two_that_keep_changing_gets_its_turn() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    a.write(&dot("i=1,c=2,r=1,C=1", [1; 4]));
    let mut encoded = false;
    for turn in 0..8u8 {
        // Newest of the direct images, and a placeholder image, which always go after those.
        a.write(&dot("i=2,c=2,r=1,C=1", [turn; 4]));
        a.write(&dot("i=3,U=1,c=2,r=1", [turn; 4]));
        a.encoded(a.next_encode().unwrap().run());
        encoded |= carries_the_first(&a);
    }
    assert!(encoded);
}

#[test]
fn what_has_waited_longest_gets_a_turn_whatever_keeps_changing_around_it() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    // Between the two in the order they're wanted, and older than either.
    a.write(&dot("i=1,c=2,r=1,C=1", [1; 4]));
    let mut encoded = false;
    for turn in 0..1000u32 {
        // Each change comes just after the encoder has taken its next job, and always the
        // one that would put the list back as it was.
        let which = if turn % 2 == 0 { 2 } else { 3 };
        let byte = [turn as u8; 4];
        a.write(&dot(&format!("i={which},c=2,r=1,C=1"), byte));
        a.write(&dot("i=4,U=1,c=2,r=1", byte));
        let job = a.next_encode().unwrap();
        a.write(&dot(&format!("i={},c=2,r=1,C=1", 5 - which), byte));
        a.encoded(job.run());
        if carries_the_first(&a) {
            encoded = true;
            assert!(turn < 4, "took {turn} turns");
            break;
        }
    }
    assert!(encoded);
}

#[test]
fn the_encoder_starts_on_what_is_wanted_most_each_time_it_starts() {
    let mut a = Terminal::new(80, 24, 1 << 20);
    for id in [1, 2] {
        a.write(&dot(&format!("i={id},c=2,r=1,C=1"), [1; 4]));
    }
    settle(&mut a, usize::MAX, "");
    for id in [4, 5, 6] {
        a.write(&dot(&format!("i={id},c=2,r=1,C=1"), [1; 4]));
    }
    assert_eq!(a.next_encode().unwrap().id(), 6, "the newest");
}

#[test]
fn a_repaint_at_a_prompt_isnt_taken_for_more_of_the_prompt() {
    // As a terminal does that lets the shell redraw its prompt, which clears from there on a resize.
    let prompt = "\x1b]133;A\x07$ \x1b]133;B\x07";
    let mut page = String::new();
    for name in ["one", "two"] {
        page += &format!("{prompt}{name}\r\n\x1b]133;C\x07");
        page += &"line\r\n".repeat(if name == "one" { 5 } else { 60 });
    }
    page += prompt;
    let mut screen = Terminal::new(80, 24, 1 << 20);
    screen.write(page.as_bytes());
    let nonblank = |t: &Terminal| t.screen_rows().iter().filter(|r| !r.trim().is_empty()).count();
    for repaint in [screen.repaint(), screen.paint_keeping_images()] {
        let mut viewer = Terminal::new(80, 24, 1 << 20);
        viewer.write(b"\x1b]133;A;redraw=1\x07\x1b]133;C\x07\x1b[H\x1b[2J");
        viewer.write(page.as_bytes());
        viewer.write(&repaint);
        viewer.resize(70, 20);
        assert_eq!(nonblank(&viewer), 20);
    }
}
