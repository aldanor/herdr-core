use super::given::{CACHED, PLACE_CACHED};
use super::*;
use std::time::Instant;

fn late_with_lines_broken(bytes: &[&[u8]]) -> (Rig, Arc<Conn>, u32) {
    let (rig, viewer, channel) = Rig::late(bytes);
    assert!(rig.with_viewer(|v| v.reflows_apart));
    (rig, viewer, channel)
}

type Seat = (Arc<Conn>, u32);

/// Painted while another window held the screen at 40x24, so not at its own size.
fn painted_at_another_size() -> (Rig, Seat, Seat) {
    let (rig, first, first_channel) = Rig::late(&[IMAGE]);
    let (second, second_channel) = rig.viewer();
    read(&first, first_channel);
    rig.core.viewer_resize(&second, second_channel, 40, 24);
    rig.needs_a_full_paint();
    (rig, (first, first_channel), (second, second_channel))
}

fn repainted(rig: &Rig, viewer: &Arc<Conn>, channel: u32) {
    repaint_of(rig, viewer, channel, &mut ghostty_vt::Terminal::new(80, 24, 1 << 20));
}

const NO_SIZE: [(u16, u16); 3] = [(0, 0), (80, 0), (0, 24)];

#[test]
fn a_late_viewer_gets_a_paint_after_a_resize_with_images_in_view() {
    let (rig, viewer, channel) = late_with_lines_broken(&[&[b'x'; 100], IMAGE]);
    while woken(&viewer) {}
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    assert_eq!(rig.owed(), Some(true));
    assert!(!rig.with_viewer(|v| v.whole));
    assert!(woken(&viewer), "a_viewer_resized_wakes_its_writer_for_what_it_is_owed");
    rig.due(&viewer, channel);
    assert!(read(&viewer, channel).is_empty(), "not while it may still be dragging");
    rig.with_viewer(|v| v.resized_at = None);
    rig.core.prepare_repaints(&viewer);
    let mut shown = ghostty_vt::Terminal::new(120, 24, 1 << 20);
    shown.write(&read(&viewer, channel));
    let core = rig.core.lock().screens[&rig.id].image_placements();
    assert_eq!(shown.image_placements()[0].col, core[0].col);
    assert_eq!(shown.image_placements()[0].row, core[0].row);
}

#[test]
fn a_viewer_there_from_the_start_needs_nothing_after_a_resize() {
    for (case, alternate) in [
        ("a_viewer_there_from_the_start_needs_nothing_after_a_resize", false),
        ("a_viewer_there_from_the_start_keeps_the_main_screen_through_a_resize_on_the_other", true),
    ] {
        let rig = Rig::new();
        let (viewer, channel) = rig.viewer();
        assert!(!rig.with_viewer(|v| v.reflows_apart), "{case}");
        rig.output(&[b'x'; 100]);
        rig.output(IMAGE);
        if alternate {
            rig.output(b"\x1b[?1049h");
        }
        rig.core.viewer_resize(&viewer, channel, 120, 24);
        if alternate {
            rig.output(b"\x1b[?1049l");
        }
        assert_eq!(rig.owed(), None, "{case}");
        assert!(rig.with_viewer(|v| v.whole), "{case}");
    }
}

#[track_caller]
fn late_resized(output: &[&[u8]], reflows_apart: bool, size: (u16, u16), owed: Option<bool>) {
    let (rig, viewer, channel) = Rig::late(output);
    assert_eq!(rig.with_viewer(|v| v.reflows_apart), reflows_apart, "or this isn't the case it's about");
    rig.core.viewer_resize(&viewer, channel, size.0, size.1);
    assert_eq!(rig.owed(), owed);
    assert_eq!(rig.with_viewer(|v| v.whole), owed.is_none());
}

#[test]
fn a_late_viewer_with_no_wrapped_line_needs_nothing_after_a_resize() {
    late_resized(&[IMAGE], false, (120, 24), None);
}

#[test]
fn growing_into_history_a_late_viewer_never_had_is_a_paint_though_no_line_wraps() {
    late_resized(&[&b"line\r\n".repeat(30), IMAGE], true, (80, 40), Some(true));
}

#[test]
fn a_viewer_of_another_size_is_only_marked_for_its_next_paint() {
    let (rig, leader, channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
    let (_other, _) = rig.viewer_as(false, false);
    rig.core.viewer_resize(&leader, channel, 120, 24);
    let st = rig.core.lock();
    let other = &st.viewers[&rig.id][1];
    assert_eq!(other.size, (80, 24), "or this isn't the case it's about");
    assert!(!other.whole);
    assert!(other.owed.is_none());
}

#[test]
fn a_command_about_an_image_does_not_call_off_the_paint_a_resize_is_owed() {
    let (rig, viewer, channel) = late_with_lines_broken(&[CACHED, &[b'x'; 100], IMAGE]);
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    assert!(rig.with_viewer(|v| v.given[0].as_ref().unwrap().carried().is_some()));
    rig.output(PLACE_CACHED);
    rig.with_viewer(|v| {
        assert!(v.owed.unwrap().all);
        assert_eq!(v.owed.unwrap().carried, None);
    });
}

#[test]
fn a_viewer_painted_later_on_has_its_reflows_apart_like_any() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    assert!(!rig.with_viewer(|v| v.reflows_apart));
    rig.output(&[b'x'; 100]);
    rig.output(IMAGE);
    read(&viewer, channel);
    note_places(&mut rig.core.lock(), rig.id);
    rig.after_read();
    read(&viewer, channel);
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn a_viewer_told_of_a_resize_has_a_repaint_coming_and_nothing_more() {
    let (rig, leader, channel) = Rig::late(&[IMAGE]);
    let (_observer, _) = rig.viewer_as(true, true);
    rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].size = (120, 24);
    rig.core.viewer_resize(&leader, channel, 120, 24);
    let st = rig.core.lock();
    assert!(st.viewers[&rig.id][1].repaint_pending);
    assert!(st.viewers[&rig.id][1].owed.is_none());
}

#[test]
fn a_resize_while_a_late_viewer_is_on_the_alternate_screen_costs_it_the_main_one() {
    let (rig, viewer, channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
    rig.output(b"\x1b[?1049h");
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    read(&viewer, channel);
    assert_eq!(rig.owed(), None, "nothing shows there");
    rig.output(b"\x1b[?1049l");
    assert_eq!(rig.owed(), Some(true));
    assert!(!rig.with_viewer(|v| v.whole));
}

#[test]
fn a_viewer_coming_to_the_size_another_set_gets_its_paint_too() {
    let (rig, first, first_channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
    let (second, second_channel) = rig.viewer();
    rig.core.viewer_resize(&second, second_channel, 120, 24);
    rig.with_viewer(|v| v.whole = true);
    rig.core.viewer_resize(&first, first_channel, 120, 24);
    let st = rig.core.lock();
    assert!(st.viewers[&rig.id][0].owed.is_some_and(|o| o.all));
    assert!(!st.viewers[&rig.id][0].whole);
}

#[test]
fn a_resize_that_takes_an_image_out_of_view_is_a_paint_too() {
    let (rig, viewer, channel) = late_with_lines_broken(&[&[b'x'; 100], b"\x1b[3;1H", IMAGE, b"\x1b[20;1H"]);
    rig.core.viewer_resize(&viewer, channel, 80, 10);
    assert!(!rig.core.lock().screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn a_resize_that_brings_a_placement_into_view_is_a_paint_too_though_it_has_the_image() {
    let (rig, viewer, channel) = late_with_lines_broken(&[
        b"\x1b_Ga=T,f=32,s=1,v=1,U=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\",
        b"\x1b_Ga=p,i=5,p=2,c=2,r=1,q=2\x1b\\",
        &b"\r\n".repeat(30),
        &[b'x'; 100],
    ]);
    assert!(!rig.core.lock().screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
    rig.core.viewer_resize(&viewer, channel, 80, 60);
    assert!(rig.core.lock().screens[&rig.id].direct_images_in_view());
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn an_observer_is_owed_nothing_for_a_size_it_tells_of() {
    for (case, notified, size) in [
        ("a_viewer_told_of_resizes_needs_nothing_for_telling_its_own", true, (80, 24)),
        ("an_observer_of_another_size_cant_be_put_right_by_a_paint", false, (100, 30)),
    ] {
        let rig = Rig::new();
        rig.output(IMAGE);
        let (observer, observed) = rig.viewer_as(true, notified);
        rig.core.viewer_resize(&observer, observed, size.0, size.1);
        assert_eq!(rig.owed(), None, "{case}");
    }
}

#[test]
fn a_resize_means_a_paint_however_little_a_paint_would_add() {
    let (rig, viewer, channel) = late_with_lines_broken(&[&[b'x'; 100], IMAGE]);
    rig.with_viewer(|v| {
        v.owed = Some(Owed { since: Instant::now(), all: false, finished: 0, carried: Some(7), layout: false })
    });
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    rig.with_viewer(|v| {
        let owed = v.owed.unwrap();
        assert!(owed.all);
        assert_eq!(owed.carried, None);
    });
}

#[test]
fn a_viewer_owed_by_someone_elses_leaving_hears_of_it() {
    let rig = Rig::new();
    let (leader, channel) = rig.viewer();
    rig.core.viewer_resize(&leader, channel, 80, 12);
    rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\");
    rig.output(&b"\r\n".repeat(20));
    let (late, _) = viewer_that_came_back(&rig);
    while woken(&late) {}
    rig.core.detach(&leader, Some(channel));
    assert!(rig.core.lock().viewers[&rig.id][0].owed.is_some(), "or this isn't the case it's about");
    assert!(woken(&late));
}

/// One back after a dropped line: not the one setting the size until it does something.
fn viewer_that_came_back(rig: &Rig) -> (Arc<Conn>, u32) {
    let (conn, channel) = rig.attach(AttachRequest { resumed: true, ..request(false, false) });
    read(&conn, channel);
    (conn, channel)
}

#[test]
fn every_kind_of_paint_says_whether_it_broke_any_lines() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    assert!(!rig.with_viewer(|v| v.reflows_apart));
    rig.output(&[b'x'; 100]);
    read(&viewer, channel);

    // From its writer.
    rig.with_viewer(|v| v.repaint_pending = true);
    rig.due(&viewer, channel);
    read(&viewer, channel);
    assert!(rig.with_viewer(|v| v.reflows_apart));

    // And a later paint, with none left to break, puts them right.
    rig.output(b"\x1b[2J\x1b[H");
    rig.with_viewer(|v| v.repaint_pending = true);
    rig.due(&viewer, channel);
    read(&viewer, channel);
    assert!(!rig.with_viewer(|v| v.reflows_apart));

    // From a recovery.
    rig.output(&[b'x'; 100]);
    rig.output(b"\x1b]0;");
    rig.output(&vec![b'A'; 70 * 1024]);
    rig.with_viewer(|v| {
        v.recovering = true;
        v.reflows_apart = false;
    });
    read(&viewer, channel);
    rig.output(b"AA\x1b\\");
    assert!(!rig.with_viewer(|v| v.recovering));
    assert!(rig.with_viewer(|v| v.reflows_apart));
}

#[test]
fn going_from_window_to_window_with_nothing_moved_costs_neither_of_them_anything() {
    for typed in [false, true] {
        let rig = Rig::new();
        let (first, first_channel) = rig.viewer();
        rig.output(IMAGE);
        rig.output(&b"\r\n".repeat(30));
        rig.output(CACHED);
        rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\");
        let (second, second_channel) = rig.viewer();
        read(&first, first_channel);
        read(&second, second_channel);
        // The late one's own reflow is no match for the screen's, so it's put right this once.
        rig.core.viewer_resize(&second, second_channel, 100, 30);
        rig.rested();
        rig.with_viewer_n(1, |v| v.resized_at = None);
        second.with(|out| out.hold_images_until(second_channel, None));
        rig.core.prepare_repaints(&second);
        read(&second, second_channel);
        let owed = || rig.core.lock().viewers[&rig.id].iter().map(|v| v.owed.is_some()).collect::<Vec<_>>();
        assert_eq!(owed(), [false, false], "or this isn't the case it's about");
        assert!(rig.core.lock().screens[&rig.id].direct_images_in_view(), "nor this");
        let places = rig.core.lock().screens[&rig.id].image_places();
        for _ in 0..3 {
            for (viewer, channel) in [(&first, first_channel), (&second, second_channel)] {
                if typed {
                    rig.core.viewer_input(viewer, channel, b"k", false);
                } else {
                    rig.core.viewer_focus(viewer, channel, true);
                }
                assert_eq!(owed(), [false, false], "typed: {typed}");
            }
        }
        assert!(rig.with_viewer(|v| v.whole), "and the first still has what only it has");
        assert_eq!(rig.core.lock().screens[&rig.id].image_places(), places);
    }
}

#[test]
fn being_left_with_the_size_when_another_viewer_goes_gets_a_late_viewer_its_paint() {
    let (rig, leader, leader_channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
    rig.core.viewer_resize(&leader, leader_channel, 100, 30);
    read(&leader, leader_channel);
    // Painted at the size the leader keeps.
    let (conn, channel) = viewer_that_came_back(&rig);
    assert!(rig.core.lock().viewers[&rig.id][1].painted_elsewhere, "or this isn't the case it's about");
    rig.core.detach(&leader, Some(leader_channel));
    assert_eq!(rig.owed(), Some(true));
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    repaint_of(&rig, &conn, channel, &mut shown);
    assert!(!rig.with_viewer(|v| v.painted_elsewhere));
}

#[test]
fn a_viewer_with_no_size_to_give_pays_nothing_for_keys() {
    for size in NO_SIZE {
        let (rig, viewer, channel) = Rig::late(&[IMAGE]);
        rig.core.viewer_resize(&viewer, channel, size.0, size.1);
        rig.with_viewer(|v| v.owed = None);
        for _ in 0..3 {
            rig.core.viewer_input(&viewer, channel, b"k", false);
            assert_eq!(rig.owed(), None, "{size:?}");
        }
    }
}

#[test]
fn the_alternate_screen_reflows_on_neither_side_whatever_a_paint_said_of_it() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b[?1049h", &[b'x'; 100], IMAGE]);
    assert!(rig.core.lock().screens[&rig.id].screen_rows()[1].contains('x'), "or this isn't the case it's about");
    read(&viewer, channel);
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    assert_eq!(rig.owed(), None);
}

#[test]
fn a_paint_given_at_another_size_is_put_right_on_coming_to_its_own() {
    let (rig, (first, first_channel), _) = painted_at_another_size();
    repainted(&rig, &first, first_channel);
    assert!(rig.with_viewer(|v| v.painted_elsewhere));
    rig.core.viewer_input(&first, first_channel, b"k", false);
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn a_paint_not_yet_acknowledged_was_still_laid_out_for_the_old_size() {
    for (case, read_by_the_writer, size) in [
        ("a_paint_read_and_not_yet_acknowledged_was_still_laid_out_for_the_old_size", true, (120, 24)),
        ("a_paint_still_on_its_way_when_a_viewer_resizes_was_laid_out_for_the_old_size", false, (40, 24)),
    ] {
        let rig = Rig::new();
        rig.output(IMAGE);
        let (conn, channel) = rig.attach(request(false, false));
        if read_by_the_writer {
            conn.with(|out| while out.next_frame().is_some() {});
            assert_eq!(conn.outbox().queued(channel), 0, "{case}: or this isn't the case it's about");
        } else {
            assert!(!rig.with_viewer(|v| v.reflows_apart), "{case}: or this isn't the case it's about");
        }
        rig.core.viewer_resize(&conn, channel, size.0, size.1);
        assert_eq!(rig.owed(), Some(true), "{case}");
    }
}

#[test]
fn an_image_cut_off_at_the_top_comes_whole_when_the_screen_grows() {
    let rig = Rig::new();
    let pixels = vec![123; 40 * 80 * 4];
    let command =
        format!("\x1b_Ga=T,f=32,s=40,v=80,i=7,c=4,r=4,C=1,q=2;{}\x1b\\", ghostty_vt::testing::base64(&pixels));
    rig.output(command.as_bytes());
    rig.output(b"\x1b[24;1H\n\n");
    assert_eq!(rig.core.lock().screens[&rig.id].image_placements()[0].row, -2);
    let (viewer, channel) = rig.viewer();
    rig.core.viewer_resize(&viewer, channel, 80, 26);
    rig.rested();
    rig.with_viewer(|v| v.resized_at = None);
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.prepare_repaints(&viewer);
    let mut shown = ghostty_vt::Terminal::new(80, 26, 1 << 20);
    shown.write(&read(&viewer, channel));
    let core = rig.core.lock().screens[&rig.id].image_placements();
    let got = shown.image_placements();
    assert_eq!((got[0].row, got[0].source, got[0].cells), (core[0].row, core[0].source, core[0].cells));
}

#[test]
fn a_paint_of_the_alternate_screen_says_nothing_of_how_the_main_one_reflows() {
    let (rig, viewer, channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
    rig.output(b"\x1b[?1049h\x1b[?1000hfull screen");
    read(&viewer, channel);
    note_places(&mut rig.core.lock(), rig.id);
    rig.after_read();
    assert!(leaves_images_alone(&read(&viewer, channel)), "or this isn't the case it's about");
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    rig.output(b"\x1b[?1000l\x1b[?1049l");
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn an_observer_not_told_of_resizes_gets_no_paint_for_one() {
    let rig = Rig::new();
    let (_leader, _) = rig.viewer();
    rig.output(&[b'x'; 100]);
    rig.output(IMAGE);
    let (observer, channel) = rig.viewer_as(true, false);
    assert!(rig.core.lock().viewers[&rig.id][1].reflows_apart);
    rig.core.viewer_resize(&observer, channel, 80, 24);
    assert!(rig.core.lock().viewers[&rig.id][1].owed.is_none());
}

#[test]
fn a_resize_on_the_alternate_screen_says_nothing_of_how_the_main_one_reflows() {
    let (rig, viewer, channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
    let (_other, _) = rig.viewer();
    rig.output(b"\x1b[?1049h");
    rig.output(IMAGE);
    {
        let st = rig.core.lock();
        assert!(st.viewers[&rig.id][0].reflows_apart);
        assert!(st.screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
    }
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    let st = rig.core.lock();
    assert!(st.viewers[&rig.id][0].owed.is_none(), "it saw that image arrive, and reflows it as the screen does");
    assert!(st.viewers[&rig.id][1].whole, "nor has it lost anything here");
}

#[test]
fn a_viewer_taking_on_the_size_another_set_loses_the_main_screen_behind_the_alternate_one() {
    for (case, takes_it_on) in [
        ("a_viewer_taking_on_the_size_another_set_loses_the_main_screen_behind_the_alternate_one", true),
        ("a_viewer_that_stayed_at_its_size_keeps_the_main_screen_behind_the_alternate_one", false),
    ] {
        let (rig, first, first_channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
        let (second, second_channel) = rig.viewer();
        rig.output(b"\x1b[?1049h");
        rig.core.viewer_resize(&second, second_channel, 120, 24);
        if takes_it_on {
            read(&first, first_channel);
            rig.with_viewer(|v| v.given[0] = v.given[1].clone());
            rig.core.viewer_resize(&first, first_channel, 120, 24);
        }
        rig.output(b"\x1b[?1049l");
        assert_eq!(rig.owed(), takes_it_on.then_some(true), "{case}");
    }
}

#[test]
fn an_observer_told_of_a_resize_reflows_the_main_screen_behind_the_alternate_one_too() {
    let (rig, leader, channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
    let (_observer, _) = rig.viewer_as(true, true);
    rig.output(b"\x1b[?1049h");
    rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].repaint_pending = false;
    rig.core.viewer_resize(&leader, channel, 120, 24);
    assert!(rig.core.lock().viewers[&rig.id][1].given[0].is_none());
}

#[test]
fn what_a_viewer_is_owed_waits_for_the_screen_to_come_to_its_size() {
    let rig = Rig::new();
    let (leader, channel) = rig.viewer();
    rig.core.viewer_resize(&leader, channel, 60, 20);
    rig.output(&[b'x'; 100]);
    rig.output(IMAGE);
    let (late, late_channel) = viewer_that_came_back(&rig);
    rig.owe(1, Instant::now() - IMAGES_AT_MOST);
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    rig.due(&late, late_channel);
    assert!(read(&late, late_channel).is_empty(), "laid out for 60x20, it would only be wrong");
    assert!(rig.core.lock().viewers[&rig.id][1].owed.is_some(), "and it isn't forgotten");
    rig.core.viewer_focus(&late, late_channel, true);
    rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].resized_at = None;
    rig.due(&late, late_channel);
    shown.write(&read(&late, late_channel));
    assert_eq!(shown.image_placements().len(), 1);
}

#[test]
fn what_a_viewer_is_owed_doesnt_wait_for_a_size_a_terminal_that_ended_wont_come_to() {
    let rig = Rig::new();
    let (leader, channel) = rig.viewer();
    rig.core.viewer_resize(&leader, channel, 60, 20);
    rig.output(IMAGE);
    let (late, late_channel) = viewer_that_came_back(&rig);
    rig.owe(1, Instant::now());
    rig.core.queue_repaints(&mut rig.core.lock(), rig.id, None);
    assert!(carries_images(&read(&late, late_channel)));
}

#[test]
fn a_full_screen_program_quitting_paints_nothing_laid_out_for_another_window() {
    let (rig, first, first_channel) = Rig::late(&[&b"line\r\n".repeat(40)]);
    let (second, second_channel) = rig.viewer();
    rig.core.viewer_resize(&second, second_channel, 60, 20);
    rig.output(b"\x1b[?1049h\x1b[Hfull screen");
    read(&first, first_channel);
    rig.output(b"\x1b[?1049l");
    read(&first, first_channel);
    rig.due(&first, first_channel);
    assert!(read(&first, first_channel).is_empty());
}

#[test]
fn growth_bringing_an_image_into_view_is_a_paint_only_for_an_observer_told_of_it() {
    for told in [true, false] {
        let rig = Rig::new();
        let (leader, channel) = rig.viewer();
        rig.output(IMAGE);
        rig.output(&b"\r\n".repeat(30));
        let (observer, observed) = rig.viewer_as(true, told);
        read(&leader, channel);
        assert!(!rig.core.lock().screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
        rig.core.viewer_resize(&leader, channel, 80, 60);
        assert!(rig.core.lock().screens[&rig.id].direct_images_in_view(), "nor this");
        rig.rested();
        rig.with_viewer_n(1, |v| v.resized_at = None);
        observer.with(|out| out.hold_images_until(observed, None));
        rig.core.prepare_repaints(&observer);
        let paint = read(&observer, observed);
        let mut shown = ghostty_vt::Terminal::new(80, 60, 1 << 20);
        shown.write(&paint);
        assert_eq!(paint.is_empty(), !told, "told: {told}");
        assert_eq!(shown.image_pixels(1).is_some(), told, "told: {told}");
    }
}

#[test]
fn an_observer_gets_no_paint_for_the_screen_coming_back_to_its_size() {
    let rig = Rig::new();
    let (leader, channel) = rig.viewer();
    rig.output(b"\x1b[20H");
    rig.output(IMAGE);
    let (_observer, _) = rig.viewer_as(true, false);
    rig.core.viewer_resize(&leader, channel, 100, 30);
    rig.output(b"\x1b[30H\n\n");
    rig.core.viewer_resize(&leader, channel, 80, 24);
    assert!(rig.core.lock().screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
    assert!(rig.core.lock().viewers[&rig.id][1].owed.is_none());
}

#[test]
fn coming_to_a_size_with_no_image_in_view_is_no_paint() {
    let (rig, late, late_channel) = Rig::late(&[&[b'x'; 100]]);
    let (leader, leader_channel) = rig.viewer();
    rig.core.viewer_resize(&leader, leader_channel, 100, 30);
    rig.with_viewer(|v| v.painted_elsewhere = true);
    rig.core.viewer_input(&late, late_channel, b"k", false);
    assert_eq!(rig.owed(), None);
    assert_eq!(rig.core.lock().reg.terminals[&rig.id].cols, 80, "or this isn't the case it's about");
}

fn repaint_of(rig: &Rig, viewer: &Arc<Conn>, channel: u32, shown: &mut ghostty_vt::Terminal) {
    rig.rested();
    for v in rig.core.lock().viewers.get_mut(&rig.id).unwrap() {
        v.resized_at = None;
    }
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.prepare_repaints(viewer);
    shown.write(&read(viewer, channel));
}

#[test]
fn a_viewer_that_stayed_at_its_size_keeps_its_own_picture_of_what_went_on_at_another() {
    for how in 0..3 {
        for output in [&b"\x1b[24;1H\n\n"[..], b"\r\x1b[J$ ", b"\x1b_Ga=p,i=1,p=2,c=2,r=1,q=2\x1b\\"] {
            let rig = Rig::new();
            let (first, first_channel) = rig.viewer();
            rig.output(b"\x1b[3H");
            rig.output(IMAGE);
            rig.output(CACHED);
            let (second, second_channel) = rig.viewer();
            read(&first, first_channel);
            read(&second, second_channel);
            rig.core.viewer_resize(&second, second_channel, 40, 12);
            rig.output(output);
            match how {
                0 => rig.core.viewer_focus(&first, first_channel, true),
                1 => rig.core.viewer_input(&first, first_channel, b"k", false),
                _ => rig.core.detach(&second, Some(second_channel)),
            }
            assert_eq!(rig.owed(), None, "how: {how}, {output:?}");
            assert!(rig.with_viewer(|v| v.whole), "how: {how}, {output:?}");
        }
    }
}

#[test]
fn coming_to_the_size_another_set_is_nothing_to_a_viewer_that_saw_it_all_arrive() {
    let rig = Rig::new();
    let (first, first_channel) = rig.viewer();
    let (second, second_channel) = rig.viewer();
    read(&first, first_channel);
    read(&second, second_channel);
    rig.output(b"\x1b[20H");
    rig.output(IMAGE);
    rig.core.viewer_resize(&second, second_channel, 100, 30);
    rig.core.viewer_resize(&first, first_channel, 100, 30);
    assert_eq!(rig.owed(), None);
}

#[test]
fn a_viewer_painted_at_another_size_comes_right_by_coming_to_its_own_only_once() {
    let (rig, (first, first_channel), (second, second_channel)) = painted_at_another_size();
    repainted(&rig, &first, first_channel);
    rig.core.viewer_focus(&first, first_channel, true);
    assert_eq!(rig.owed(), Some(true));
    repainted(&rig, &first, first_channel);
    assert!(!rig.with_viewer(|v| v.painted_elsewhere));
    rig.core.viewer_focus(&second, second_channel, true);
    rig.core.viewer_focus(&first, first_channel, true);
    assert_eq!(rig.owed(), None);
}

#[test]
fn another_windows_growth_is_nothing_to_a_viewer_of_its_own_size() {
    let (rig, first, first_channel) = Rig::late(&[IMAGE, &b"\r\n".repeat(30)]);
    let (second, second_channel) = rig.viewer();
    read(&first, first_channel);
    rig.core.viewer_resize(&second, second_channel, 80, 60);
    assert!(rig.core.lock().screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
    assert_eq!(rig.owed(), None);
}

#[test]
fn a_viewer_painted_at_another_size_is_owed_nothing_while_the_screen_is_not_yet_at_its_own() {
    for (case, observer, size) in [
        ("an_observer_painted_at_another_size_gets_no_paint_on_the_screen_coming_to_its_own", true, (80, 24)),
        ("a_viewer_painted_at_another_size_waits_for_its_own", false, (120, 30)),
    ] {
        let rig = Rig::new();
        let (leader, channel) = rig.viewer();
        rig.core.viewer_resize(&leader, channel, 100, 30);
        rig.output(IMAGE);
        let _kept = if observer { rig.viewer_as(true, false) } else { viewer_that_came_back(&rig) };
        assert!(rig.core.lock().viewers[&rig.id][1].painted_elsewhere, "{case}: or this isn't the case it's about");
        rig.core.viewer_resize(&leader, channel, size.0, size.1);
        assert!(rig.core.lock().viewers[&rig.id][1].owed.is_none(), "{case}");
    }
}

#[test]
fn a_kept_paint_at_another_size_can_have_scrolled_what_a_viewer_has() {
    let rig = Rig::new();
    let (first, first_channel) = rig.viewer();
    rig.output(IMAGE);
    let (second, second_channel) = rig.viewer();
    read(&first, first_channel);
    rig.core.viewer_resize(&second, second_channel, 100, 40);
    read(&second, second_channel);
    note_places(&mut rig.core.lock(), rig.id);
    rig.after_read();
    assert!(!carries_images(&read(&first, first_channel)), "or this isn't the case it's about");
    rig.with_viewer(|v| {
        assert!(v.painted_elsewhere);
        assert!(!v.whole);
    });
    rig.core.viewer_focus(&first, first_channel, true);
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn a_viewer_painted_at_another_size_comes_to_it_by_resizing_too() {
    let (rig, (first, first_channel), _) = painted_at_another_size();
    repainted(&rig, &first, first_channel);
    rig.with_viewer(|v| {
        assert!(v.painted_elsewhere && !v.reflows_apart, "or this isn't the case it's about");
    });
    rig.core.viewer_resize(&first, first_channel, 40, 24);
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn a_viewer_with_no_size_to_give_still_gets_what_its_owed() {
    for size in NO_SIZE {
        let rig = Rig::new();
        rig.output(IMAGE);
        let (conn, channel) = rig.attach(AttachRequest { size, ..request(false, false) });
        read(&conn, channel);
        rig.owe(0, Instant::now());
        let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
        repaint_of(&rig, &conn, channel, &mut shown);
        assert_eq!(rig.owed(), None, "{size:?}");
        assert_eq!(shown.image_placements().len(), 1, "{size:?}");
    }
}

#[test]
fn coming_to_the_screens_size_wakes_a_viewer_for_what_it_was_owed() {
    let rig = Rig::new();
    let (first, first_channel) = rig.viewer();
    let (second, second_channel) = rig.viewer();
    rig.core.viewer_resize(&second, second_channel, 100, 30);
    rig.owe(0, Instant::now());
    for touched in [true, false] {
        while woken(&first) {}
        if touched {
            rig.core.viewer_resize(&first, first_channel, 100, 30);
        } else {
            rig.core.viewer_focus(&first, first_channel, true);
        }
        assert!(woken(&first), "resized: {touched}");
    }
}

#[test]
fn a_programs_own_columns_dont_keep_a_viewer_from_what_its_owed() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    rig.output(b"\x1b[?40h\x1b[?3h");
    rig.output(IMAGE);
    shown.write(&read(&viewer, channel));
    assert_eq!(rig.core.lock().screens[&rig.id].size().0, 132, "or this isn't the case it's about");
    rig.owe(0, Instant::now());
    repaint_of(&rig, &viewer, channel, &mut shown);
    assert_eq!(rig.owed(), None);
    assert!(!rig.with_viewer(|v| v.painted_elsewhere), "a_programs_own_columns_arent_being_painted_for_another_size");
}

#[test]
fn a_repaint_by_the_writer_adds_nothing_to_a_viewers_history_at_a_marked_prompt() {
    const PROMPT: &[u8] = b"\x1b[24;1H\x1b]133;A\x07$ \x1b]133;B\x07";
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    rig.output(&b"line\r\n".repeat(40));
    rig.output(IMAGE);
    rig.output(PROMPT);
    shown.write(&read(&viewer, channel));
    let history = shown.history_rows();
    for _ in 0..3 {
        rig.owe(0, Instant::now());
        rig.with_viewer(|v| v.whole = false);
        repaint_of(&rig, &viewer, channel, &mut shown);
        assert_eq!(rig.owed(), None, "or this isn't the case it's about");
        shown.write(PROMPT);
    }
    assert_eq!(shown.history_rows(), history);
}

/// A viewer that got an image from the stream too big for any paint: `after` is what it shows once resized.
fn stream_image_then_resized(before_attach: &[&[u8]]) -> usize {
    use ghostty_vt::testing::{chunked, noise};
    let (rig, viewer, channel) = Rig::late(before_attach);
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    for piece in chunked("a=T,f=32,s=2048,v=1024,i=7,C=1,q=2", &noise(8 << 20)).chunks(64 << 10) {
        rig.output(piece);
        shown.write(&read(&viewer, channel));
    }
    assert_eq!(shown.image_placements().len(), 1, "it came with the stream");
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    shown.resize(120, 24);
    rig.with_viewer(|v| v.resized_at = None);
    rig.due(&viewer, channel);
    rig.encoder_ran();
    rig.due(&viewer, channel);
    shown.write(&read(&viewer, channel));
    shown.image_placements().len()
}

#[test]
fn a_resize_does_not_cost_a_viewer_an_image_no_paint_can_bring() {
    let history = b"line\r\n".repeat(60);
    let prompt: &[u8] = b"$ ";
    for before_attach in [&[][..], &[prompt], &[&history[..]]] {
        assert_eq!(stream_image_then_resized(before_attach), 1);
    }
}
