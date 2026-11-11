use super::*;
use crate::wire;
use ghostty_vt::testing::count;
use std::cell::Cell;

mod given;
mod images;
mod resizes;

thread_local! {
    pub(crate) static PAINTS: Cell<usize> = const { Cell::new(0) };
}

#[test]
fn resize_drags_serialize_nothing_without_observers_and_only_latest_when_stalled() {
    let root = crate::test_tmp().join(format!("hc-resize-{}", crate::platform::random_hex()));
    let paths = crate::paths::Paths {
        socket: root.join("core.sock"),
        holder_socket: root.join("holder.sock"),
        state: root.clone(),
        config: root.join("config.toml"),
    };
    paths.create_dirs().unwrap();
    let core = Core::start(paths, crate::api::CoreInfo { id: "test".into(), name: "test".into() }).unwrap();
    let id = {
        let mut st = core.lock();
        let id = st
            .reg
            .create(
                crate::terminals::NewTerminal { argv: vec!["sh".into()], cols: 80, rows: 24, ..Default::default() },
                0,
            )
            .unwrap();
        st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
        id
    };
    let leader = Arc::new(Conn::default());
    let channel = core.attach(&leader, id, request(false, false)).unwrap();
    PAINTS.set(0);
    for cols in 81..100 {
        core.viewer_resize(&leader, channel, cols, 24);
    }
    assert_eq!(PAINTS.get(), 0, "desktop-only drags must not serialize paints");
    let observer = Arc::new(Conn::default());
    let observed = core.attach(&observer, id, request(true, true)).unwrap();
    observer.with(|out| {
        out.resume(observed);
        out.data(observed, &vec![b'x'; wire::WINDOW]);
        while out.next_frame().is_some() {}
    });
    PAINTS.set(0);
    for cols in 100..200 {
        core.viewer_resize(&leader, channel, cols, 24);
        core.prepare_repaints(&observer);
    }
    assert_eq!(PAINTS.get(), 0, "no serialization while credit stalled");
    observer.with(|out| {
        assert_eq!(out.queued(observed), 0, "no obsolete paints accumulated");
        assert!(out.next_frame().is_none());
        out.credit(observed, wire::WINDOW);
    });
    core.prepare_repaints(&observer);
    assert_eq!(PAINTS.get(), 1);
    observer.with(|out| {
        let frame = out.next_frame().unwrap();
        assert_eq!(frame[4], wire::RESIZE);
        assert_eq!(&frame[wire::HEADER..], &[199, 0, 24, 0]);
        let frame = out.next_frame().unwrap();
        assert_eq!(frame[4], wire::DATA);
        assert_eq!(frame[wire::HEADER], 0x18, "cancel a partial old-grid sequence");
    });
    core.prepare_repaints(&observer);
    assert_eq!(PAINTS.get(), 1, "unchanged grid is not repainted twice");
    // Exit before the writer next wakes must still deliver the final screen.
    core.viewer_resize(&leader, channel, 200, 24);
    core.record_finish(&mut core.lock(), id, proto::Exit { code: Some(0), signal: None }, "exited");
    observer.with(|out| {
        let size = out.next_frame().expect("final resize");
        assert_eq!(size[4], wire::RESIZE);
        assert_eq!(&size[wire::HEADER..], &[200, 0, 24, 0]);
        assert_eq!(out.next_frame().unwrap()[4], wire::DATA);
        while let Some(frame) = out.next_frame() {
            if frame[4] == wire::CLOSE {
                break;
            }
        }
    });
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}

/// A core with one screen, nothing behind it, and time under a test's control:
/// `flowing` and `rested` say when output last came.
struct Rig {
    core: Arc<Core>,
    id: u32,
    root: std::path::PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        // A last save may still be on its way.
        self.core.lock().viewers.clear();
        for _ in 0..50 {
            let _ = std::fs::remove_dir_all(&self.root);
            if !self.root.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn request(observe: bool, resize_notifications: bool) -> AttachRequest {
    AttachRequest {
        observe,
        resize_notifications,
        history: 0,
        size: (80, 24),
        display: Display::default(),
        resumed: false,
    }
}

const IMAGE: &[u8] = b"\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=1,C=1,q=2;AAAAAA==\x1b\\";

impl Rig {
    fn new() -> Rig {
        let root = std::path::PathBuf::from(format!("/var/tmp/hc-rig-{}", crate::platform::random_hex()));
        let paths = crate::paths::Paths {
            socket: root.join("core.sock"),
            holder_socket: root.join("holder.sock"),
            state: root.clone(),
            config: root.join("config.toml"),
        };
        paths.create_dirs().unwrap();
        let core = Core::start(paths, crate::api::CoreInfo { id: "test".into(), name: "test".into() }).unwrap();
        let id = {
            let mut st = core.lock();
            let new =
                crate::terminals::NewTerminal { argv: vec!["sh".into()], cols: 80, rows: 24, ..Default::default() };
            let id = st.reg.create(new, 0).unwrap();
            st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
            id
        };
        Rig { core, id, root }
    }

    /// A viewer come late to this output.
    fn late(output: &[&[u8]]) -> (Rig, Arc<Conn>, u32) {
        let rig = Rig::new();
        output.iter().for_each(|bytes| rig.output(bytes));
        let (viewer, channel) = rig.viewer();
        (rig, viewer, channel)
    }

    /// Attached, its first paint read, all its credit back.
    fn viewer(&self) -> (Arc<Conn>, u32) {
        self.viewer_as(false, false)
    }

    /// Attached and resumed, with nothing read yet.
    fn attach(&self, request: AttachRequest) -> (Arc<Conn>, u32) {
        let conn = Arc::new(Conn::default());
        let channel = self.core.attach(&conn, self.id, request).unwrap();
        conn.with(|out| out.resume(channel));
        (conn, channel)
    }

    fn viewer_as(&self, observe: bool, resize_notifications: bool) -> (Arc<Conn>, u32) {
        self.rested();
        let (conn, channel) = self.attach(request(observe, resize_notifications));
        read(&conn, channel);
        // However long that took on a slow machine, it doesn't hold back what a test does next.
        (conn, channel)
    }

    /// Viewers on one connection, of these terminals, with their first paints read.
    fn viewers_on(&self, conn: &Arc<Conn>, ids: &[u32]) -> Vec<u32> {
        let channels: Vec<u32> = ids
            .iter()
            .map(|id| {
                let request = request(false, false);
                let channel = self.core.attach(conn, *id, request).unwrap();
                conn.with(|out| out.resume(channel));
                channel
            })
            .collect();
        conn.with(|out| while out.next_frame().is_some() {});
        channels
    }

    fn output(&self, bytes: &[u8]) {
        self.core.output(&mut self.core.lock(), self.id, bytes);
    }

    /// Output as if it's still coming.
    fn flowing(&self) {
        self.core.lock().last_output.insert(self.id, Instant::now() + Duration::from_secs(3600));
    }

    fn rested(&self) {
        self.core.lock().last_output.insert(self.id, Instant::now() - IMAGES_REST);
    }

    /// Output has rested and its line is free: what it's due goes.
    fn due(&self, viewer: &Arc<Conn>, _channel: u32) {
        self.rested();
        self.core.prepare_repaints(viewer);
    }

    fn needs_a_full_paint(&self) {
        self.with_viewer(|v| {
            v.repaint_pending = true;
        });
    }

    fn with_viewer<R>(&self, f: impl FnOnce(&mut Viewer) -> R) -> R {
        self.with_viewer_n(0, f)
    }

    fn with_viewer_n<R>(&self, n: usize, f: impl FnOnce(&mut Viewer) -> R) -> R {
        f(&mut self.core.lock().viewers.get_mut(&self.id).unwrap()[n])
    }

    /// The nth viewer is owed all its images, since then.
    fn owe(&self, n: usize, since: Instant) {
        self.with_viewer_n(n, |v| v.owed = Some(Owed { since, carried: None, spare: false }));
    }

    fn after_read(&self) {
        self.core.repaint_after_read(&mut self.core.lock(), self.id);
    }

    /// A graphics command too long to replay, still going.
    fn open_long_sequence(&self) {
        self.output(b"\x1b_Gm=1;");
        self.output(&vec![b'A'; 70 * 1024]);
    }

    fn owed(&self) -> Option<bool> {
        self.with_viewer(|v| v.owed.map(|_| true))
    }

    fn past_the_ceiling(&self) {
        self.with_viewer(|v| v.owed.as_mut().unwrap().since = Instant::now() - IMAGES_AT_MOST);
    }

    /// However long a slow machine takes, the ceiling isn't what a test sees.
    fn far_from_the_ceiling(&self) -> Instant {
        let far = Instant::now() + Duration::from_secs(3600);
        self.with_viewer(|v| v.owed.as_mut().unwrap().since = far);
        far
    }

    /// It starts missing output, as for a resize or a read.
    fn viewer_misses_output(&self) {
        let mut st = self.core.lock();
        st.viewers.get_mut(&self.id).unwrap().iter_mut().for_each(|v| v.repaint_pending = true);
    }

    /// Overflows a viewer so it falls behind, with nothing read and nothing scrolled.
    fn overflow(&self) {
        self.output(&b"\x1b[9;1Hy".repeat(wire::MAX_PENDING / 7 + 1));
    }
}

/// What a viewer reads, giving the credit back as it would.
fn read(conn: &Arc<Conn>, channel: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    conn.with(|out| {
        while let Some(frame) = out.next_frame() {
            if frame[4] == wire::DATA {
                bytes.extend_from_slice(&frame[wire::HEADER..]);
                out.credit(channel, frame.len() - wire::HEADER);
            }
        }
    });
    bytes
}

/// Whether the viewer's writer has been woken since it last asked.
fn woken(conn: &Arc<Conn>) -> bool {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    runtime.block_on(async { tokio::time::timeout(Duration::ZERO, conn.wake.notified()).await.is_ok() })
}

fn carries_images(paint: &[u8]) -> bool {
    paint.windows(4).any(|w| w == b"a=t,")
}

fn spend_all_credit(conn: &Arc<Conn>, channel: u32) {
    conn.with(|out| {
        out.data(channel, &vec![b'x'; wire::WINDOW]);
        while out.next_frame().is_some() {}
    });
    assert!(!conn.outbox().can_send(channel));
}

#[test]
fn a_viewer_that_falls_behind_is_painted_only_once_it_has_credit() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    spend_all_credit(&viewer, channel);
    PAINTS.set(0);
    rig.overflow();
    rig.output(b"more");
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 0, "falling behind builds nothing by itself");
    assert!(rig.with_viewer(|v| v.repaint_pending));
    viewer.with(|out| out.credit(channel, wire::WINDOW));
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 1, "painted once it can receive it");
    assert!(!rig.with_viewer(|v| v.repaint_pending));
}

#[test]
fn a_paint_on_its_way_finishes_before_another_starts() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    viewer.with(|out| out.paint(channel, &vec![b'p'; wire::WINDOW / 2]));
    PAINTS.set(0);
    rig.overflow();
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 0, "it has credit, but a paint is still going out");
    assert_eq!(read(&viewer, channel).len(), wire::WINDOW / 2, "and that one arrives whole");
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 1);
}

#[test]
fn while_output_flows_a_repaint_goes_without_images_and_they_follow_when_it_rests() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    rig.overflow();
    rig.flowing();
    PAINTS.set(0);
    rig.core.prepare_repaints(&viewer);
    let text = read(&viewer, channel);
    assert_eq!(text[0], 0x18, "a repaint");
    assert!(!carries_images(&text));
    assert_eq!(rig.owed(), Some(true));
    assert_eq!(PAINTS.get(), 1);
    rig.far_from_the_ceiling();
    let wait = rig.core.prepare_repaints(&viewer).expect("the writer has to look again");
    assert!(wait <= IMAGES_REST);
    assert_eq!(PAINTS.get(), 1, "not while it flows");
    rig.rested();
    assert_eq!(rig.core.prepare_repaints(&viewer), None, "nothing more to wait for");
    assert!(carries_images(&read(&viewer, channel)));
    assert_eq!(rig.owed(), None);
    assert_eq!(rig.core.prepare_repaints(&viewer), None);
    assert_eq!(PAINTS.get(), 2, "and once");

    rig.overflow();
    rig.rested();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)), "a repaint after output rested needs no second one");
    assert_eq!(rig.owed(), None);
}

#[test]
fn output_that_never_rests_holds_images_back_only_so_long() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    rig.overflow();
    rig.flowing();
    rig.core.prepare_repaints(&viewer);
    read(&viewer, channel);
    let since = rig.far_from_the_ceiling();

    rig.overflow();
    rig.flowing();
    rig.core.prepare_repaints(&viewer);
    assert!(!carries_images(&read(&viewer, channel)));
    assert_eq!(rig.with_viewer(|v| v.owed.unwrap().since), since, "more text paints don't push it back");

    rig.past_the_ceiling();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)), "a spinner: its own output, nothing owed before");
    assert_eq!(rig.owed(), None);

    rig.overflow();
    rig.flowing();
    rig.core.prepare_repaints(&viewer);
    assert!(!carries_images(&read(&viewer, channel)), "the clock starts again");
    rig.past_the_ceiling();
    rig.overflow();
    rig.flowing();
    rig.core.prepare_repaints(&viewer);
    assert!(carries_images(&read(&viewer, channel)), "a flood too, however it falls behind");
}

#[test]
fn a_read_repaints_a_free_viewer_at_once_and_waits_for_one_receiving_a_paint() {
    let rig = Rig::new();
    rig.output(IMAGE);
    PAINTS.set(0);
    rig.after_read();
    assert_eq!(PAINTS.get(), 0, "with nobody to see it, nothing is built");

    let (viewer, channel) = rig.viewer();
    PAINTS.set(0);
    rig.flowing();
    rig.needs_a_full_paint();
    rig.after_read();
    assert_eq!(PAINTS.get(), 1);
    assert!(carries_images(&read(&viewer, channel)), "a read isn't a flood");
    assert!(!rig.with_viewer(|v| v.repaint_pending), "this was the repaint");

    viewer.with(|out| out.paint(channel, &vec![b'p'; wire::WINDOW / 2]));
    PAINTS.set(0);
    rig.after_read();
    assert_eq!(PAINTS.get(), 0, "reads can't keep restarting a paint");
    assert!(rig.with_viewer(|v| v.repaint_pending));
    assert_eq!(read(&viewer, channel).len(), wire::WINDOW / 2);
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 1);
}

#[test]
fn a_read_paints_those_it_can_and_leaves_the_rest_their_paint() {
    let (rig, free, free_channel) = Rig::late(&[IMAGE]);
    let (busy, busy_channel) = rig.viewer();
    busy.with(|out| out.paint(busy_channel, &vec![b'p'; wire::WINDOW / 2]));
    rig.after_read();
    assert!(carries_images(&read(&free, free_channel)));
    assert_eq!(read(&busy, busy_channel), vec![b'p'; wire::WINDOW / 2]);
    let pending: Vec<bool> = rig.core.lock().viewers[&rig.id].iter().map(|v| v.repaint_pending).collect();
    assert_eq!(pending, [false, true]);
}

#[test]
fn a_writer_paints_only_its_own_viewers() {
    let rig = Rig::new();
    let (mine, _) = rig.viewer();
    let (_other, _) = rig.viewer();
    rig.core.lock().viewers.get_mut(&rig.id).unwrap()[1].repaint_pending = true;
    PAINTS.set(0);
    assert_eq!(rig.core.prepare_repaints(&mine), None);
    assert_eq!(PAINTS.get(), 0);
    assert!(rig.core.lock().viewers[&rig.id][1].repaint_pending);
}

#[test]
fn a_repaint_says_whether_what_follows_it_needs_recovering() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.with_viewer(|v| {
        v.recovering = true;
        v.repaint_pending = true;
    });
    rig.core.prepare_repaints(&viewer);
    assert!(!rig.with_viewer(|v| v.recovering), "at ground, the stream can follow it as it is");
    read(&viewer, channel);

    rig.with_viewer(|v| v.recovering = true);
    rig.after_read();
    assert!(!rig.with_viewer(|v| v.recovering));
    read(&viewer, channel);

    rig.open_long_sequence();
    rig.with_viewer(|v| v.repaint_pending = true);
    rig.core.prepare_repaints(&viewer);
    assert!(rig.with_viewer(|v| v.recovering), "mid-way through something too long to replay");
    read(&viewer, channel);
    rig.after_read();
    assert!(rig.with_viewer(|v| v.recovering));
}

#[test]
fn a_paint_built_mid_sequence_ends_with_what_lets_the_rest_of_it_land() {
    let rig = Rig::new();
    rig.output(b"\x1b_Gm=1;half");
    let paint = first_paint(&rig.core.lock(), rig.id, 0);
    assert!(paint.ends_with(b"\x1b_Gm=1;half"));
    rig.output(b"\x1b\\");
    let paint = first_paint(&rig.core.lock(), rig.id, 0);
    assert!(!paint.ends_with(b"half"), "that sequence has ended");
}

#[test]
fn nothing_is_painted_while_a_read_has_the_app_scrolled() {
    let rig = Rig::new();
    let (viewer, _) = rig.viewer();
    rig.overflow();
    rig.core.lock().stitching.insert(rig.id);
    PAINTS.set(0);
    assert_eq!(rig.core.prepare_repaints(&viewer), None);
    assert_eq!(PAINTS.get(), 0);
    rig.core.lock().stitching.remove(&rig.id);
    rig.core.prepare_repaints(&viewer);
    assert_eq!(PAINTS.get(), 1);
}

#[test]
fn a_viewer_waiting_for_a_repaint_is_not_recovered_as_well() {
    let rig = Rig::new();
    let (_viewer, _) = rig.viewer();
    rig.open_long_sequence();
    rig.with_viewer(|v| {
        v.recovering = true;
        v.repaint_pending = true;
    });
    PAINTS.set(0);
    rig.output(b"AA\x1b\\tail");
    assert_eq!(PAINTS.get(), 0, "its repaint will do");
    rig.with_viewer(|v| v.repaint_pending = false);
    rig.open_long_sequence();
    rig.with_viewer(|v| v.recovering = true);
    PAINTS.set(0);
    rig.output(b"AA\x1b\\tail");
    assert_eq!(PAINTS.get(), 1, "without one, this is how it gets the screen back");
}

#[test]
fn a_recovered_viewer_gets_the_images_later_like_any_repaint_while_output_flows() {
    let (rig, viewer, channel) = Rig::late(&[IMAGE]);
    rig.open_long_sequence();
    rig.with_viewer(|v| v.recovering = true);
    read(&viewer, channel);
    rig.output(b"AA\x1b\\");
    let paint = read(&viewer, channel);
    assert_eq!(paint.iter().filter(|b| **b == 0x18).count(), 1, "a repaint");
    assert!(!carries_images(&paint));
    assert_eq!(rig.owed(), Some(true));
}

/// Output resting takes 100 ms for both; only one of them is 50 ms from the ceiling.
#[track_caller]
fn looks_again_for_the_first_due(rig: &Rig, conn: &Arc<Conn>, owed: [(u32, usize); 2]) {
    let nearly = Instant::now() - IMAGES_AT_MOST + Duration::from_millis(50);
    let far = Instant::now() + Duration::from_secs(3600);
    for (first, second) in [(nearly, far), (far, nearly)] {
        for ((id, n), since) in owed.into_iter().zip([first, second]) {
            let mut st = rig.core.lock();
            st.last_output.insert(id, far);
            st.viewers.get_mut(&id).unwrap()[n].owed = Some(Owed { since, carried: None, spare: false });
        }
        let wait = rig.core.prepare_repaints(conn).unwrap_or_default();
        assert!(wait <= Duration::from_millis(50), "{wait:?}");
    }
}

#[test]
fn a_writer_looks_again_for_whichever_of_its_viewers_is_due_first() {
    let rig = Rig::new();
    let conn = Arc::new(Conn::default());
    rig.viewers_on(&conn, &[rig.id, rig.id]);
    looks_again_for_the_first_due(&rig, &conn, [(rig.id, 0), (rig.id, 1)]);
}

#[test]
fn a_writer_looks_again_for_whichever_of_its_terminals_is_due_first() {
    let rig = Rig::new();
    let other = {
        let mut st = rig.core.lock();
        let new = crate::terminals::NewTerminal { argv: vec!["sh".into()], cols: 80, rows: 24, ..Default::default() };
        let id = st.reg.create(new, 0).unwrap();
        st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
        id
    };
    let conn = Arc::new(Conn::default());
    rig.viewers_on(&conn, &[rig.id, other]);
    looks_again_for_the_first_due(&rig, &conn, [(rig.id, 0), (other, 0)]);
}

#[test]
fn a_recovery_paint_is_a_paint_and_waits_for_one_already_going_out() {
    let rig = Rig::new();
    let (viewer, channel) = rig.viewer();
    rig.open_long_sequence();
    read(&viewer, channel);
    rig.with_viewer(|v| v.recovering = true);
    rig.output(b"AA\x1b\\");
    assert!(!viewer.outbox().can_paint(channel), "it went out as a paint, not as backlog");
    read(&viewer, channel);

    rig.open_long_sequence();
    read(&viewer, channel);
    rig.with_viewer(|v| v.recovering = true);
    viewer.with(|out| out.paint(channel, b"going out"));
    rig.output(b"AA\x1b\\");
    assert!(rig.with_viewer(|v| v.repaint_pending), "that one finishes first");
    assert!(read(&viewer, channel).starts_with(b"going out"));
}

#[test]
fn a_first_paint_of_any_size_arrives_whole() {
    let rig = Rig::new();
    {
        let mut st = rig.core.lock();
        st.screens.insert(rig.id, ghostty_vt::Terminal::new(80, 24, 128 << 20));
    }
    let line = format!("{}\r\n", "x".repeat(78));
    rig.output(line.repeat(wire::MAX_PENDING / line.len() + 1000).as_bytes());
    let (conn, channel) = rig.attach(AttachRequest { history: 200_000, ..request(false, false) });
    assert!(read(&conn, channel).len() > wire::MAX_PENDING);
    assert!(!rig.core.lock().viewers[&rig.id][0].repaint_pending);
}
