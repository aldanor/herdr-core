//! Viewers: raw attach streams on framed connections. First paint, live
//! bytes (minus answered queries), input, and who sets the size.

use crate::holder_proto as proto;
use crate::relay::image_answer_wait;
use crate::rpc::RpcError;
use crate::runtime::{Core, State};
use crate::wire::Conn;
use ghostty_vt::{Carried, Rgb};
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
    /// A drag repaints at every step: images wait for it to end.
    resized_at: Option<Instant>,
    /// Its last paint was of the alternate screen, which goes by way of clearing the main one.
    main_lost: bool,
    /// The main screen's text came from a paint, which breaks wrapped lines for good and
    /// leaves history out, so its own reflow won't come out as the screen's.
    reflows_apart: bool,
}

#[derive(Clone, Copy)]
struct Owed {
    since: Instant,
    /// The images that paint carried, if any.
    carried: Option<u64>,
    /// It can do without: not worth what a paint can't bring back.
    spare: bool,
}

/// What a viewer is sent, and what it has afterwards.
#[derive(Clone, Copy, PartialEq)]
enum Paint {
    Images,
    Text,
    /// Of a screen with no images to bring.
    Empty,
}

impl Viewer {
    fn quiet(&self, output_quiet: Duration, now: Instant) -> Duration {
        self.resized_at.map_or(output_quiet, |at| output_quiet.min(now.saturating_duration_since(at)))
    }

    /// Whether a repaint it needs now can carry images.
    fn repaint_has_images(&self, quiet: Duration, now: Instant) -> bool {
        quiet >= IMAGES_REST || self.owed.is_some_and(|o| now >= o.since + IMAGES_AT_MOST)
    }

    /// How long until it should get the images it's owed. `None`: it isn't owed
    /// any, or only more output can change that.
    fn images_due_in(&self, quiet: Duration, now: Instant) -> Option<Duration> {
        let owed = self.owed?;
        let at_most = (owed.since + IMAGES_AT_MOST).saturating_duration_since(now);
        Some(at_most.min(IMAGES_REST.saturating_sub(quiet)))
    }

    /// `flowing`: output hadn't rested, so an image may be part way through arriving, and the
    /// raw stream can't complete one this paint cut in two. It gets a look once output rests.
    fn painted(&mut self, now: Instant, paint: Paint, carried: Carried, flowing: bool) {
        self.owed = match paint {
            // Text alone doesn't count as getting anywhere, or output could put images off for good.
            Paint::Text => Some(Owed { since: self.owed.map_or(now, |o| o.since), carried: None, spare: false }),
            Paint::Images => flowing.then_some(Owed { since: now, carried: Some(carried.images), spare: true }),
            Paint::Empty => flowing.then_some(Owed { since: now, carried: None, spare: true }),
        };
    }

    /// A paint went out.
    fn gave(&mut self, alt: bool, apart: bool) {
        // The alternate screen doesn't reflow, on either side.
        if !alt {
            self.reflows_apart = apart;
        }
        self.main_lost = alt;
    }

    fn wake_if_owed(&self) {
        if self.owed.is_some() {
            self.conn.wake.notify_one();
        }
    }

    /// What it shows isn't what the screen has: a paint has to say.
    fn owe_paint(&mut self, now: Instant) {
        let since = self.owed.map_or(now, |o| o.since);
        self.owed = Some(Owed { since, carried: None, spare: false });
    }

    /// The same at a size that may still be changing.
    fn owe_for_layout(&mut self, now: Instant) {
        let spare = self.owed.is_none_or(|o| o.spare);
        self.owe_paint(now);
        self.owed.as_mut().expect("just owed").spare = spare;
        self.resized_at = Some(now);
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
            resized_at: None,
            main_lost: false,
            reflows_apart: false,
        });
        if !req.observe {
            self.apply_size(&mut st, id);
        }
        let paint = first_paint(&st, id, req.history);
        let (flowing, carried) = (output_flowing(&st, id), st.screens[&id].images_carried());
        let alt = st.screens[&id].alt_screen();
        let apart = reflows_apart(&st.screens[&id]);
        let viewer = st.viewers.get_mut(&id).unwrap().last_mut().unwrap();
        viewer.repaint_pending = false;
        viewer.painted(Instant::now(), Paint::Images, carried, flowing);
        viewer.gave(alt, apart);
        conn.with(|out| {
            out.open_paused(channel);
            if req.observe && req.resize_notifications {
                let t = &st.reg.terminals[&id];
                let size = [t.cols.to_le_bytes(), t.rows.to_le_bytes()].concat();
                out.resize(channel, &size);
            }
            out.paint(channel, &paint);
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
        let alt = screen.alt_screen();
        let (out, recovered) =
            if recovering { relay.feed_to_ground(screen, bytes) } else { (relay.feed(screen, bytes), None) };
        let alt_changed = screen.alt_screen() != alt;
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
        for v in st.viewers.get_mut(&id).into_iter().flatten() {
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
                v.painted(now, recovered_paint, Carried::default(), true);
                v.gave(full_screen, apart);
                v.conn.with(|ob| ob.paint(v.channel, paint));
                continue;
            }
            // Its backlog went: a repaint follows, built once it has credit for one.
            v.conn.with(|ob| {
                v.repaint_pending = !ob.data(v.channel, &out);
            });
            // Back on a main screen it has none of: a paint has to bring it.
            if alt_changed && !full_screen && v.main_lost {
                v.owe_paint(now);
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
        let direct = direct_images_in_view(st, id);
        self.apply_size(st, id);
        let direct = direct || direct_images_in_view(st, id);
        let alt = st.screens.get(&id).is_some_and(|screen| screen.alt_screen());
        let (_, v) = Self::viewer(st, conn, channel)?;
        // A viewer's image is where its own reflow left it, which may not be where the
        // screen's did.
        if direct && !alt && v.reflows_apart {
            v.owe_for_layout(Instant::now());
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
        st.screens.get_mut(&id).expect("live screen").resize(cols, rows);
        rec.cols = cols;
        rec.rows = rows;
        if let Some(link) = st.link(id) {
            link.send(&proto::json_frame(proto::RESIZE, id, &proto::Size { cols, rows }));
        }
        let size = [cols.to_le_bytes(), rows.to_le_bytes()].concat();
        for v in st.viewers.get_mut(&id).into_iter().flatten().filter(|v| v.resize_notifications) {
            v.repaint_pending = true;
            v.resized_at = Some(Instant::now());
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
        let flowing = output_quiet < IMAGES_REST;
        let mut again = None;
        let screen = &st.screens;
        let carried = std::cell::OnceCell::<Carried>::new();
        let carried_now = || {
            *carried.get_or_init(|| {
                screen[&id].look_at_images();
                screen[&id].images_carried()
            })
        };
        let mut paints = Vec::new();
        let mut settled = Vec::new();
        for (index, v) in st.viewers.get(&id).into_iter().flatten().enumerate() {
            let due = v.repaint_pending || (v.owed.is_some() && conn.is_some());
            if !due || conn.is_some_and(|conn| !Arc::ptr_eq(&v.conn, conn) || !conn.outbox().can_paint(v.channel)) {
                continue;
            }
            // The last it will ever get: the program's gone and the stream closes behind it.
            if conn.is_none() {
                paints.push((index, Paint::Text));
                continue;
            }
            let quiet = v.quiet(output_quiet, now);
            match v.images_due_in(quiet, now) {
                _ if v.repaint_pending => {
                    let images = v.repaint_has_images(quiet, now);
                    paints.push((index, if images { Paint::Images } else { Paint::Text }));
                }
                Some(wait) if wait.is_zero() => {
                    let carried = carried_now();
                    // A paint clears what's in view: of this it can't bring all, which the stream may have.
                    let keeps = carried.refused && v.owed.is_some_and(|o| o.spare);
                    if keeps || v.owed.and_then(|o| o.carried) == Some(carried.images) {
                        // It already has all a paint could bring.
                        settled.push(index);
                    } else {
                        paints.push((index, Paint::Images));
                    }
                }
                Some(wait) => look_again(&mut again, wait),
                None => {}
            }
        }
        for index in settled {
            // Nothing reached it: this was the look.
            st.viewers.get_mut(&id).expect("settled viewers")[index].owed = None;
        }
        if !paints.is_empty() {
            // With none on the screen there's nothing to hold back, but what it has to clear.
            if !st.screens[&id].has_images() {
                paints.iter_mut().filter(|p| p.1 == Paint::Text).for_each(|p| p.1 = Paint::Images);
            }
            let any = |paint| paints.iter().any(|p| p.1 == paint);
            let with_images = any(Paint::Images).then(|| repaint(st, id));
            let text = any(Paint::Text).then(|| with_state(st, id, st.screens[&id].paint_text(0)));
            let screen = &st.screens[&id];
            let recovering = screen.continuation().is_none();
            let carried = screen.images_carried();
            let (alt, apart) = (screen.alt_screen(), reflows_apart(screen));
            let viewers = st.viewers.get_mut(&id).expect("viewers to paint");
            for (index, paint) in paints {
                let v = &mut viewers[index];
                let looked = !v.repaint_pending;
                let bytes = match paint {
                    Paint::Images => &with_images,
                    Paint::Text => &text,
                    Paint::Empty => unreachable!("only a recovered viewer gets this"),
                };
                let bytes = bytes.as_ref().expect("built above");
                v.conn.with(|out| out.paint(v.channel, bytes));
                v.repaint_pending = false;
                v.recovering = recovering;
                // A look that found something doesn't look again, or a stream of images would keep it painted.
                v.painted(now, paint, carried, flowing && !looked);
                v.gave(alt, apart);
            }
        }
        again
    }

    /// A read scrolled the app unseen: every viewer needs the screen back. One still
    /// receiving a paint gets this after it, or reads could keep a paint from ever ending.
    pub(crate) fn repaint_after_read(&self, st: &mut State, id: u32) {
        let now = Instant::now();
        let free = |v: &Viewer| v.conn.outbox().can_paint(v.channel);
        let paint = st.viewers.get(&id).into_iter().flatten().any(free).then(|| repaint(st, id));
        let recovering = st.screens[&id].continuation().is_none();
        let carried = st.screens[&id].images_carried();
        let alt = st.screens[&id].alt_screen();
        let apart = reflows_apart(&st.screens[&id]);
        for v in st.viewers.get_mut(&id).into_iter().flatten() {
            match paint.as_ref().filter(|_| free(v)) {
                Some(bytes) => {
                    v.conn.with(|out| out.paint(v.channel, bytes));
                    v.repaint_pending = false;
                    v.recovering = recovering;
                    v.painted(now, Paint::Images, carried, false);
                    v.gave(alt, apart);
                }
                None => {
                    v.repaint_pending = true;
                    v.conn.wake.notify_one();
                }
            }
        }
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

fn direct_images_in_view(st: &State, id: u32) -> bool {
    st.screens.get(&id).is_some_and(|screen| screen.direct_images_in_view())
}

/// Whether a viewer given this screen by a paint could reflow away from it: a paint makes
/// its soft wraps hard and leaves history out, and there's only no text to be sure of that.
fn reflows_apart(screen: &ghostty_vt::Terminal) -> bool {
    screen.history_rows() > 0 || screen.screen_rows().iter().any(|row| !row.trim().is_empty())
}

fn output_flowing(st: &State, id: u32) -> bool {
    st.last_output.get(&id).is_some_and(|at| at.elapsed() < IMAGES_REST)
}

/// The screen, then the latest 7501 report of every record and whether the
/// program wants win32 key events (both pass through to viewers, so a new one
/// must see them too; a viewer repainted after falling behind gets the mode
/// either way).
pub(crate) fn first_paint(st: &State, id: u32, history: usize) -> Vec<u8> {
    with_state(st, id, st.screens[&id].paint(history))
}

/// `first_paint` for a viewer already showing this screen.
fn repaint(st: &State, id: u32) -> Vec<u8> {
    with_state(st, id, st.screens[&id].repaint())
}

fn look_again(again: &mut Option<Duration>, wait: Duration) {
    *again = Some(again.map_or(wait, |soonest| soonest.min(wait)));
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
