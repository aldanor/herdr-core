use super::*;
use std::time::{Duration, Instant};

#[test]
fn a_viewer_that_has_missed_nothing_gets_only_the_text() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    viewer.with(|out| out.hold_images_until(channel, Some(Instant::now() + Duration::from_secs(3600))));
    rig.viewer_misses_output();
    rig.flowing();
    rig.core.prepare_repaints(&viewer);
    assert!(leaves_images_alone(&read(&viewer, channel)), "neither output nor the last paint holds this up");
    assert!(rig.with_viewer(|v| v.whole));
    assert_eq!(rig.owed(), None);

    note_places(&mut rig.core.lock(), rig.id);
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    assert!(leaves_images_alone(&read(&viewer, channel)), "nor does a read need more");
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn a_viewer_whose_images_moved_while_it_missed_output_gets_them_again() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b[10;1H", IMAGE]);
    read(&viewer, channel);
    rig.viewer_misses_output();
    rig.output(b"text, that moves nothing");
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(leaves_images_alone(&read(&viewer, channel)));

    rig.viewer_misses_output();
    rig.output(b"\x1b[24;1H\r\n\r\n\r\n");
    rig.due(&viewer, channel);
    assert!(carries_images(&read(&viewer, channel)), "the screen scrolled and it didn't");
}

#[test]
fn a_second_resize_does_not_hide_what_moved_since_the_first() {
    let (rig, leader, channel) = Rig::late(&[IMAGE]);
    let (observer, observed) = rig.viewer_as(true, true);
    read(&observer, observed);
    rig.core.viewer_resize(&leader, channel, 90, 24);
    rig.output(&b"\r\n".repeat(30));
    rig.core.viewer_resize(&leader, channel, 100, 24);
    assert!(!rig.core.lock().viewers[&rig.id][1].whole);
}

#[test]
fn a_clear_a_viewer_misses_is_a_change_to_its_images() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(b"\x1b_Ga=t,f=32,s=1,v=1,i=9,q=2;AAAAAA==\x1b\\");
    read(&viewer, channel);
    rig.viewer_misses_output();
    rig.output(b"\x1b[2J");
    assert!(!rig.with_viewer(|v| v.whole), "the screen forgot that image, and it hasn't");
}

#[test]
fn a_resize_that_cuts_a_paint_short_means_that_paint_is_still_owed() {
    let (rig, leader, channel) = Rig::late(&[IMAGE]);
    let observer = Arc::new(Conn::default());
    let request = request(true, true);
    let observed = rig.core.attach(&observer, rig.id, request).unwrap();
    observer.with(|out| out.resume(observed));
    rig.core.viewer_resize(&leader, channel, 81, 24);
    rig.rested();
    let drag_over = Instant::now() - IMAGES_REST;
    rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].resized_at = Some(drag_over);
    observer.with(|out| out.hold_images_until(observed, None));
    rig.core.prepare_repaints(&observer);
    assert!(carries_images(&read(&observer, observed)), "none of the first one got through");
}

#[test]
fn a_paint_with_images_counts_as_queued_until_it_has_gone() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    rig.needs_a_full_paint();
    rig.due(&viewer, channel);
    assert!(viewer.outbox().images_queued(channel));
    assert!(carries_images(&read(&viewer, channel)));
    assert!(!viewer.outbox().images_queued(channel));
}

#[test]
fn a_viewer_past_the_ceiling_gets_what_is_ready_while_the_encoder_works_on() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    rig.core.lock().screens[&rig.id].set_inline_pixels(0);
    rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\");
    read(&viewer, channel);
    rig.core.lock().screens[&rig.id].set_encoding(true);
    rig.owe_since(Instant::now(), None);
    rig.rested();
    viewer.with(|out| out.hold_images_until(channel, None));
    PAINTS.set(0);
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 0, "it's at it, and will say when it's done");
    rig.past_the_ceiling();
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 1);
    assert_eq!(read(&viewer, channel).windows(4).filter(|w| *w == b"a=t,").count(), 1, "the one that's ready");
    assert_eq!(rig.owed(), Some(true), "and the other still to come");
    assert!(rig.with_viewer(|v| v.owed.unwrap().since.elapsed() < IMAGES_AT_MOST), "in its own time");
}

#[test]
fn a_viewer_past_the_ceiling_with_nothing_new_to_get_waits_again() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    rig.core.lock().screens[&rig.id].set_inline_pixels(0);
    rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\");
    read(&viewer, channel);
    let carried = rig.ready_carried();
    rig.core.lock().screens[&rig.id].set_encoding(true);
    rig.owe_since(Instant::now() - IMAGES_AT_MOST, Some(carried));
    rig.rested();
    viewer.with(|out| out.hold_images_until(channel, None));
    PAINTS.set(0);
    let wait = rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 0);
    assert_eq!(rig.owed(), Some(true));
    assert!(rig.with_viewer(|v| v.owed.unwrap().since.elapsed() < IMAGES_AT_MOST));
    assert!(wait.is_some_and(|wait| wait <= IMAGES_AT_MOST), "or nothing would look at the ceiling again");
}

#[test]
fn a_paint_with_images_starts_the_wait_over_and_text_does_not() {
    let rig = Rig::new();
    let (_viewer, _) = rig.viewer();
    let long_ago = Instant::now() - Duration::from_secs(10);
    let deferred = ghostty_vt::Carried { deferred: true, ..Default::default() };
    let now = Instant::now();
    let since = |rig: &Rig, paint, carried| {
        rig.with_viewer(|v| {
            v.owed = Some(Owed { since: long_ago, all: true, finished: 0, carried: None, layout: false });
            v.painted(now, paint, carried, LoadsNow::default());
            v.owed.unwrap().since
        })
    };
    assert_eq!(since(&rig, Paint::Images, deferred), now);
    assert_eq!(since(&rig, Paint::Text, ghostty_vt::Carried::default()), long_ago);
    let open = LoadsNow { open: true, finished: 0 };
    let cut = rig.with_viewer(|v| {
        v.owed = Some(Owed { since: long_ago, all: true, finished: 0, carried: None, layout: false });
        v.painted(now, Paint::Images, ghostty_vt::Carried::default(), open);
        v.owed.unwrap().since
    });
    assert_eq!(cut, now, "a load this paint cut is a new debt");
}

#[test]
fn a_paint_keeping_images_leaves_a_load_arriving_to_finish_by_itself() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(CHUNK_FIRST);
    read(&viewer, channel);
    rig.viewer_misses_output();
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(leaves_images_alone(&read(&viewer, channel)));
    assert_eq!(rig.owed(), None);
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn viewers_waiting_together_cost_one_look_at_the_images() {
    let rig = Rig::new();
    rig.output(IMAGE);
    let conn = Arc::new(Conn::default());
    rig.viewers_on(&conn, &[rig.id, rig.id, rig.id]);
    let carried = rig.carried();
    {
        let mut st = rig.core.lock();
        for v in st.viewers.get_mut(&rig.id).unwrap() {
            v.owed =
                Some(Owed { since: Instant::now(), all: true, finished: 0, carried: Some(carried), layout: false });
            v.whole = false;
        }
    }
    conn.with(|out| {
        while let Some(frame) = out.next_frame() {
            let channel = u32::from_le_bytes(frame[..4].try_into().unwrap());
            out.credit(channel, frame.len());
        }
    });
    rig.rested();
    let before = rig.walks();
    rig.core.prepare_repaints(&conn);
    assert_eq!(rig.walks(), before + 1);
    assert_eq!(rig.owed(), None);
}

#[test]
fn a_viewer_missing_only_text_still_has_every_image() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    rig.with_viewer(|v| v.repaint_pending = true);
    rig.output(b"text it won't be sent");
    assert!(rig.with_viewer(|v| v.whole));
    rig.output(b"\x1b_Ga=p,i=1,q=2");
    assert!(!rig.with_viewer(|v| v.whole), "from the first byte of an image command");
    rig.output(b"\x1b\\");
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)));
    assert!(rig.with_viewer(|v| v.whole), "and with that it has them again");
}

#[test]
fn a_viewer_whose_backlog_went_may_have_lost_images_with_it() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    rig.overflow();
    assert!(!rig.with_viewer(|v| v.whole));
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)));
}

#[test]
fn image_commands_a_viewer_has_not_read_yet_go_with_a_repaint() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    read(&viewer, channel);
    rig.output(IMAGE);
    assert!(viewer.outbox().images_queued(channel));
    rig.with_viewer(|v| v.repaint_pending = true);
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)), "it would lose them to this paint");
    assert!(!viewer.outbox().images_queued(channel), "they went with that");

    rig.output(IMAGE);
    read(&viewer, channel);
    assert!(!viewer.outbox().images_queued(channel), "once read, it has them");
    rig.viewer_misses_output();
    rig.core.prepare_repaints(&viewer);
    assert!(leaves_images_alone(&read(&viewer, channel)));
}

#[test]
fn a_viewer_a_paint_left_an_image_out_of_is_not_whole() {
    let (rig, _viewer, _) = Rig::late(&[CHUNK_FIRST]);
    assert!(!rig.with_viewer(|v| v.whole), "the rest of that image means nothing to it");
    let carried = ghostty_vt::Carried { refused: true, ..Default::default() };
    rig.with_viewer(|v| v.painted(Instant::now(), Paint::Images, carried, LoadsNow::default()));
    assert!(!rig.with_viewer(|v| v.whole), "one that couldn't fit may yet, when others have gone");
    rig.with_viewer(|v| v.painted(Instant::now(), Paint::Kept, Default::default(), LoadsNow::default()));
    assert!(!rig.with_viewer(|v| v.whole), "and text doesn't bring what it doesn't have");
    rig.with_viewer(|v| v.painted(Instant::now(), Paint::Images, Default::default(), LoadsNow::default()));
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn encoding_that_finds_nothing_new_doesn_t_send_the_paint_again() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    let carried = rig.carried();
    rig.owe_since(Instant::now(), Some(carried));
    rig.rested();
    PAINTS.set(0);
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 0);
    assert_eq!(rig.owed(), None);
    assert!(rig.with_viewer(|v| v.whole));

    rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=9,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\");
    read(&viewer, channel);
    rig.owe_since(Instant::now(), Some(carried));
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 1, "new ones do need it");
}

#[test]
fn a_program_ending_gets_viewers_the_images_there_was_no_time_for() {
    let rig = Rig::new();
    for id in 11..=14 {
        rig.output(format!("\x1b_Ga=T,f=32,s=1,v=1,i={id},c=2,r=1,C=1,q=2;AAAAAA==\x1b\\").as_bytes());
    }
    rig.core.lock().screens[&rig.id].set_inline_pixels(0);
    let (viewer, channel) = rig.viewer();
    read(&viewer, channel);
    assert_eq!(rig.owed(), Some(true));
    rig.flowing();
    rig.core.queue_repaints(&mut rig.core.lock(), rig.id, None);
    assert_eq!(read(&viewer, channel).windows(4).filter(|w| *w == b"a=t,").count(), 4);
}

#[test]
fn what_a_viewer_is_owed_for_good_waits_for_the_ceiling_while_a_load_stays_stuck() {
    let rig = Rig::new();
    let (_viewer, _) = rig.viewer();
    let loads = LoadsNow { open: true, finished: 3 };
    let carried = ghostty_vt::Carried { deferred: true, ..Default::default() };
    rig.with_viewer(|v| v.painted(Instant::now(), Paint::Images, carried, loads));
    let due = rig.with_viewer(|v| v.images_due_in(IMAGES_REST, Duration::ZERO, loads, Instant::now()));
    assert!(due.unwrap() > IMAGES_REST, "nothing has finished since its paint, so the rest doesn't count");
    let finished = LoadsNow { open: true, finished: 4 };
    let due = rig.with_viewer(|v| v.images_due_in(IMAGES_REST, Duration::ZERO, finished, Instant::now()));
    assert_eq!(due, Some(Duration::ZERO), "now something has");
}

#[test]
fn program_output_and_an_idle_writer_never_walk_the_placements() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    let before = rig.walks();
    rig.output(b"plain text\r\n");
    rig.output(IMAGE);
    rig.output(b"\x1b_Ga=p,i=1,p=2,c=2,r=1,C=1,q=2\x1b\\");
    rig.core.prepare_repaints(&viewer);
    assert_eq!(rig.walks(), before, "hundreds of placements would make every read of output pay");
}

#[track_caller]
fn missing(what: &str, early: &[u8], misses: impl Fn(&Rig), after: &[(&[u8], bool)]) {
    let (rig, _viewer, _) = Rig::late(&[early]);
    assert!(rig.with_viewer(|v| v.whole), "{what}: or this isn't the case it's about");
    misses(&rig);
    for (bytes, whole) in after {
        rig.output(bytes);
        assert_eq!(rig.with_viewer(|v| v.whole), *whole, "{what}: {bytes:?}");
    }
}

#[test]
fn a_viewer_missing_output_may_lack_what_it_did() {
    let repaint_pending = |rig: &Rig| rig.with_viewer(|v| v.repaint_pending = true);
    missing("a full reset", IMAGE, repaint_pending, &[(b"\x1bc", false)]);
    missing("a switch of screen", b"", repaint_pending, &[(b"\x1b[?1049h", false)]);
    let reading = |rig: &Rig| {
        rig.core.lock().stitching.insert(rig.id);
    };
    missing("image commands during a read", b"", reading, &[(b"text it won't be sent", true), (IMAGE, false)]);
    let recovering = |rig: &Rig| rig.with_viewer(|v| v.recovering = true);
    missing("a delete while recovering", IMAGE, recovering, &[(b"\x1b_Ga=d,d=A,q=2\x1b\\", false)]);
}

#[test]
fn a_resize_that_drops_queued_image_commands_costs_a_viewer_its_wholeness() {
    let rig = Rig::new();
    let (leader, channel) = rig.viewer();
    let (observer, observed) = rig.viewer_as(true, true);
    rig.output(IMAGE);
    read(&leader, channel);
    assert!(observer.outbox().images_queued(observed));
    rig.core.viewer_resize(&leader, channel, 120, 24);
    assert!(!rig.core.lock().viewers[&rig.id][1].whole);
}

#[test]
fn a_read_replacing_queued_image_commands_brings_the_images() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(IMAGE);
    note_places(&mut rig.core.lock(), rig.id);
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    assert!(carries_images(&read(&viewer, channel)));
    assert!(!viewer.outbox().images_queued(channel), "they went with that");
}

#[test]
fn a_paint_remembers_what_it_carried_so_encoding_can_tell_if_a_viewer_has_it() {
    let (rig, _viewer, _) = Rig::late(&[IMAGE]);
    rig.core.lock().screens[&rig.id].set_inline_pixels(0);
    rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\");
    let (_second, _) = rig.viewer();
    let carried = rig.carried();
    let second = rig.core.lock().viewers[&rig.id][1].owed.unwrap();
    assert_eq!(second.carried, Some(carried));
}

#[test]
fn a_viewer_recovered_on_a_screen_with_no_images_is_owed_none() {
    let rig = Rig::new();
    let (_viewer, _) = rig.viewer();
    rig.output(b"\x1b]0;");
    rig.output(&vec![b'A'; 70 * 1024]);
    rig.with_viewer(|v| v.recovering = true);
    rig.output(b"AA\x1b\\tail");
    assert!(!rig.with_viewer(|v| v.recovering));
    assert_eq!(rig.owed(), None);
}

#[test]
fn a_viewer_missing_only_the_end_of_an_image_command_has_missed_it() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(b"\x1b_Ga=p,i=1,q=2");
    read(&viewer, channel);
    rig.with_viewer(|v| v.repaint_pending = true);
    assert!(rig.with_viewer(|v| v.whole));
    rig.output(b"\x1b\\");
    assert!(!rig.with_viewer(|v| v.whole));
}

#[test]
fn the_encoder_is_asked_once_for_a_screen_until_it_has_finished() {
    let rig = Rig::new();
    let mut asked = rig.core.encoder_rx.lock().unwrap().take().unwrap();
    rig.core.lock().screens[&rig.id].set_inline_pixels(0);
    rig.output(IMAGE);
    let (_first, _) = rig.viewer();
    let (_second, _) = rig.viewer();
    assert_eq!(asked.try_recv().ok(), Some(rig.id));
    assert!(asked.try_recv().is_err(), "it's already at it");
    rig.encoder_ran();
    assert!(!rig.core.lock().screens[&rig.id].encoding());
    rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\");
    let (_third, _) = rig.viewer();
    assert_eq!(asked.try_recv().ok(), Some(rig.id), "and again, for what's new");
}

#[test]
fn a_writer_looks_no_closer_at_the_images_while_the_encoder_has_them() {
    let rig = Rig::new();
    rig.output(IMAGE);
    rig.core.lock().screens[&rig.id].set_inline_pixels(0);
    let (viewer, channel) = rig.viewer();
    read(&viewer, channel);
    rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\");
    rig.owe_since(Instant::now(), None);
    rig.rested();
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.lock().screens[&rig.id].set_encoding(true);
    let before = rig.walks();
    rig.core.prepare_repaints(&viewer);
    assert_eq!(rig.walks(), before, "every wake of a busy terminal would pay");
}

#[test]
fn a_resize_brings_direct_images_again_and_leaves_placeholder_ones_be() {
    let repaint = |image: &[u8]| {
        let (rig, leader, channel) = Rig::late(&[image]);
        let (observer, observed) = rig.viewer_as(true, true);
        read(&observer, observed);
        rig.core.viewer_resize(&leader, channel, 90, 24);
        rig.rested();
        rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].resized_at = Some(Instant::now() - IMAGES_REST);
        observer.with(|out| out.hold_images_until(observed, None));
        rig.core.prepare_repaints(&observer);
        read(&observer, observed)
    };
    assert!(carries_images(&repaint(IMAGE)), "it may not reflow to where the screen did");
    let placeholder = b"\x1b_Ga=T,f=32,s=1,v=1,U=1,i=1,c=2,r=1,q=2;AAAAAA==\x1b\\";
    assert!(leaves_images_alone(&repaint(placeholder)));
}

#[test]
fn an_image_with_no_place_yet_goes_with_a_repaint_unless_the_viewer_has_read_it() {
    for read_it in [true, false] {
        let rig = Rig::new();
        let (viewer, channel) = rig.viewer();
        read(&viewer, channel);
        rig.output(b"\x1b_Ga=t,f=32,s=1,v=1,i=5,q=2;AAAAAA==\x1b\\");
        if read_it {
            read(&viewer, channel);
        }
        rig.viewer_misses_output();
        rig.due(&viewer, channel);
        let paint = read(&viewer, channel);
        assert_eq!(leaves_images_alone(&paint), read_it, "read: {read_it}");
    }
}

#[test]
fn where_images_were_is_only_good_until_the_next_paint() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    read(&viewer, channel);
    rig.viewer_misses_output();
    assert!(rig.with_viewer(|v| v.places.is_some()));
    rig.core.prepare_repaints(&viewer);
    assert!(rig.with_viewer(|v| v.places.is_none()));
}

#[test]
fn a_repaint_partway_through_a_command_does_not_cost_a_viewer_that_load() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    rig.output(b"\x1b_Ga=T,f=32,s=2,v=1,i=2,p=2,c=2,r=1,C=1,q=2,m=1;/wAA\x1b\\");
    shown.write(&read(&viewer, channel));
    rig.output(b"\x1b_Gm=0;");
    shown.write(&read(&viewer, channel));
    note_places(&mut rig.core.lock(), rig.id);
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    shown.write(&read(&viewer, channel));
    rig.output(b"//8AAP8=\x1b\\");
    shown.write(&read(&viewer, channel));
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    shown.write(&read(&viewer, channel));
    assert!(shown.image_pixels(2).is_some(), "it's on the screen, so it comes back");
    assert_eq!(rig.owed(), None);
}

#[test]
fn a_repaint_between_commands_leaves_a_load_arriving_alone() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    rig.output(b"\x1b_Ga=T,f=32,s=2,v=1,i=2,p=2,c=2,r=1,C=1,q=2,m=1;/wAA\x1b\\");
    shown.write(&read(&viewer, channel));
    rig.viewer_misses_output();
    rig.core.prepare_repaints(&viewer);
    let paint = read(&viewer, channel);
    assert!(leaves_images_alone(&paint));
    shown.write(&paint);
    rig.output(b"\x1b_Gm=0;//8AAP8=\x1b\\");
    shown.write(&read(&viewer, channel));
    assert!(shown.image_pixels(2).is_some());
}

#[test]
fn the_encoder_has_nothing_to_do_with_nobody_to_show_it_to() {
    let rig = Rig::new();
    rig.core.lock().screens[&rig.id].set_inline_pixels(0);
    rig.output(IMAGE);
    assert!(rig.core.next_encode(rig.id).is_none());
    let (_viewer, _) = rig.viewer();
    assert!(rig.core.next_encode(rig.id).is_some());
}

#[test]
fn a_read_does_not_drop_queued_text_that_moved_an_image() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b[10;1H", IMAGE]);
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    shown.write(&rig.core.lock().screens[&rig.id].paint(0));
    rig.output(b"\x1b[24;1H\r\n\r\n");
    assert!(!viewer.outbox().images_queued(channel), "only scrolling text queued");
    note_places(&mut rig.core.lock(), rig.id);
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    shown.write(&read(&viewer, channel));
    let expected = rig.core.lock().screens[&rig.id].image_placements()[0].row;
    assert_eq!(shown.image_placements()[0].row, expected);
}

#[test]
fn a_read_does_not_forget_image_bytes_still_queued_after_a_frame() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    let body = ghostty_vt::testing::base64(&[1u8; 40_000]);
    let parts: Vec<_> = body.as_bytes().chunks(4096).collect();
    let mut upload = Vec::new();
    for (n, part) in parts.iter().enumerate() {
        let more = (n + 1 < parts.len()) as u8;
        let head = if n == 0 { format!("a=t,f=32,s=100,v=100,i=5,q=2,m={more}") } else { format!("q=2,m={more}") };
        upload.extend(format!("\x1b_G{head};").as_bytes());
        upload.extend_from_slice(part);
        upload.extend(b"\x1b\\");
    }
    rig.output(&upload);
    let frame = viewer.outbox().next_frame().unwrap();
    assert_eq!(frame[4], wire::DATA);
    shown.write(&frame[wire::HEADER..]);
    assert!(viewer.outbox().queued(channel) > 0);
    note_places(&mut rig.core.lock(), rig.id);
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    shown.write(&read(&viewer, channel));
    rig.output(IMAGE);
    shown.write(&read(&viewer, channel));
    assert!(shown.image_pixels(1).is_some(), "the half-sent load swallowed the next image");
}

#[test]
fn a_program_ending_brings_images_even_on_a_line_holding_them_back() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    rig.overflow();
    viewer.with(|out| out.hold_images_until(channel, Some(Instant::now() + Duration::from_secs(30))));
    rig.core.record_finish(&mut rig.core.lock(), rig.id, proto::Exit { code: Some(0), signal: None }, "exited");
    assert!(carries_images(&read(&viewer, channel)), "there is no later paint after CLOSE");
}

#[test]
fn a_repaint_replays_a_partial_character_once() {
    let (rig, viewer, channel) = Rig::late(&[b"hello \xf0\x9f"]);
    rig.viewer_misses_output();
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    shown.write(&read(&viewer, channel));
    rig.output(b"\x98\x80!");
    shown.write(&read(&viewer, channel));
    assert_eq!(shown.plain_text(), rig.core.lock().screens[&rig.id].plain_text());
}

/// A viewer behind on output, about to have it dropped by a resize it's told of.
fn resized_with_this_unread(rig: &Rig, bytes: &[u8]) -> (Arc<Conn>, u32) {
    let (leader, channel) = rig.viewer();
    let (observer, observed) = rig.viewer_as(true, true);
    rig.output(IMAGE);
    read(&observer, observed);
    read(&leader, channel);
    rig.output(bytes);
    rig.core.viewer_resize(&leader, channel, 100, 30);
    (observer, observed)
}

#[test]
fn image_commands_a_resize_drops_are_made_up_for_though_no_placement_is_left() {
    for bytes in [&b"\x1b_Ga=d,d=a,q=2\x1b\\"[..], b"\x1b_Ga=d,d=A,q=2\x1b\\", b"\x1b[2J", b"\x1b[H\x1b[2J\x1b[3J"] {
        let rig = Rig::new();
        let (observer, observed) = resized_with_this_unread(&rig, bytes);
        assert_eq!(rig.core.lock().screens[&rig.id].image_placements().len(), 0, "or this isn't the case it's about");
        assert!(!rig.core.lock().viewers[&rig.id][1].whole, "{bytes:?}");
        rig.rested();
        rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].resized_at = None;
        rig.core.prepare_repaints(&observer);
        let paint = read(&observer, observed);
        assert!(!leaves_images_alone(&paint), "it still has the image: {bytes:?}");
    }
}

#[test]
fn the_start_of_a_load_a_resize_drops_is_made_up_for() {
    let rig = Rig::new();
    let (observer, observed) = resized_with_this_unread(&rig, b"\x1b_Ga=t,f=32,s=1,v=1,i=5,m=1,q=2;AAAA\x1b\\");
    assert!(!rig.core.lock().viewers[&rig.id][1].whole);
    rig.output(b"\x1b_Gm=0;AA==\x1b\\");
    rig.output(b"\x1b_Ga=p,i=5,p=1,c=2,r=1,C=1,q=2\x1b\\");
    rig.rested();
    rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].resized_at = None;
    rig.core.prepare_repaints(&observer);
    assert!(count(&read(&observer, observed), b"a=t,f=100,i=5,") == 1);
}

#[test]
fn a_viewer_seeing_only_the_last_of_many_resizes_gets_images_that_left_the_screen_on_the_way() {
    let (rig, leader, channel) = Rig::late(&[b"\x1b[3;1H", IMAGE, b"\x1b[20;1H"]);
    let (observer, observed) = rig.viewer_as(true, true);
    read(&observer, observed);
    rig.core.viewer_resize(&leader, channel, 80, 10);
    {
        let st = rig.core.lock();
        assert!(!st.screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
        assert_eq!(st.viewers[&rig.id][1].places, None, "the first step had one, if not the last");
    }
    rig.core.viewer_resize(&leader, channel, 76, 10);
    assert!(!rig.core.lock().viewers[&rig.id][1].whole);
    rig.rested();
    rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].resized_at = None;
    rig.core.prepare_repaints(&observer);
    assert!(!leaves_images_alone(&read(&observer, observed)));
}

#[test]
fn a_resize_that_brings_an_image_back_into_view_leaves_a_viewer_to_be_painted_in_full() {
    let rig = Rig::new();
    let (leader, channel) = rig.viewer();
    let (observer, observed) = rig.viewer_as(true, true);
    rig.output(IMAGE);
    rig.output(&b"\r\n".repeat(30));
    read(&observer, observed);
    assert!(!rig.core.lock().screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
    rig.core.viewer_resize(&leader, channel, 80, 60);
    let st = rig.core.lock();
    assert!(st.screens[&rig.id].direct_images_in_view());
    assert_eq!(st.viewers[&rig.id][1].places, None);
}

#[test]
fn a_repaint_left_waiting_on_a_slow_line_wakes_the_writer_to_time_it() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    {
        let mut st = rig.core.lock();
        note_places(&mut st, rig.id);
        st.stitching.insert(rig.id);
    }
    rig.output(IMAGE);
    rig.core.lock().stitching.remove(&rig.id);
    assert!(!rig.with_viewer(|v| v.whole));
    viewer.with(|out| out.hold_images_until(channel, Some(Instant::now() + Duration::from_secs(30))));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let notified =
        || runtime.block_on(async { tokio::time::timeout(Duration::ZERO, viewer.wake.notified()).await.is_ok() });
    while notified() {}
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    assert!(rig.with_viewer(|v| v.repaint_pending));
    assert!(notified());
}

#[test]
fn the_ceiling_stands_however_long_a_resize_goes_on() {
    let (rig, viewer, channel) = Rig::late(&[&[b'x'; 100], IMAGE]);
    rig.core.viewer_resize(&viewer, channel, 100, 24);
    let original = Instant::now() - IMAGES_AT_MOST;
    rig.with_viewer(|v| v.owed.as_mut().unwrap().since = original);
    rig.core.viewer_resize(&viewer, channel, 120, 24);
    rig.flowing();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)));
}

#[test]
fn a_read_waiting_for_credit_still_leaves_a_viewer_its_images() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(b"\x1b_Ga=t,f=32,s=1,v=1,i=9,q=2;AAAAAA==\x1b\\");
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    shown.write(&read(&viewer, channel));
    assert!(shown.image_pixels(9).is_some());
    viewer.with(|out| {
        out.data(channel, &vec![0; wire::WINDOW]);
        while out.next_frame().is_some() {}
    });
    assert!(!viewer.outbox().can_paint(channel));
    note_places(&mut rig.core.lock(), rig.id);
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    assert!(rig.with_viewer(|v| v.repaint_pending));
    viewer.with(|out| out.credit(channel, wire::WINDOW));
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    shown.write(&read(&viewer, channel));
    assert!(shown.image_pixels(9).is_some());
}

#[test]
fn the_first_chunk_of_a_load_a_resize_drops_is_made_up_for_with_no_placement_anywhere() {
    let rig = Rig::new();
    let (leader, leader_channel) = rig.viewer();
    let (observer, observed) = rig.viewer_as(true, true);
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    rig.output(CHUNK_FIRST);
    assert!(rig.core.lock().screens[&rig.id].image_placements().is_empty());
    rig.core.viewer_resize(&leader, leader_channel, 100, 30);
    shown.resize(100, 30);
    rig.rested();
    rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].resized_at = None;
    rig.core.prepare_repaints(&observer);
    shown.write(&read(&observer, observed));
    rig.output(CHUNK_LAST);
    shown.write(&read(&observer, observed));
    rig.due(&observer, observed);
    shown.write(&read(&observer, observed));
    assert!(rig.core.lock().screens[&rig.id].image_pixels(2).is_some());
    assert!(shown.image_pixels(2).is_some());
}

#[test]
fn a_read_kept_on_the_alternate_screen_leaves_what_the_main_one_had() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    rig.output(b"\x1b_Ga=t,f=32,s=1,v=1,i=5,q=2;AAAAAA==\x1b\\");
    rig.output(IMAGE);
    shown.write(&read(&viewer, channel));
    assert!(shown.image_pixels(5).is_some(), "or this isn't the case it's about");
    rig.output(b"\x1b[?1049h");
    shown.write(&read(&viewer, channel));
    note_places(&mut rig.core.lock(), rig.id);
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    let paint = read(&viewer, channel);
    assert!(!carries_images(&paint), "nor this");
    shown.write(&paint);
    rig.output(b"\x1b[?1049l");
    shown.write(&read(&viewer, channel));
    assert_eq!(rig.owed(), None);
    assert!(shown.image_pixels(5).is_some());
}
