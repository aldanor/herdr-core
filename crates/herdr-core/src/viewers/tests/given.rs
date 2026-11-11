use super::*;

use std::time::Instant;

pub(super) const CACHED: &[u8] = b"\x1b_Ga=t,f=32,s=1,v=1,i=5,q=2;AAAAAA==\x1b\\";
pub(super) const PLACE_CACHED: &[u8] = b"\x1b_Ga=p,i=5,p=1,c=2,r=1,C=1,q=2\x1b\\";

impl Rig {
    /// Owing everything, as since `since`, with a paint that carried these.
    pub(super) fn owe_since(&self, since: Instant, carried: Option<u64>) {
        self.with_viewer(|v| {
            v.owed = Some(Owed { since, carried, spare: false });
        });
    }

    pub(super) fn walks(&self) -> u64 {
        self.core.lock().screens[&self.id].placement_walks()
    }

    pub(super) fn carried(&self) -> u64 {
        self.core.lock().screens[&self.id].images_carried().images
    }

    pub(super) fn ready_carried(&self) -> u64 {
        self.core.lock().screens[&self.id].look_at_images();
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
    rig.core.prepare_repaints(viewer);
    assert_eq!(count(&read(viewer, channel), b"a=t,f=100,i=5,"), 1, "{what}");
    assert_eq!(rig.owed(), None, "{what}");
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
    read(&viewer, channel);

    let (late, late_channel) = rig.viewer_as(false, false);
    rig.output(b"\x1b[9;1H\x1b_Ga=p,i=5,p=3,c=2,r=1,C=1,q=2\x1b\\");
    assert!(rig.core.lock().viewers[&rig.id][1].owed.is_none(), "its paint carried it");
    assert!(!read(&late, late_channel).is_empty());
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
    rig.output(b"\x1b[?1049l");
    assert_eq!(rig.owed(), None, "it had this one all along");
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
fn an_image_out_of_view_is_nothing_to_owe_on_going_back_to_its_screen() {
    let (rig, _viewer, _) = Rig::late(&[IMAGE, &b"\r\n".repeat(30)]);
    rig.output(b"\x1b[?1049h\x1b[?1049l");
    assert_eq!(rig.owed(), None);
    rig.output(b"\x1b[?1049h");
    rig.output(b"\x1b[?1049l");
    assert_eq!(rig.owed(), None);
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
fn an_image_sent_again_comes_with_the_stream() {
    let (rig, _viewer, _) = Rig::late(&[CACHED]);
    rig.output(CACHED);
    assert_eq!(rig.owed(), None, "the one it never got has gone, and it's seeing this one");
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), None);
}

#[test]
fn an_image_arriving_in_time_on_the_alternate_screen_owes_nothing() {
    let (rig, _viewer, _) = Rig::late(&[b"\x1b_Ga=T,f=32,s=1,v=1,i=5,c=2,r=1,q=2;AAAAAA==\x1b\\"]);
    rig.output(b"\x1b[?1049h");
    rig.output(CACHED);
    rig.output(PLACE_CACHED);
    assert_eq!(rig.owed(), None);
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
}

#[test]
fn settling_on_the_alternate_screen_leaves_what_it_has_of_the_main_one() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    rig.output(b"\x1b[?1049h");
    rig.output(b"\x1b_Ga=t,f=32,s=1,v=1,i=9,m=1,q=2;AAAA\x1b\\");
    let carried = rig.ready_carried();
    rig.with_viewer(|v| {
        v.owed = Some(Owed { since: Instant::now(), carried: Some(carried), spare: false });
    });
    rig.output(b"\x1b_Gm=0;AA==\x1b\\");
    read(&viewer, channel);
    rig.due(&viewer, channel);
    assert_eq!(rig.owed(), None);
    rig.output(b"\x1b[?1049l");
    assert_eq!(rig.owed(), None, "it never lost that one");
}

#[test]
fn settling_leaves_a_viewers_record_as_it_is_but_for_a_load_it_was_cut_off_from() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(CACHED);
    let carried = rig.ready_carried();
    rig.with_viewer(|v| v.owed = Some(Owed { since: Instant::now(), carried: Some(carried), spare: false }));
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
