//! Viewers: raw attach streams on framed connections. First paint, live
//! bytes (minus answered queries), input, and who sets the size.

use crate::holder_proto as proto;
use crate::relay::{LoadsNow, Named, image_answer_wait};
use crate::rpc::RpcError;
use crate::runtime::{Core, State};
use crate::wire::Conn;
use ghostty_vt::{Carried, Given, Rgb};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
pub(crate) mod tests;

/// What a viewer's own terminal looks like, reported by the client.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
pub struct Display {
    /// Default foreground as `#rrggbb`.
    pub fg: Option<String>,
    /// Default background as `#rrggbb`.
    pub bg: Option<String>,
    /// Cursor color as `#rrggbb`.
    pub cursor: Option<String>,
    /// Cell size in pixels: `[width, height]`.
    pub cell: Option<[u32; 2]>,
}

pub struct Viewer {
    pub conn: Arc<Conn>,
    pub channel: u32,
    pub observe: bool,
    resize_notifications: bool,
    repaint_pending: bool,
    size: (u16, u16),
    /// None: not active since it came back after a dropped line.
    active_at: Option<Instant>,
    display: Display,
    /// An unfinished sequence exceeded continuation tracking; skip its tail.
    pub(crate) recovering: bool,
    answers: crate::relay::Answers,
    /// Images its last paint couldn't carry.
    owed: Option<Owed>,
    /// It has what the screen has: no image left out of a paint, and no byte of an image
    /// command or switch of screen lost since. Its repaints can leave its images be.
    whole: bool,
    /// Where the screen's images were when output last reached it. If they've moved
    /// since, they aren't where it has them.
    places: Option<u64>,
    /// A drag repaints at every step: images wait for it to end.
    resized_at: Option<Instant>,
    /// What its last paints left it of each screen, main and alternate. None: nothing
    /// of that one, and it hasn't been there since.
    given: [Option<Given>; 2],
    /// The main screen's text came from a paint, which breaks wrapped lines for good and
    /// leaves history out, so its own reflow won't come out as the screen's.
    reflows_apart: bool,
    /// Its last paint was laid out for a size other than its own. Anything else it shows is
    /// its own picture of the stream, as it would be anyway, and right by itself.
    painted_elsewhere: bool,
    /// Its last paint came during a load, which it won't have all of when that ends.
    load_cut: bool,
}

#[derive(Clone, Copy)]
struct Owed {
    since: Instant,
    /// It got none of them, not just missed one that was still arriving.
    all: bool,
    /// How many loads had ended by its last paint.
    finished: u64,
    /// The images that paint carried, if any.
    carried: Option<u64>,
    /// Only a resize is why: not worth what a paint can't bring back.
    layout: bool,
}

/// What a viewer is sent, and what it has afterwards.
#[derive(Clone, Copy, PartialEq)]
enum Paint {
    Images,
    Text,
    /// The text, for one that has every image where the screen has it.
    Kept,
    /// Of a screen with no images to bring.
    Empty,
}

impl Viewer {
    fn quiet(&self, output_quiet: Duration, now: Instant) -> Duration {
        self.resized_at.map_or(output_quiet, |at| output_quiet.min(now.saturating_duration_since(at)))
    }

    /// Whether a repaint it needs now can carry images. `hold`: how long its line still needs.
    fn repaint_has_images(&self, quiet: Duration, hold: Duration, now: Instant) -> bool {
        hold.is_zero() && (quiet >= IMAGES_REST || self.owed.is_some_and(|o| now >= o.since + IMAGES_AT_MOST))
    }

    /// How long until it should get the images it's owed. `None`: it isn't owed
    /// any, or only more output can change that.
    fn images_due_in(&self, quiet: Duration, hold: Duration, loads: LoadsNow, now: Instant) -> Option<Duration> {
        let owed = self.owed?;
        let at_most = (owed.since + IMAGES_AT_MOST).saturating_duration_since(now);
        let due = if !loads.open || loads.finished != owed.finished {
            at_most.min(IMAGES_REST.saturating_sub(quiet))
        } else if owed.all {
            at_most
        } else {
            // Short only of the one still arriving, and it may never finish.
            return None;
        };
        Some(due.max(hold))
    }

    /// The raw stream can't complete a load this paint cut in two, so that's owed too.
    fn painted(&mut self, now: Instant, paint: Paint, carried: Carried, loads: LoadsNow) {
        let (left_out, refused, carried) = match paint {
            Paint::Images => (carried.deferred, carried.refused, Some(carried.images)),
            Paint::Text => (true, false, None),
            Paint::Kept | Paint::Empty => (false, false, None),
        };
        self.owed = if left_out {
            // Text alone doesn't count as getting anywhere, or output could put images off for good.
            let since = if paint == Paint::Images { now } else { self.owed.map_or(now, |o| o.since) };
            Some(Owed { since, all: true, finished: loads.finished, carried, layout: false })
        } else if paint == Paint::Kept {
            // That one left any load be.
            None
        } else {
            loads.open.then_some(Owed { since: now, all: false, finished: loads.finished, carried, layout: false })
        };
        self.places = None;
        self.load_cut = loads.open && paint != Paint::Kept;
        // A paint without images can't make up for what it has lost.
        self.whole = self.owed.is_none() && !refused && (paint == Paint::Images || self.whole);
    }

    /// A paint went out. `given`: what it left of the screen it's on, if it carried images.
    fn gave(&mut self, paint: Paint, alt: bool, given: &Given, apart: bool, size: (u16, u16)) {
        // The alternate screen doesn't reflow, on either side.
        if !alt {
            self.reflows_apart = apart;
        }
        match paint {
            Paint::Kept => {
                // Rows laid out for another size can scroll what it has out from under it.
                if size != self.size {
                    self.painted_elsewhere = true;
                    self.whole = false;
                }
                return;
            }
            Paint::Images => self.given[alt as usize] = Some(given.clone()),
            Paint::Text | Paint::Empty => self.given[alt as usize] = Some(given.without_images()),
        }
        // Painting the alternate screen goes by way of clearing the main one.
        if alt {
            self.given[0] = None;
        }
        self.painted_elsewhere = size != self.size;
    }

    fn wake_if_owed(&self) {
        if self.owed.is_some() {
            self.conn.wake.notify_one();
        }
    }

    /// What it shows can't be where the screen has it, at this size: a paint has to say.
    fn owe_for_layout(&mut self, now: Instant, loads: LoadsNow) {
        let owed = Owed { since: now, all: true, finished: loads.finished, carried: None, layout: true };
        self.owed = Some(self.owed.map_or(owed, |o| Owed { all: true, carried: None, ..o }));
        self.whole = false;
        self.resized_at = Some(now);
    }

    /// Output just named these images (by generation), or brought a screen with them. A
    /// command about one it was never given does nothing there, and nor does going back to
    /// a screen it never had, so a paint has to. `upto`: what a screen new to it holds.
    fn check_given(&mut self, named: &Named, alt: bool, upto: u64, now: Instant, loads: LoadsNow) {
        // Back on a main screen it has none of, there's text to bring whatever else there is.
        let main_lost = !alt && self.given[0].is_none();
        let given = self.given[alt as usize].get_or_insert_with(|| Given::nothing(upto));
        if main_lost || given.lacks(&named.images) {
            let carried = if main_lost { None } else { given.carried() };
            let owed = Owed { since: now, all: true, finished: loads.finished, carried, layout: false };
            // What a paint is owed for regardless stays so.
            self.owed = Some(self.owed.map_or(owed, |o| Owed {
                all: true,
                carried: o.carried.and(carried),
                layout: false,
                ..o
            }));
            self.whole = false;
        }
    }

    /// Whether a repaint can leave its images be. `places`: where they are now, noted only
    /// where `places_checked` looked at what's queued. `mid_command`: a paint starts with CAN,
    /// on which Ghostty runs the command it's partway through, and the end of a load would be
    /// cut short with it.
    fn keeps_images(&self, mid_command: bool, places: impl Fn() -> u64) -> bool {
        !mid_command && self.whole && self.places.is_some_and(|had| had == places())
    }
}

impl Display {
    /// Answer the program's color and cell-size queries with these facts.
    pub(crate) fn apply(&self, screen: &mut ghostty_vt::Terminal) {
        screen.set_display(rgb(&self.fg), rgb(&self.bg), rgb(&self.cursor), self.cell.map(|[w, h]| (w, h)));
    }
}

fn rgb(hex: &Option<String>) -> Option<Rgb> {
    let hex = hex.as_deref()?.strip_prefix('#')?;
    let n = u32::from_str_radix(hex, 16).ok().filter(|_| hex.len() == 6)?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

/// Output must rest this long before images owed to a viewer go, so a flood or a
/// stream of images costs one paint with images, not one for each step.
pub(crate) const IMAGES_REST: Duration = Duration::from_millis(100);
/// Output that never rests (a spinner) holds them back no longer than this.
const IMAGES_AT_MOST: Duration = Duration::from_secs(2);

pub struct AttachRequest {
    pub observe: bool,
    pub resize_notifications: bool,
    pub history: usize,
    pub size: (u16, u16),
    pub display: Display,
    pub resumed: bool,
}

impl Core {
    /// Adds a viewer: queues the first paint, then every live byte follows in order.
    pub fn attach(&self, conn: &Arc<Conn>, id: u32, req: AttachRequest) -> Result<u32, RpcError> {
        let mut st = self.lock();
        if let Some(t) = st.reg.terminals.get(&id) {
            t.check_program()?;
        }
        if !st.screens.contains_key(&id) {
            return Err(RpcError::new(
                "finished",
                format!("{} has finished", crate::terminals::address(id)),
                format!("read what it left: hc read {}", crate::terminals::address(id)),
            ));
        }
        let channel = conn.new_channel();
        let recovering = st.screens[&id].continuation().is_none();
        st.viewers.entry(id).or_default().push(Viewer {
            conn: conn.clone(),
            channel,
            observe: req.observe,
            resize_notifications: req.observe && req.resize_notifications,
            repaint_pending: false,
            size: req.size,
            active_at: (!req.resumed).then(Instant::now),
            display: req.display,
            recovering,
            answers: Default::default(),
            owed: None,
            whole: false,
            places: None,
            resized_at: None,
            given: [None, None],
            reflows_apart: false,
            painted_elsewhere: true,
            load_cut: false,
        });
        if !req.observe {
            self.apply_size(&mut st, id);
        }
        let paint = first_paint(&st, id, req.history);
        let (loads, carried) = (loads_now(&st, id), st.screens[&id].images_carried());
        let (given, alt) = (st.screens[&id].images_given(), st.screens[&id].alt_screen());
        let (apart, size) = (reflows_apart(&st.screens[&id]), set_size(&st, id));
        let viewer = st.viewers.get_mut(&id).unwrap().last_mut().unwrap();
        viewer.repaint_pending = false;
        viewer.painted(Instant::now(), Paint::Images, carried, loads);
        viewer.gave(Paint::Images, alt, &given, apart, size);
        conn.with(|out| {
            out.open_paused(channel);
            if req.observe && req.resize_notifications {
                let t = &st.reg.terminals[&id];
                let size = [t.cols.to_le_bytes(), t.rows.to_le_bytes()].concat();
                out.resize(channel, &size);
            }
            out.paint_with_images(channel, &paint);
        });
        Ok(channel)
    }

    /// A single runtime task owns image deadlines, including output from holder threads.
    pub async fn expire_image_answers(self: Arc<Self>) {
        loop {
            let next = {
                let mut st = self.lock();
                let now = tokio::time::Instant::now();
                let due: Vec<_> = st
                    .image_deadlines
                    .iter()
                    .filter(|(_, (_, at))| *at <= now)
                    .map(|(&id, &(wait, _))| (id, wait))
                    .collect();
                for (id, wait) in due {
                    st.image_deadlines.remove(&id);
                    let late = st.relays.get_mut(&id).map(|r| r.expire(wait)).unwrap_or_default();
                    let Some(gate) = st.gates.get_mut(&id) else { continue };
                    let late = gate.viewer(&late, false);
                    if let Some(link) = st.link(id) {
                        link.input(id, &late);
                    }
                }
                st.image_deadlines.values().map(|&(_, at)| at).min()
            };
            match next {
                Some(at) => tokio::select! {
                    _ = tokio::time::sleep_until(at) => {}
                    _ = self.image_due.notified() => {}
                },
                None => self.image_due.notified().await,
            }
        }
    }

    /// Program output: through the relay to the screen and every viewer,
    /// then whatever status or title it reported.
    pub fn output(self: &Arc<Self>, st: &mut State, id: u32, bytes: &[u8]) {
        let now = Instant::now();
        st.last_output.insert(id, now);
        *st.output_seq.entry(id).or_default() += 1;
        if let Some(t) = st.reg.terminals.get_mut(&id) {
            t.printed += bytes.iter().filter(|b| **b == b'\n').count() as u64;
        }
        // While a read scrolls the app, its frames are ours alone: viewers get one repaint after.
        let scrolling = st.stitching.contains(&id);
        let instance = st.reg.terminals.get(&id).and_then(|t| t.holder.clone());
        let State { screens, relays, viewers, holders, output_watch, gates, .. } = st;
        let holder = holders.iter().find(|l| Some(&l.instance) == instance.as_ref());
        let Some(screen) = screens.get_mut(&id) else { return };
        let relay = relays.entry(id).or_default();
        // One waiting for a repaint gets it from the paint it's waiting for.
        let recovering = viewers.get(&id).into_iter().flatten().any(|v| v.recovering && !v.repaint_pending);
        let (seen, alt, images) = (relay.loads.seen(), screen.alt_screen(), screen.images_generation());
        let (out, recovered) =
            if recovering { relay.feed_to_ground(screen, bytes) } else { (relay.feed(screen, bytes), None) };
        let (alt_changed, images_before) = (screen.alt_screen() != alt, images);
        let images = screen.images_generation();
        let cant_be_missed = relay.loads.seen() != seen || alt_changed || images != images_before;
        let mut named = relay.loads.take_named();
        // Every image at every read of output would be too much, so only for this.
        if alt_changed && viewers.get(&id).is_some_and(|viewers| !viewers.is_empty()) {
            named.images.extend(screen.placed_images());
        }
        let upto = named.switched.unwrap_or(images);
        let loads = relay.loads.now();
        let win32_input = relay.win32_input.take();
        if let Some(watchers) = output_watch.get_mut(&id).filter(|_| !scrolling) {
            watchers.retain(|w| w.send(out.clone()).is_ok());
        }
        relay.expect_answers(viewers.get_mut(&id).into_iter().flatten().filter(|v| !v.observe).map(|v| &mut v.answers));
        let (replies, wait) = relay.take_replies(screen, answering(viewers.get(&id)));
        let replies = ordered_replies(gates.entry(id).or_default(), &replies);
        if let Some(link) = holder {
            link.input(id, &replies);
        }
        if let Some(wait) = wait {
            st.image_deadlines.insert(id, (wait, tokio::time::Instant::now() + image_answer_wait()));
            self.image_due.notify_one();
        }
        let recovered_paint = if recovered.is_some() && screen.has_images() { Paint::Text } else { Paint::Empty };
        let apart = recovered.is_some() && reflows_apart(screen);
        let reports = screen.take_status_reports();
        let title = screen.take_title_changed().then(|| screen.title());
        let full_screen = screen.alt_screen();
        // Before any repaint below, so it carries the mode this output just set.
        if let Some(on) = win32_input
            && let Some(t) = st.reg.terminals.get_mut(&id).filter(|t| t.win32_input != on)
        {
            t.win32_input = on;
            self.save(st);
        }
        let recovered = recovered.map(|(at, paint)| {
            let mut paint = paint_state(st, id, paint);
            paint.extend_from_slice(&out[at..]);
            paint
        });
        let size = set_size(st, id);
        for v in st.viewers.get_mut(&id).into_iter().flatten() {
            // None of this reaches it, whichever screen it was for.
            if cant_be_missed && (scrolling || v.repaint_pending || v.recovering) {
                v.whole = false;
                v.given[!full_screen as usize] = None;
            }
            if scrolling || v.repaint_pending {
                continue;
            }
            if v.recovering {
                let Some(paint) = &recovered else { continue };
                v.recovering = false;
                // One already on its way finishes, and this one's rest of the sequence is gone.
                if !v.conn.outbox().can_paint(v.channel) {
                    v.repaint_pending = true;
                    continue;
                }
                v.painted(now, recovered_paint, Carried::default(), loads);
                v.gave(recovered_paint, full_screen, &Given::nothing(images), apart, size);
                v.conn.with(|ob| ob.paint(v.channel, paint));
                continue;
            }
            // Its backlog went: a repaint follows, built once it has credit for one.
            v.conn.with(|ob| {
                v.repaint_pending = !ob.data(v.channel, &out);
                v.whole &= !v.repaint_pending;
                if cant_be_missed {
                    ob.queued_images(v.channel);
                }
            });
            if v.repaint_pending {
                v.given[!full_screen as usize] = None;
            }
            if !named.images.is_empty() || alt_changed {
                v.check_given(&named, full_screen, upto, now, loads);
            }
        }
        if full_screen && let Some(t) = st.reg.terminals.get_mut(&id) {
            t.full_screen = true;
        }
        if !reports.is_empty() {
            self.report_status(st, id, &reports);
        }
        if let Some(title) = title {
            st.reg.terminals.get_mut(&id).expect("live terminal").title = title.clone();
            self.save_soon();
            self.emit(st, "title", id, serde_json::json!({"title": title}));
        }
    }

    /// Applies 7501 reports and tells subscribers what changed.
    pub fn report_status(&self, st: &mut State, id: u32, reports: &[ghostty_vt::StatusReport]) {
        let word = |st: &State| st.reg.terminals[&id].status.as_ref().map(|s| s.state.clone());
        let mut word_changed = false;
        for r in reports {
            let was = word(st);
            let t = st.reg.terminals.get_mut(&id).expect("live terminal");
            let change = crate::status::apply(t, r);
            self.status_changed(st, id, change);
            word_changed |= word(st) != was;
        }
        // Losing the holder too (a reboot) leaves no tail to replay: queue a new word,
        // and with it a turn or news, for immediate persistence. Messages and progress can wait.
        if word_changed { self.save(st) } else { self.save_soon() }
    }

    pub fn status_changed(&self, st: &mut State, id: u32, change: crate::status::Change) {
        let t = st.reg.terminals.get_mut(&id).expect("reported terminal");
        let first_task_finished = t.first_task_turn.is_some_and(|turn| {
            t.turn > turn
                || (change.root_report
                    && t.status.as_ref().is_some_and(|s| matches!(s.state.as_str(), "done" | "blocked" | "error")))
        });
        if first_task_finished {
            t.first_task_turn = None;
        }
        let (status, records, turn, reported) = (t.status.clone(), t.records.clone(), t.turn, t.reported);
        if change.status {
            self.emit(
                st,
                "status",
                id,
                serde_json::json!({"status": status, "records": records, "reported": reported}),
            );
        }
        if change.turn_ended {
            let state = status.map(|s| s.state);
            self.emit(st, "turn", id, serde_json::json!({"turn": turn, "state": state}));
        }
        if (change.turn_ended || change.stuck || first_task_finished) && st.reg.terminals[&id].is_live() {
            self.news(st, id);
        }
    }

    fn viewer<'a>(st: &'a mut State, conn: &Arc<Conn>, channel: u32) -> Option<(u32, &'a mut Viewer)> {
        st.viewers.iter_mut().find_map(|(id, vs)| {
            vs.iter_mut().find(|v| Arc::ptr_eq(&v.conn, conn) && v.channel == channel).map(|v| (*id, v))
        })
    }

    /// Typing, resizing or focusing makes a viewer the one whose size and colors count.
    fn touch(&self, st: &mut State, conn: &Arc<Conn>, channel: u32, f: impl FnOnce(&mut Viewer)) -> Option<u32> {
        let (id, v) = Self::viewer(st, conn, channel)?;
        let own = v.size;
        f(v);
        if v.observe {
            return None;
        }
        v.active_at = Some(Instant::now());
        if v.size == own {
            self.apply_size(st, id);
            let (_, v) = Self::viewer(st, conn, channel)?;
            // What it's owed may have been waiting for this.
            v.wake_if_owed();
            return Some(id);
        }
        let paint_on_its_way = v.conn.outbox().painting(v.channel);
        let direct = direct_images_in_view(st, id);
        self.apply_size(st, id);
        let direct = direct || direct_images_in_view(st, id);
        let alt = st.screens.get(&id).is_some_and(|screen| screen.alt_screen());
        let loads = loads_now(st, id);
        let (_, v) = Self::viewer(st, conn, channel)?;
        // The main screen reflows behind the alternate one, and it can't see how.
        if alt && v.reflows_apart {
            v.given[0] = None;
        }
        // A viewer's image is where its own reflow left it, which may not be where the
        // screen's did.
        if direct && (paint_on_its_way || v.painted_elsewhere || (!alt && v.reflows_apart)) {
            v.owe_for_layout(Instant::now(), loads);
        }
        v.wake_if_owed();
        Some(id)
    }

    pub fn viewer_input(&self, conn: &Arc<Conn>, channel: u32, bytes: &[u8], automatic: bool) {
        let mut st = self.lock();
        let id = if automatic {
            Self::viewer(&mut st, conn, channel).filter(|(_, v)| !v.observe).map(|(id, _)| id)
        } else {
            self.touch(&mut st, conn, channel, |_| {})
        };
        if let Some(id) = id {
            let answered = Self::viewer(&mut st, conn, channel).unwrap().1.answers.feed(bytes);
            let mut now = Vec::new();
            let mut from = 0;
            // Insert at the complete answer, before any trailing unfinished APC.
            // Queue both through the gate so expiry cannot overtake them.
            for (end, query) in answered {
                now.extend(st.gates.entry(id).or_default().viewer(&bytes[from..end], !automatic));
                let waited = st.relays.get_mut(&id).map(|r| r.answered(&[(end, query)])).unwrap_or_default();
                now.extend(st.gates.entry(id).or_default().viewer(&waited, false));
                from = end;
            }
            now.extend(st.gates.entry(id).or_default().viewer(&bytes[from..], !automatic));
            if let Some(link) = st.link(id) {
                link.input(id, &now);
            }
        }
    }

    pub fn viewer_resize(&self, conn: &Arc<Conn>, channel: u32, cols: u16, rows: u16) {
        let mut st = self.lock();
        self.touch(&mut st, conn, channel, |v| v.size = (cols, rows));
    }

    pub fn viewer_facts(&self, conn: &Arc<Conn>, channel: u32, display: Display) {
        let mut st = self.lock();
        let Some((id, v)) = Self::viewer(&mut st, conn, channel) else { return };
        v.display.fg = display.fg.or(v.display.fg.take());
        v.display.bg = display.bg.or(v.display.bg.take());
        v.display.cursor = display.cursor.or(v.display.cursor.take());
        v.display.cell = display.cell.or(v.display.cell);
        let display = st.viewers[&id]
            .iter()
            .filter(|v| !v.observe)
            .max_by_key(|v| v.active_at)
            .filter(|v| Arc::ptr_eq(&v.conn, conn) && v.channel == channel)
            .map(|v| v.display.clone());
        if let Some(display) = display {
            self.apply_display(&mut st, id, Some(&display));
        }
    }

    /// Focus changes count as activity, and reach programs that asked for them.
    pub fn viewer_focus(&self, conn: &Arc<Conn>, channel: u32, focused: bool) {
        let mut st = self.lock();
        let id = if focused {
            self.touch(&mut st, conn, channel, |_| {})
        } else {
            Self::viewer(&mut st, conn, channel).filter(|(_, v)| !v.observe).map(|(id, _)| id)
        };
        if let Some(id) = id
            && st.screens.get(&id).is_some_and(|s| s.dec_mode(1004))
            && let Some(link) = st.link(id)
        {
            link.input(id, if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    /// A viewer left (`channel` = None: the whole connection went away).
    pub fn detach(&self, conn: &Arc<Conn>, channel: Option<u32>) {
        let mut st = self.lock();
        self.unsubscribe(&mut st, conn, channel);
        let mut touched = Vec::new();
        for (id, vs) in st.viewers.iter_mut() {
            let mut typing_left = false;
            vs.retain(|v| {
                let leaves = Arc::ptr_eq(&v.conn, conn) && channel.is_none_or(|c| c == v.channel);
                typing_left |= leaves && !v.observe;
                !leaves
            });
            if typing_left {
                touched.push(*id);
            }
        }
        st.viewers.retain(|_, vs| !vs.is_empty());
        for id in touched {
            self.apply_size(&mut st, id);
        }
    }

    /// The most recently active typing viewer sets size and display facts;
    /// with none, the terminal runs headless.
    fn apply_size(&self, st: &mut State, id: u32) {
        let leader = st.viewers.get(&id).and_then(|vs| vs.iter().filter(|v| !v.observe).max_by_key(|v| v.active_at));
        let (cols, rows) = leader.map_or_else(|| self.headless(), |v| v.size);
        let display = leader.map(|v| v.display.clone());
        if !st.screens.contains_key(&id) {
            return;
        }
        self.apply_display(st, id, display.as_ref());
        let rec = st.reg.terminals.get_mut(&id).expect("live terminal");
        if (rec.cols, rec.rows) == (cols, rows) || cols == 0 || rows == 0 {
            return;
        }
        let notified = st.viewers.get(&id).into_iter().flatten().any(|v| v.resize_notifications);
        if notified {
            places_checked(st, id);
        }
        // Whichever side of this step: a viewer may only see the size of the last of many.
        let direct = st.screens[&id].direct_images_in_view();
        let rec = st.reg.terminals.get_mut(&id).expect("live terminal");
        st.screens.get_mut(&id).expect("live screen").resize(cols, rows);
        rec.cols = cols;
        rec.rows = rows;
        if let Some(link) = st.link(id) {
            link.send(&proto::json_frame(proto::RESIZE, id, &proto::Size { cols, rows }));
        }
        let size = [cols.to_le_bytes(), rows.to_le_bytes()].concat();
        // Placeholder images are in the cells it's sent. Direct ones it moves for itself, by
        // text whose soft wraps a paint made hard, so it may not put them where the screen did.
        let direct = direct || st.screens[&id].direct_images_in_view();
        let places = (notified && !direct).then(|| st.screens[&id].image_places());
        let alt = st.screens[&id].alt_screen();
        let loads = loads_now(st, id);
        let now = Instant::now();
        for v in st.viewers.get_mut(&id).into_iter().flatten() {
            if direct && !alt && v.reflows_apart {
                v.whole = false;
            }
            // One told of it reflows its main screen too, behind the alternate one, unseen.
            if alt && v.reflows_apart && v.resize_notifications {
                v.given[0] = None;
            }
            // Coming to its size is news only to one painted for another: what it has otherwise
            // is what it would have anyway, and the screen's trip there and back isn't exact.
            // An observer doesn't get a paint for one: told of it, it has a repaint coming.
            if direct && v.painted_elsewhere && v.size == (cols, rows) && !v.observe {
                v.owe_for_layout(now, loads);
            }
            v.wake_if_owed();
        }
        for v in st.viewers.get_mut(&id).into_iter().flatten().filter(|v| v.resize_notifications) {
            v.repaint_pending = true;
            v.resized_at = Some(Instant::now());
            v.places = places;
            v.conn.with(|out| out.resize(v.channel, &size));
        }
        self.save_soon();
        self.emit(st, "size", id, serde_json::json!({"cols": cols, "rows": rows}));
    }

    /// Serialize only the latest grid, when a viewer has credit to receive it.
    /// Returns how soon to look again if nothing else wakes the writer first.
    pub fn prepare_repaints(&self, conn: &Arc<Conn>) -> Option<Duration> {
        let mut st = self.lock();
        let ids: Vec<u32> = st.viewers.keys().copied().collect();
        ids.into_iter().filter_map(|id| self.queue_repaints(&mut st, id, Some(conn))).min()
    }

    /// On exit there will be no later writer pass over this screen.
    pub(crate) fn queue_repaints(&self, st: &mut State, id: u32, conn: Option<&Arc<Conn>>) -> Option<Duration> {
        // A read scrolling the app ends with a paint of its own, for everyone.
        if conn.is_some() && st.stitching.contains(&id) {
            return None;
        }
        let now = Instant::now();
        let output_quiet = st.last_output.get(&id).map_or(Duration::MAX, |t| now.saturating_duration_since(*t));
        let loads = loads_now(st, id);
        let mut again = None;
        let screen = &st.screens;
        let mid_command = st.relays.get(&id).is_some_and(|relay| relay.loads.in_command());
        let places = std::cell::OnceCell::new();
        let places_now = || *places.get_or_init(|| screen[&id].image_places());
        let carried = std::cell::OnceCell::<Carried>::new();
        let carried_now = || {
            *carried.get_or_init(|| {
                screen[&id].images_ready();
                screen[&id].images_carried()
            })
        };
        let size = set_size(st, id);
        let mut paints = Vec::new();
        let mut settled = Vec::new();
        for (index, v) in st.viewers.get(&id).into_iter().flatten().enumerate() {
            // What it's owed is for its own size: laid out for another, it would only be wrong.
            // One with no size to give takes what there is.
            let no_size = v.size.0 == 0 || v.size.1 == 0;
            let at_its_size = v.observe || no_size || v.size == size || conn.is_none();
            let due = v.repaint_pending || (v.owed.is_some() && at_its_size);
            if !due || conn.is_some_and(|conn| !Arc::ptr_eq(&v.conn, conn) || !conn.outbox().can_paint(v.channel)) {
                continue;
            }
            if v.keeps_images(mid_command, places_now) {
                paints.push((index, Paint::Kept));
                continue;
            }
            if conn.is_none() {
                paints.push((index, Paint::Images));
                continue;
            }
            let quiet = v.quiet(output_quiet, now);
            let hold = v.conn.outbox().images_wait(v.channel, now);
            match v.images_due_in(quiet, hold, loads, now) {
                _ if v.repaint_pending => {
                    let images = v.repaint_has_images(quiet, hold, now);
                    paints.push((index, if images { Paint::Images } else { Paint::Text }));
                }
                Some(wait) if wait.is_zero() => {
                    let since = v.owed.map_or(now, |o| o.since);
                    let past_the_ceiling = now >= since + IMAGES_AT_MOST;
                    let until_the_ceiling = (since + IMAGES_AT_MOST).saturating_duration_since(now);
                    // The encoder says when it's done.
                    if st.screens[&id].encoding() && !past_the_ceiling {
                        look_again(&mut again, until_the_ceiling);
                        continue;
                    }
                    let carried = carried_now();
                    if carried.deferred {
                        ask_encoder(st, id);
                    }
                    // A paint clears images, and of these it can't bring all, which the stream may have.
                    let keep = carried.refused && !carried.deferred && v.owed.is_some_and(|o| o.layout);
                    if keep || v.owed.and_then(|o| o.carried) == Some(carried.images) {
                        // It may already have all a paint could bring: all that encoding was to find out.
                        settled.push(index);
                        if carried.deferred {
                            look_again(&mut again, IMAGES_AT_MOST);
                        }
                    } else if !carried.deferred || past_the_ceiling {
                        paints.push((index, Paint::Images));
                    } else {
                        look_again(&mut again, until_the_ceiling);
                    }
                }
                Some(wait) => look_again(&mut again, wait),
                None => {}
            }
        }
        for index in settled {
            let screen = &st.screens[&id];
            let (carried, given, alt) = (screen.images_carried(), screen.images_given(), screen.alt_screen());
            let v = &mut st.viewers.get_mut(&id).expect("settled viewers")[index];
            let load_cut = v.load_cut;
            v.painted(now, Paint::Images, carried, loads);
            // Nothing reached it, so its record stands, but for a load it came in on: that
            // counts as new when it ends, and this viewer doesn't have all of it.
            if load_cut {
                v.given[alt as usize] = Some(given);
            }
        }
        if !paints.is_empty() {
            // With none on the screen there's nothing to hold back, but what it has to clear.
            if !st.screens[&id].has_images() {
                paints.iter_mut().filter(|p| p.1 == Paint::Text).for_each(|p| p.1 = Paint::Images);
            }
            let any = |paint| paints.iter().any(|p| p.1 == paint);
            // The last it will ever get has no later paint to leave anything to.
            let with_images = any(Paint::Images).then(|| match conn {
                Some(_) => repaint(st, id),
                None => with_state(st, id, st.screens[&id].repaint()),
            });
            let text = any(Paint::Text).then(|| with_state(st, id, st.screens[&id].paint_text(0)));
            let kept = any(Paint::Kept).then(|| with_state(st, id, st.screens[&id].paint_keeping_images()));
            let screen = &st.screens[&id];
            let recovering = screen.continuation().is_none();
            let carried = screen.images_carried();
            let (given, alt, apart) = (screen.images_given(), screen.alt_screen(), reflows_apart(screen));
            let size = set_size(st, id);
            let viewers = st.viewers.get_mut(&id).expect("viewers to paint");
            for (index, paint) in paints {
                let v = &mut viewers[index];
                let bytes = match paint {
                    Paint::Images => &with_images,
                    Paint::Text => &text,
                    Paint::Kept => &kept,
                    Paint::Empty => unreachable!("only a recovered viewer gets this"),
                };
                let bytes = bytes.as_ref().expect("built above");
                match paint {
                    Paint::Images => v.conn.with(|out| out.paint_with_images(v.channel, bytes)),
                    _ => v.conn.with(|out| out.paint(v.channel, bytes)),
                }
                v.repaint_pending = false;
                v.recovering = recovering;
                v.painted(now, paint, carried, loads);
                v.gave(paint, alt, &given, apart, size);
            }
        }
        again
    }

    /// A read scrolled the app unseen: every viewer needs the screen back. One still
    /// receiving a paint gets this after it, or reads could keep a paint from ever ending.
    pub(crate) fn repaint_after_read(&self, st: &mut State, id: u32) {
        let now = Instant::now();
        let loads = loads_now(st, id);
        let places = st.screens[&id].image_places();
        let mid_command = st.relays.get(&id).is_some_and(|relay| relay.loads.in_command());
        let kept = |v: &Viewer| v.keeps_images(mid_command, || places);
        let free = |v: &Viewer| {
            let kept = kept(v);
            let out = v.conn.outbox();
            out.can_paint(v.channel) && (kept || out.images_wait(v.channel, now).is_zero())
        };
        let any = |wanted: bool| st.viewers.get(&id).into_iter().flatten().any(|v| free(v) && kept(v) == wanted);
        let (images, leaving) = (any(false), any(true));
        let images = images.then(|| repaint(st, id));
        let leaving = leaving.then(|| with_state(st, id, st.screens[&id].paint_keeping_images()));
        let recovering = st.screens[&id].continuation().is_none();
        let carried = st.screens[&id].images_carried();
        let (given, alt) = (st.screens[&id].images_given(), st.screens[&id].alt_screen());
        let (apart, size) = (reflows_apart(&st.screens[&id]), set_size(st, id));
        for v in st.viewers.get_mut(&id).into_iter().flatten() {
            let (paint, bytes) = if kept(v) { (Paint::Kept, &leaving) } else { (Paint::Images, &images) };
            match bytes.as_ref().filter(|_| free(v)) {
                Some(bytes) => {
                    match paint {
                        Paint::Images => v.conn.with(|out| out.paint_with_images(v.channel, bytes)),
                        _ => v.conn.with(|out| out.paint(v.channel, bytes)),
                    }
                    v.repaint_pending = false;
                    v.recovering = recovering;
                    v.painted(now, paint, carried, loads);
                    v.gave(paint, alt, &given, apart, size);
                }
                None => {
                    v.repaint_pending = true;
                    v.conn.wake.notify_one();
                }
            }
        }
    }

    /// Encodes what paints are waiting for, away from the lock and from every other task:
    /// one image at a time and each terminal in turn, so a large one holds up nothing else.
    pub async fn run_encoder(self: Arc<Self>, mut asked: tokio::sync::mpsc::UnboundedReceiver<u32>) {
        let mut waiting = std::collections::VecDeque::new();
        loop {
            while let Ok(id) = asked.try_recv() {
                waiting.push_back(id);
            }
            let id = match waiting.pop_front() {
                Some(id) => id,
                None => match asked.recv().await {
                    Some(id) => id,
                    None => return,
                },
            };
            let Some(job) = self.next_encode(id) else {
                self.encoding_finished(id);
                continue;
            };
            if let Ok(done) = tokio::task::spawn_blocking(move || job.run()).await {
                self.keep_encoded(id, done);
            }
            waiting.push_back(id);
        }
    }

    pub(crate) fn next_encode(&self, id: u32) -> Option<ghostty_vt::EncodeJob> {
        let st = self.lock();
        // Nobody to show them to: the next to attach asks again.
        st.viewers.get(&id).is_some_and(|viewers| !viewers.is_empty()).then(|| st.screens.get(&id)?.next_encode())?
    }

    pub(crate) fn keep_encoded(&self, id: u32, done: ghostty_vt::EncodedImage) {
        if let Some(screen) = self.lock().screens.get(&id) {
            screen.encoded(done);
        }
    }

    /// Those owed images hear of it from their writers.
    pub(crate) fn encoding_finished(&self, id: u32) {
        let st = self.lock();
        if let Some(screen) = st.screens.get(&id) {
            screen.set_encoding(false);
        }
        st.viewers.get(&id).into_iter().flatten().for_each(|v| v.conn.wake.notify_one());
    }

    fn apply_display(&self, st: &mut State, id: u32, display: Option<&Display>) {
        let answering = answering(st.viewers.get(&id));
        let State { screens, relays, .. } = st;
        let Some(screen) = screens.get_mut(&id) else { return };
        if let Some(d) = display {
            d.apply(screen);
        }
        // Display changes can notify an idle program, even without a resize or output.
        // In order behind a viewer's image answer; only output begins a wait.
        let (replies, _) = relays.entry(id).or_default().take_replies(screen, answering);
        let replies = ordered_replies(st.gates.entry(id).or_default(), &replies);
        if let Some(link) = st.link(id) {
            link.input(id, &replies);
        }
    }
}

/// DA1 must follow already queued viewer answers, even after their relay wait ended.
/// Prompt queries and other screen replies remain immediate.
fn ordered_replies(gate: &mut crate::input::Gate, replies: &[u8]) -> Vec<u8> {
    let mut now = Vec::with_capacity(replies.len());
    let mut i = 0;
    while i < replies.len() {
        if let Some(n) = crate::relay::da1_len(&replies[i..]) {
            now.extend(gate.viewer(&replies[i..i + n], false));
            i += n;
        } else {
            now.push(replies[i]);
            i += 1;
        }
    }
    now
}

/// Whether a viewer could answer an image: observers' replies never reach the program.
fn answering(viewers: Option<&Vec<Viewer>>) -> bool {
    viewers.is_some_and(|vs| vs.iter().any(|v| !v.observe))
}

/// The size viewers set: a program's own 132 columns don't change what they are asking for.
fn set_size(st: &State, id: u32) -> (u16, u16) {
    st.reg.terminals.get(&id).map_or((0, 0), |t| (t.cols, t.rows))
}

fn direct_images_in_view(st: &State, id: u32) -> bool {
    st.screens.get(&id).is_some_and(|screen| screen.direct_images_in_view())
}

/// Whether a viewer given this screen by a paint could reflow away from it: a paint makes
/// its soft wraps hard and leaves history out, and there's only no text to be sure of that.
fn reflows_apart(screen: &ghostty_vt::Terminal) -> bool {
    screen.history_rows() > 0 || screen.screen_rows().iter().any(|row| !row.trim().is_empty())
}

fn loads_now(st: &State, id: u32) -> LoadsNow {
    st.relays.get(&id).map_or(LoadsNow::default(), |relay| relay.loads.now())
}

/// The screen, then the latest 7501 report of every record and whether the
/// program wants win32 key events (both pass through to viewers, so a new one
/// must see them too; a viewer repainted after falling behind gets the mode
/// either way).
pub(crate) fn first_paint(st: &State, id: u32, history: usize) -> Vec<u8> {
    asking_encoder(st, id, st.screens[&id].paint_ready(history))
}

/// `first_paint` for a viewer already showing this screen.
fn repaint(st: &State, id: u32) -> Vec<u8> {
    asking_encoder(st, id, st.screens[&id].repaint_ready())
}

fn asking_encoder(st: &State, id: u32, screen: Vec<u8>) -> Vec<u8> {
    let paint = with_state(st, id, screen);
    if st.screens[&id].images_carried().deferred {
        ask_encoder(st, id);
    }
    paint
}

fn look_again(again: &mut Option<Duration>, wait: Duration) {
    *again = Some(again.map_or(wait, |soonest| soonest.min(wait)));
}

/// A viewer about to miss output: if it's already missing some and the images have moved
/// since it started, or what's queued for it can be lost and may move them, it won't have
/// them where they are. Returns where they are.
pub(crate) fn places_checked(st: &mut State, id: u32) -> u64 {
    let places = st.screens[&id].image_places();
    for v in st.viewers.get_mut(&id).into_iter().flatten() {
        let out = v.conn.outbox();
        let lost = out.images_queued(v.channel) || (places != 0 && out.queued(v.channel) > 0);
        drop(out);
        v.whole &= !(lost || (v.repaint_pending && v.places != Some(places)));
    }
    places
}

/// Everyone starts missing output now, with the images where they are.
pub(crate) fn note_places(st: &mut State, id: u32) {
    let places = places_checked(st, id);
    st.viewers.get_mut(&id).into_iter().flatten().for_each(|v| v.places = Some(places));
}

fn ask_encoder(st: &State, id: u32) {
    let screen = &st.screens[&id];
    if !screen.encoding() {
        screen.set_encoding(true);
        if let Some(encoder) = &st.encoder {
            let _ = encoder.send(id);
        }
    }
}

fn with_state(st: &State, id: u32, screen: Vec<u8>) -> Vec<u8> {
    #[cfg(test)]
    tests::PAINTS.with(|n| n.set(n.get() + 1));
    let mut paint = paint_state(st, id, screen);
    if let Some(continuation) = st.screens[&id].continuation() {
        paint.extend(continuation);
    }
    paint
}

fn paint_state(st: &State, id: u32, screen: Vec<u8>) -> Vec<u8> {
    // A backlog may have ended in any parser state, not at an escape boundary.
    let mut paint = vec![0x18];
    paint.extend(screen);
    let t = &st.reg.terminals[&id];
    paint.extend(if t.win32_input { b"\x1b[?9001h" } else { b"\x1b[?9001l" });
    let root = t.status.iter().map(|s| ("", s));
    for (rid, status) in root.chain(t.records.iter().map(|(k, s)| (k.as_str(), s))) {
        paint.extend(status.encode(rid).as_bytes());
    }
    paint
}
