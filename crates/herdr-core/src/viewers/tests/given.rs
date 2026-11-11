use super::*;
use ghostty_vt::testing::{chunked, noise};
use std::time::Instant;

pub(super) const CACHED: &[u8] = b"\x1b_Ga=t,f=32,s=1,v=1,i=5,q=2;AAAAAA==\x1b\\";
pub(super) const PLACE_CACHED: &[u8] = b"\x1b_Ga=p,i=5,p=1,c=2,r=1,C=1,q=2\x1b\\";

impl Rig {
    /// Owing everything, as since `since`, with a paint that carried these.
    pub(super) fn owe_since(&self, since: Instant, carried: Option<u64>) {
        self.with_viewer(|v| {
            v.owed = Some(Owed { since, all: true, finished: 0, carried, layout: false });
            v.whole = false;
        });
    }

    pub(super) fn walks(&self) -> u64 {
        self.core.lock().screens[&self.id].placement_walks()
    }

    pub(super) fn carried(&self) -> u64 {
        self.core.lock().screens[&self.id].images_carried().images
    }

    pub(super) fn ready_carried(&self) -> u64 {
        self.core.lock().screens[&self.id].images_ready();
        self.carried()
    }
}

#[track_caller]
fn image_comes(rig: &Rig, viewer: &Arc<Conn>, channel: u32) {
    image_comes_for("", rig, viewer, channel);
}

#[track_caller]
fn image_comes_for(what: &str, rig: &Rig, viewer: &Arc<Conn>, channel: u32) {
    rig.rested();
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.prepare_repaints(viewer);
    assert_eq!(count(&read(viewer, channel), b"a=t,f=100,i=5,"), 1, "{what}");
    assert_eq!(rig.owed(), None, "{what}");
}

#[test]
fn an_image_placed_after_a_viewer_attached_reaches_it_though_its_paint_had_no_room_for_it() {
    let (rig, viewer, channel) = Rig::late(&[CACHED]);
    rig.output(PLACE_CACHED);
    assert!(!rig.with_viewer(|v| v.whole));
    assert_eq!(rig.owed(), Some(true));
    image_comes(&rig, &viewer, channel);
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn an_image_out_of_view_when_a_viewer_attached_reaches_it_when_placed_again() {
    let (rig, viewer, channel) =
        Rig::late(&[b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\", &b"\r\n".repeat(30)]);
    rig.output(PLACE_CACHED);
    image_comes(&rig, &viewer, channel);
}

#[test]
fn an_image_a_full_paint_cleared_reaches_a_viewer_when_placed_again() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(CACHED);
    read(&viewer, channel);
    rig.needs_a_full_paint();
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    read(&viewer, channel);
    rig.output(PLACE_CACHED);
    image_comes(&rig, &viewer, channel);
}

#[test]
fn a_viewer_that_saw_an_image_arrive_needs_nothing_when_it_is_placed() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(CACHED);
    rig.output(PLACE_CACHED);
    rig.output(b"\x1b[5;1H");
    rig.output(b"\x1b_Ga=p,i=5,p=2,c=2,r=1,C=1,q=2\x1b\\");
    assert_eq!(rig.owed(), None);
    assert!(rig.with_viewer(|v| v.whole));
    read(&viewer, channel);

    let (late, late_channel) = rig.viewer_as(false, false);
    rig.output(b"\x1b[9;1H\x1b_Ga=p,i=5,p=3,c=2,r=1,C=1,q=2\x1b\\");
    assert!(rig.core.lock().viewers[&rig.id][1].owed.is_none(), "its paint carried it");
    assert!(!read(&late, late_channel).is_empty());
}

#[test]
fn an_image_no_paint_has_room_for_is_not_owed_for_ever() {
    let rig = Rig::new();
    rig.output(&chunked("a=t,f=32,s=2000,v=1000,i=5,q=2", &noise(8_000_000)));
    assert!(rig.core.lock().screens[&rig.id].image_pixels(5).is_some(), "or this isn't the case it's about");
    let (viewer, channel) = rig.viewer();
    let paints = || PAINTS.with(|n| n.get());
    rig.output(PLACE_CACHED);
    rig.rested();
    for _ in 0..3 {
        rig.encoder_ran();
        rig.core.prepare_repaints(&viewer);
    }
    read(&viewer, channel);
    let before = paints();
    assert_eq!(rig.owed(), None, "it can't come");
    assert!(rig.core.lock().screens[&rig.id].images_carried().refused, "or this isn't the case it's about");
    rig.output(b"\x1b[9;1H\x1b_Ga=p,i=5,p=3,c=2,r=1,C=1,q=2\x1b\\");
    rig.due(&viewer, channel);
    assert_eq!(paints(), before, "nor by asking again");
    assert_eq!(rig.owed(), None);
}

#[test]
fn going_back_to_a_screen_a_viewer_never_had_brings_its_images() {
    for (what, control) in
        [("images", "a=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2"), ("placeholder images", "a=T,f=32,s=1,v=1,U=1,i=5,c=2,r=1,q=2")]
    {
        let image = format!("\x1b_G{control};AAAAAA==\x1b\\");
        let (rig, viewer, channel) = Rig::late(&[image.as_bytes(), b"\x1b[?1049h\x1b[Hfull screen"]);
        rig.output(b"\x1b[?1049l");
        assert_eq!(rig.owed(), Some(true), "{what}");
        image_comes_for(what, &rig, &viewer, channel);
    }
}

#[test]
fn leaving_for_the_alternate_screen_and_back_owes_nothing() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\"]);
    rig.output(b"\x1b[?1049h\x1b_Ga=T,f=32,s=1,v=1,i=6,c=2,r=1,q=2;AAAAAA==\x1b\\");
    assert_eq!(rig.owed(), None, "it saw that one arrive");
    assert!(rig.with_viewer(|v| v.whole));
    rig.output(b"\x1b[?1049l");
    assert_eq!(rig.owed(), None, "it had this one all along");
    assert!(rig.with_viewer(|v| v.whole));
    read(&viewer, channel);
}

#[test]
fn a_paint_from_the_alternate_screen_means_the_main_ones_images_are_owed_on_return() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\"]);
    rig.output(b"\x1b[?1049h");
    rig.needs_a_full_paint();
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    read(&viewer, channel);
    rig.output(b"\x1b[?1049l");
    image_comes(&rig, &viewer, channel);
}

#[test]
fn growing_brings_an_image_a_viewer_attached_too_late_for_back_into_its_view() {
    let (rig, leader, channel) =
        Rig::late(&[b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\", &b"\r\n".repeat(30)]);
    assert_eq!(rig.owed(), None);
    rig.core.viewer_resize(&leader, channel, 80, 60);
    assert!(rig.core.lock().screens[&rig.id].direct_images_in_view(), "or this isn't the case it's about");
    assert_eq!(rig.owed(), Some(true));
    rig.with_viewer(|v| v.resized_at = None);
    image_comes(&rig, &leader, channel);
}

#[test]
fn an_image_placed_while_another_load_is_stuck_still_comes() {
    let (rig, viewer, channel) = Rig::late(&[CACHED, b"\x1b_Ga=t,f=32,s=1,v=1,i=9,m=1,q=2;AAAA\x1b\\"]);
    assert_eq!(rig.owed(), Some(false), "only that one, and it may never finish");
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), Some(true));
    rig.past_the_ceiling();
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.prepare_repaints(&viewer);
    assert_eq!(count(&read(&viewer, channel), b"a=t,f=100,i=5,"), 1);
}

#[test]
fn a_paint_that_left_a_viewers_images_alone_left_it_all_it_had() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    rig.output(CACHED);
    read(&viewer, channel);
    note_places(&mut rig.core.lock(), rig.id);
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    assert!(leaves_images_alone(&read(&viewer, channel)));
    rig.output(b"\x1b_Ga=p,i=1,p=7,c=2,r=1,C=1,q=2\x1b\\");
    assert_eq!(rig.owed(), None, "it was given this one");
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), None, "and saw this one arrive");
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn an_image_a_full_paint_after_a_read_cleared_reaches_a_viewer_when_placed_again() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(CACHED);
    read(&viewer, channel);
    rig.with_viewer(|v| v.whole = false);
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    assert!(!leaves_images_alone(&read(&viewer, channel)));
    rig.output(PLACE_CACHED);
    image_comes(&rig, &viewer, channel);
}

#[test]
fn an_image_a_recovery_cleared_reaches_a_viewer_when_placed_again() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(CACHED);
    read(&viewer, channel);
    rig.output(b"\x1b]0;");
    rig.output(&vec![b'A'; 70 * 1024]);
    rig.with_viewer(|v| v.recovering = true);
    read(&viewer, channel);
    rig.output(b"AA\x1b\\");
    assert!(!rig.with_viewer(|v| v.recovering));
    read(&viewer, channel);
    assert_eq!(rig.owed(), None, "no load is holding this up");
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), Some(true));
    rig.due(&viewer, channel);
    assert_eq!(count(&read(&viewer, channel), b"a=t,f=100,i=5,"), 1);
}

#[test]
fn an_image_out_of_view_is_nothing_to_owe_on_going_back_to_its_screen() {
    let (rig, _viewer, _) = Rig::late(&[IMAGE, &b"\r\n".repeat(30)]);
    rig.output(b"\x1b[?1049h\x1b[?1049l");
    assert_eq!(rig.owed(), None);
    rig.output(b"\x1b[?1049h");
    rig.output(b"\x1b[?1049l");
    assert_eq!(rig.owed(), None);
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn every_image_a_paint_carried_counts_as_given_whatever_order_they_came_in() {
    let rig = Rig::new();
    for id in [5, 6, 7] {
        rig.output(format!("\x1b_Ga=T,f=32,s=1,v=1,i={id},c=2,r=1,C=1,q=2;AAAAAA==\x1b\\").as_bytes());
    }
    let (viewer, channel) = rig.viewer();
    read(&viewer, channel);
    for id in [5, 6, 7] {
        rig.output(format!("\x1b_Ga=p,i={id},p=9,c=2,r=1,C=1,q=2\x1b\\").as_bytes());
        assert_eq!(rig.owed(), None, "image {id} was in its paint");
    }
}

#[test]
fn an_id_past_what_a_placeholder_cell_can_say_is_still_an_id() {
    let rig = Rig::new();
    let id = 0x1000005u32;
    rig.output(format!("\x1b_Ga=t,f=32,s=1,v=1,i={id},q=2;AAAAAA==\x1b\\").as_bytes());
    let (viewer, channel) = rig.viewer();
    rig.output(format!("\x1b_Ga=p,i={id},c=2,r=1,C=1,q=2\x1b\\").as_bytes());
    assert_eq!(rig.owed(), Some(true));
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert_eq!(count(&read(&viewer, channel), format!("a=t,f=100,i={id},").as_bytes()), 1);
}

#[test]
fn placing_an_image_again_does_not_put_it_off() {
    let (rig, viewer, channel) = Rig::late(&[CACHED]);
    rig.output(PLACE_CACHED);
    rig.past_the_ceiling();
    rig.output(PLACE_CACHED);
    rig.flowing();
    viewer.with(|out| out.hold_images_until(channel, None));
    rig.core.prepare_repaints(&viewer);
    assert_eq!(count(&read(&viewer, channel), b"a=t,f=100,i=5,"), 1);
}

#[test]
fn an_image_that_was_arriving_when_a_viewer_came_reaches_it_when_placed() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b_Ga=t,f=32,s=1,v=1,i=5,m=1,q=2;AAAA\x1b\\"]);
    rig.output(b"\x1b_Gm=0;AA==\x1b\\");
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert_eq!(rig.owed(), None, "nothing to show yet");
    assert!(leaves_images_alone(&read(&viewer, channel)) || rig.with_viewer(|v| v.whole));
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), Some(true));
    image_comes(&rig, &viewer, channel);
}

#[test]
fn an_image_kept_on_the_alternate_screen_reaches_a_viewer_on_going_back_there() {
    for (enter, leave, control) in [
        (&b"\x1b[?47h"[..], &b"\x1b[?47l"[..], "a=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2"),
        (b"\x1b[?1049h", b"\x1b[?1049l", "a=T,f=32,s=1,v=1,U=1,i=5,c=2,r=1,q=2"),
    ] {
        let (rig, viewer, channel) = Rig::late(&[enter, format!("\x1b_G{control};AAAAAA==\x1b\\").as_bytes(), leave]);
        assert_eq!(rig.owed(), None);
        rig.output(enter);
        assert_eq!(rig.owed(), Some(true), "{control}");
        image_comes_for(control, &rig, &viewer, channel);
    }
}

#[test]
fn a_load_ended_by_can_reaches_a_viewer_that_came_during_it() {
    for end in [0x18u8, 0x1a] {
        let (rig, viewer, channel) = Rig::late(&[b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,C=1,q=2,m=1;AAAA\x1b\\"]);
        let mut last = b"\x1b_Gm=0;AA==".to_vec();
        last.push(end);
        rig.output(&last);
        image_comes(&rig, &viewer, channel);
    }
}

#[test]
fn an_image_sent_again_comes_with_the_stream() {
    let (rig, _viewer, _) = Rig::late(&[CACHED]);
    rig.output(CACHED);
    assert_eq!(rig.owed(), None, "the one it never got has gone, and it's seeing this one");
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), None);
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn an_image_arriving_in_time_on_the_alternate_screen_owes_nothing() {
    let (rig, _viewer, _) = Rig::late(&[b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\"]);
    rig.output(b"\x1b[?1049h");
    rig.output(CACHED);
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), None);
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn going_back_to_a_main_screen_a_viewer_never_had_brings_its_text() {
    let (rig, viewer, channel) = Rig::late(&[b"on the main screen\r\n\x1b[?1049h\x1b[Hfull screen"]);
    rig.output(b"\x1b[?1049l");
    assert_eq!(rig.owed(), Some(true));
    read(&viewer, channel);
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    shown.write(&read(&viewer, channel));
    assert!(shown.plain_text().contains("on the main screen"));
    assert_eq!(rig.owed(), None);
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn another_viewers_paint_is_no_proof_of_what_this_one_has() {
    let (rig, first, first_channel) = Rig::late(&[CACHED]);
    rig.output(PLACE_CACHED);
    let (_second, _) = rig.viewer();
    rig.output(b"\x1b_Ga=d,d=i,i=5,q=2\x1b\\");
    rig.due(&first, first_channel);
    read(&first, first_channel);
    assert_eq!(rig.owed(), None, "nothing is placed now");
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), Some(true), "and it has never had this one");
    image_comes(&rig, &first, first_channel);
}

#[test]
fn settling_on_the_alternate_screen_leaves_what_it_has_of_the_main_one() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    rig.output(b"\x1b[?1049h");
    rig.output(b"\x1b_Ga=t,f=32,s=1,v=1,i=9,m=1,q=2;AAAA\x1b\\");
    let carried = rig.ready_carried();
    rig.with_viewer(|v| {
        v.owed = Some(Owed { since: Instant::now(), all: true, finished: 0, carried: Some(carried), layout: false });
    });
    rig.output(b"\x1b_Gm=0;AA==\x1b\\");
    read(&viewer, channel);
    rig.due(&viewer, channel);
    assert_eq!(rig.owed(), None);
    assert!(rig.with_viewer(|v| v.given[0].is_some()));
    rig.output(b"\x1b[?1049l");
    assert_eq!(rig.owed(), None, "it never lost that one");
    assert!(rig.with_viewer(|v| v.whole));
}

#[test]
fn settling_leaves_a_viewers_record_as_it_is_but_for_a_load_it_was_cut_off_from() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(CACHED);
    let carried = rig.ready_carried();
    rig.with_viewer(|v| {
        v.owed = Some(Owed { since: Instant::now(), all: true, finished: 0, carried: Some(carried), layout: false })
    });
    rig.due(&viewer, channel);
    assert_eq!(rig.owed(), None);
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), None, "it saw that one arrive, whatever has settled since");
}

#[test]
fn typing_at_the_size_it_has_needs_no_look_at_the_images() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    rig.core.viewer_facts(&viewer, channel, Display { cell: Some([10, 20]), ..Display::default() });
    let walks = rig.walks();
    rig.core.viewer_input(&viewer, channel, b"k", false);
    assert_eq!(rig.walks(), walks);
    assert_eq!(rig.owed(), None);
}

#[test]
fn an_image_after_one_a_viewer_has_is_still_owed_if_it_lacks_it() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE, b"\x1b_Ga=t,f=32,s=1,v=1,i=2,q=2;AAAAAA==\x1b\\"]);
    assert_eq!(rig.owed(), None);
    rig.output(b"\x1b_Ga=p,i=1,c=2,r=1,C=1,q=2\x1b\\\x1b_Ga=p,i=2,c=2,r=1,C=1,q=2\x1b\\");
    read(&viewer, channel);
    assert_eq!(rig.owed(), Some(true));
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)));

    // Or before it.
    let (rig, _viewer, _) = Rig::late(&[IMAGE, b"\x1b_Ga=t,f=32,s=1,v=1,i=2,q=2;AAAAAA==\x1b\\"]);
    rig.output(b"\x1b_Ga=p,i=2,c=2,r=1,C=1,q=2\x1b\\\x1b_Ga=p,i=1,c=2,r=1,C=1,q=2\x1b\\");
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn the_encoder_finishing_wakes_every_viewers_writer() {
    let take_wake = |conn: &Conn| {
        let mut notified = Box::pin(conn.wake.notified());
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        std::future::Future::poll(notified.as_mut(), &mut cx).is_ready()
    };
    let rig = Rig::new();
    rig.core.lock().screens[&rig.id].set_inline_pixels(0);
    rig.output(IMAGE);
    let (first, _) = rig.viewer();
    let (second, _) = rig.viewer();
    take_wake(&first);
    take_wake(&second);
    rig.core.encoding_finished(rig.id);
    assert!(take_wake(&first));
    assert!(take_wake(&second));
}

#[test]
fn a_paint_of_the_main_screen_leaves_what_it_has_of_the_other() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b[?47h", IMAGE]);
    rig.output(b"\x1b[?47l");
    read(&viewer, channel);
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    read(&viewer, channel);
    assert_eq!(rig.owed(), None);
    rig.output(b"\x1b[?47h");
    assert_eq!(rig.owed(), None);
}

#[test]
fn output_missed_on_a_screen_means_nothing_can_be_said_of_what_a_viewer_has_of_it() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b[?47h", IMAGE]);
    rig.output(b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\\x1b[?47l");
    rig.overflow();
    assert!(rig.with_viewer(|v| v.repaint_pending), "or this isn't the case it's about");
    assert!(rig.with_viewer(|v| v.given[1].is_none()));
    rig.rested();
    viewer.with(|out| {
        while out.next_frame().is_some() {}
        out.credit(channel, wire::MAX_PENDING);
        out.hold_images_until(channel, None);
    });
    rig.core.prepare_repaints(&viewer);
    read(&viewer, channel);
    rig.output(b"\x1b[?47h");
    assert_eq!(rig.owed(), Some(true));
}

#[test]
fn output_going_by_a_viewer_with_a_repaint_coming_leaves_nothing_known_of_the_screen_it_changed() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b[?47h", IMAGE]);
    read(&viewer, channel);
    rig.with_viewer(|v| v.repaint_pending = true);
    rig.output(b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\\x1b[?47l");
    rig.with_viewer(|v| {
        assert!(v.given[1].is_none());
        assert!(v.given[0].is_some() || !v.whole, "this screen it gets from the repaint");
    });
}
