use super::*;
use std::time::Instant;

#[test]
fn a_clear_a_viewer_misses_is_a_change_to_its_images() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(b"\x1b_Ga=t,f=32,s=1,v=1,i=9,q=2;AAAAAA==\x1b\\");
    read(&viewer, channel);
    rig.viewer_misses_output();
    rig.output(b"\x1b[2J");
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
    rig.core.prepare_repaints(&observer);
    assert!(carries_images(&read(&observer, observed)), "none of the first one got through");
}

#[test]
fn a_paint_with_images_counts_as_queued_until_it_has_gone() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    rig.needs_a_full_paint();
    rig.due(&viewer, channel);
    assert!(carries_images(&read(&viewer, channel)));
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
            v.owed = Some(Owed { since: Instant::now(), carried: Some(carried), spare: false });
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
    rig.output(b"\x1b_Ga=p,i=1,q=2");
    rig.output(b"\x1b\\");
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)));
}

#[test]
fn a_viewer_whose_backlog_went_may_have_lost_images_with_it() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    read(&viewer, channel);
    rig.overflow();
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)));
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

    rig.output(b"\x1b_Ga=T,f=32,s=1,v=1,i=9,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\");
    read(&viewer, channel);
    rig.owe_since(Instant::now(), Some(carried));
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 1, "new ones do need it");
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

#[test]
fn a_read_replacing_queued_image_commands_brings_the_images() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(IMAGE);
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    assert!(carries_images(&read(&viewer, channel)));
}

#[test]
fn a_viewer_missing_only_the_end_of_an_image_command_has_missed_it() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.output(b"\x1b_Ga=p,i=1,q=2");
    read(&viewer, channel);
    rig.with_viewer(|v| v.repaint_pending = true);
    rig.output(b"\x1b\\");
}

#[test]
fn where_images_were_is_only_good_until_the_next_paint() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    read(&viewer, channel);
    rig.viewer_misses_output();
    rig.core.prepare_repaints(&viewer);
}

#[test]
fn a_read_does_not_drop_queued_text_that_moved_an_image() {
    let (rig, viewer, channel) = Rig::late(&[b"\x1b[10;1H", IMAGE]);
    let mut shown = ghostty_vt::Terminal::new(80, 24, 1 << 20);
    shown.write(&rig.core.lock().screens[&rig.id].paint(0));
    rig.output(b"\x1b[24;1H\r\n\r\n");
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
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    shown.write(&read(&viewer, channel));
    rig.output(IMAGE);
    shown.write(&read(&viewer, channel));
    assert!(shown.image_pixels(1).is_some(), "the half-sent load swallowed the next image");
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
    rig.core.repaint_after_read(&mut rig.core.lock(), rig.id);
    assert!(rig.with_viewer(|v| v.repaint_pending));
    viewer.with(|out| out.credit(channel, wire::WINDOW));
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    shown.write(&read(&viewer, channel));
    assert!(shown.image_pixels(9).is_some());
}
