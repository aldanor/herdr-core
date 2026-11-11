//! Program output → the core's screen and the bytes viewers get.
//! Every byte reaches the screen. Viewers get every byte too, except queries
//! the screen answered itself: a viewer seeing them would answer a second time.
//! Images (APC: kitty graphics) are the other way round: the screen takes
//! them (they move the cursor) but its replies are dropped, since it can't
//! show them; a viewer that can answers. Programs read replies in order (DA1
//! after kitty's query is how they learn images don't work), so the screen's
//! DA1 after an image waits for the viewer's answer, briefly.

use ghostty_vt::Terminal as Screen;
use std::time::Duration;

mod graphics;
pub(crate) use graphics::Answers;

/// How long DA1 waits behind an image for a viewer's answer.
pub fn image_answer_wait() -> Duration {
    crate::input::env_ms("HC_IMAGE_ANSWER_MS", 100)
}

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const BEL: u8 = 0x07;
/// Queries are short; anything longer is passed through unexamined.
const MAX_CSI: usize = 64;
const MAX_STRING: usize = 512;

#[derive(Debug, Default, PartialEq)]
pub struct Named {
    /// Images commands placed, by generation.
    pub images: Vec<u64>,
    /// What the screen held at the last change of screen, by generation.
    pub switched: Option<u64>,
}

#[derive(Default)]
pub struct Relay {
    /// What's gone to viewers so far ends mid-sequence, for whatever comes next to end.
    viewer_unfinished: bool,
    /// An escape sequence still arriving, kept back until it can be judged.
    held: Vec<u8>,
    /// Inside an APC: how many reply bytes were waiting when it began.
    apc: Option<usize>,
    /// The program's latest win32-input-mode request (DEC 9001, ConPTY's ask
    /// for key events), until taken. The screen doesn't keep it.
    pub win32_input: Option<bool>,
    /// An image the screen answered went to viewers, and none has answered yet.
    waiting: bool,
    /// Where, in the screen's pending replies, those after that image begin.
    behind_from: Option<usize>,
    /// The screen's replies waiting for a viewer's answer.
    behind: Vec<u8>,
    /// Counts waits, so a deadline only ends the wait it was set for.
    wait: u64,
    wait_started: bool,
    graphics: graphics::Parser,
    query: u64,
    queries: Vec<(graphics::Key, u64)>,
    unanswered: Vec<u64>,
    pub loads: Loads,
}

/// Longest command head kept: real ones are far shorter, however many keys they use.
const HEAD_MAX: usize = 1024;

/// Follows kitty commands just far enough to tell whether a chunked image is still
/// arriving: a paint can't carry that one, and the screen's API can't tell. It looks
/// only at what follows `ESC _`, so a terminal without images pays nothing. A crafted
/// command it reads differently from Ghostty can cost a viewer a late or a needless paint.
#[derive(Default)]
pub struct Loads {
    /// A chunk has said more follow, and the last hasn't reached the screen. Each
    /// screen has its own: main, alternate.
    open: [bool; 2],
    /// How many have ended, which is what's new since a viewer's last paint.
    finished: u64,
    /// Each start and end of a command: what a viewer can't miss and still have what
    /// the screen has, though nothing has changed there yet.
    seen: u64,
    /// The images commands have placed since last asked, by generation: for one a viewer
    /// never got, these commands do nothing there.
    named: Vec<u64>,
    /// What the screen held at the last change of screen, by generation.
    switched: Option<u64>,
    scan: Scan,
    head: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
pub struct LoadsNow {
    pub open: bool,
    pub finished: u64,
}

#[derive(Default, PartialEq)]
enum Scan {
    #[default]
    Outside,
    /// Just after `ESC _`: the next byte says which protocol.
    Kind,
    Head,
    Body,
}

impl Loads {
    pub fn now(&self) -> LoadsNow {
        LoadsNow { open: self.open[0] || self.open[1], finished: self.finished }
    }

    pub fn seen(&self) -> u64 {
        self.seen
    }

    pub fn take_named(&mut self) -> Named {
        Named { images: std::mem::take(&mut self.named), switched: self.switched.take() }
    }

    fn set_open(&mut self, alt: bool, open: bool) {
        self.finished += (self.open[alt as usize] && !open) as u64;
        self.open[alt as usize] = open;
    }

    pub fn in_command(&self) -> bool {
        matches!(self.scan, Scan::Head | Scan::Body)
    }

    /// Reads what's there of a head, up to the ESC that `feed` will see for itself.
    fn read_head(&mut self, bytes: &[u8]) {
        let mut bytes = bytes.iter();
        while matches!(self.scan, Scan::Kind | Scan::Head) {
            let Some(&b) = bytes.next().filter(|b| !matches!(**b, ESC | CAN | SUB)) else { return };
            self.scan = match self.scan {
                Scan::Kind if b == b'G' => {
                    self.seen += 1;
                    Scan::Head
                }
                Scan::Head if b == b';' => Scan::Body,
                Scan::Head => {
                    if self.head.len() < HEAD_MAX {
                        self.head.push(b);
                    }
                    Scan::Head
                }
                _ => Scan::Outside,
            };
        }
    }

    /// The keys that say what a command does to a load, as real programs write them.
    fn keys(&self) -> Keys {
        let mut keys = Keys { action: b't', direct: true, more: false, id: 0 };
        for kv in self.head.split(|b| *b == b',') {
            let Some(at) = kv.iter().position(|b| *b == b'=') else { continue };
            let (key, value) = (&kv[..at], &kv[at + 1..]);
            let number = || std::str::from_utf8(value).ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            match (key, value) {
                (b"a", [action]) => keys.action = *action,
                (b"t", [medium]) => keys.direct = *medium == b'd',
                (b"m", _) => keys.more = number() > 0,
                (b"i", _) => keys.id = number(),
                _ => {}
            }
        }
        keys
    }

    /// As Ghostty's loader, just before the screen sees what ends the command: only one
    /// sent directly can have more chunks, and a delete abandons what was arriving.
    /// Placing and querying leave it alone.
    fn command(&mut self, screen: &Screen) {
        self.seen += 1;
        let alt = screen.alt_screen();
        let keys = self.keys();
        match keys.action {
            b't' | b'T' | b'f' => self.set_open(alt, keys.more && keys.direct),
            b'd' => self.set_open(alt, false),
            _ => {}
        }
        if keys.action == b'p' {
            self.named.extend(screen.images_named(&[keys.id]));
        }
        self.head.clear();
    }
}

struct Keys {
    action: u8,
    direct: bool,
    more: bool,
    id: u32,
}

enum Seq {
    Pending,
    Done { maybe_query: bool },
}

impl Relay {
    /// In-band resize belongs to the viewer, which knows the pixel geometry.
    /// Ghostty also emits it when 2048 is enabled and when its screen resizes.
    pub fn take_responses(screen: &mut Screen) -> Vec<u8> {
        without_resize_reports(&screen.take_responses())
    }

    /// Only DA1 after an image waits until a viewer answers or the wait ends.
    /// With no viewer to answer (`answering`
    /// false), everything goes. Returns the wait that began, which the caller
    /// ends with `expire` after `image_answer_wait()`.
    pub fn take_replies(&mut self, screen: &mut Screen, answering: bool) -> (Vec<u8>, Option<u64>) {
        let replies = screen.take_responses();
        if !answering {
            self.queries.clear();
            self.wait_started = false;
            let mut now = self.end_wait();
            now.extend(without_resize_reports(&replies));
            return (now, None);
        }
        let split = match self.behind_from.take() {
            Some(at) => at.min(replies.len()),
            None if self.waiting => 0,
            None => replies.len(),
        };
        let mut now = without_resize_reports(&replies[..split]);
        let after = without_resize_reports(&replies[split..]);
        let mut i = 0;
        while i < after.len() {
            if let Some(n) = da1_len(&after[i..]) {
                self.behind.extend_from_slice(&after[i..i + n]);
                i += n;
                continue;
            }
            now.push(after[i]);
            i += 1;
        }
        let started = std::mem::take(&mut self.wait_started).then_some(self.wait);
        (now, started)
    }

    pub(crate) fn expect_answers<'a>(&mut self, viewers: impl Iterator<Item = &'a mut Answers>) {
        let queries = std::mem::take(&mut self.queries);
        for viewer in viewers {
            for (key, query) in &queries {
                viewer.expect(key.clone(), *query);
            }
        }
    }

    /// Only complete, correlated answers can end this wait.
    pub(crate) fn answered(&mut self, queries: &[(usize, u64)]) -> Vec<u8> {
        self.unanswered.retain(|query| !queries.iter().any(|(_, answer)| query == answer));
        if self.waiting && self.unanswered.is_empty() { self.end_wait() } else { Vec::new() }
    }

    /// Ends `wait` if it is still going: no viewer answered in time.
    pub fn expire(&mut self, wait: u64) -> Vec<u8> {
        if self.wait == wait && self.waiting { self.end_wait() } else { Vec::new() }
    }

    pub(crate) fn end_wait(&mut self) -> Vec<u8> {
        self.unanswered.clear();
        self.waiting = false;
        self.behind_from = None;
        std::mem::take(&mut self.behind)
    }

    /// Feeds program output; returns what viewers should get. Replies the
    /// screen produced are left in it for the caller to send to the program.
    pub fn feed(&mut self, screen: &mut Screen, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        let mut run = 0;
        if self.held.is_empty() {
            self.loads.read_head(input);
        }
        let mut next = 0;
        while next < input.len() {
            let (i, b) = (next, input[next]);
            next += 1;
            let image = self.graphics.push(b);
            if self.held.is_empty() {
                if b == ESC {
                    pass(screen, &mut out, &input[run..i]);
                    self.held.push(b);
                    continue;
                }
                // Ghostty runs a command at these too.
                if self.loads.in_command() && matches!(b, CAN | SUB) {
                    self.loads.command(screen);
                    self.loads.scan = Scan::Outside;
                }
                // Nothing else matters until the next of those.
                let mut rest = input[next..].iter();
                let found = if self.loads.in_command() {
                    rest.position(|b| matches!(*b, ESC | CAN | SUB))
                } else {
                    rest.position(|b| *b == ESC)
                };
                // The image parser still has to see all of it.
                let skip = found.map_or(input.len(), |n| next + n);
                input[next..skip].iter().for_each(|b| _ = self.graphics.push(*b));
                next = skip;
                continue;
            }
            // Ghostty starts over at an ESC inside a sequence, unless it can be the end of a string.
            // A DCS stays whole: what's in tmux's passthrough has its ESCs doubled, and it's for
            // whoever's watching to make sense of.
            let after_esc = self.held.last() == Some(&ESC);
            let cut = match self.held.get(1) {
                Some(b'P') => None,
                Some(b']') => (after_esc && self.held.len() > 2 && b != b'\\').then(|| self.held.len() - 1),
                _ => (b == ESC).then_some(self.held.len()),
            };
            if let Some(cut) = cut {
                let rest = self.held.split_off(cut);
                if self.loads.in_command() {
                    self.loads.command(screen);
                }
                self.end_apc(screen);
                let seq = std::mem::replace(&mut self.held, rest);
                // A viewer gets all of it, as it came. A query in a string it ends here is
                // answered here, though, for which the screen has to see it end. A lone ESC
                // says nothing to the screen that the next one doesn't.
                let answered = seq.len() > 1 && {
                    let before = screen.pending_responses();
                    screen.write(&seq);
                    if seq.get(1) == Some(&b']') {
                        screen.write(b"\x1b\\");
                    }
                    screen.pending_responses() != before
                };
                if !answered {
                    out.extend_from_slice(&seq);
                    self.viewer_unfinished = true;
                }
                self.loads.scan = Scan::Outside;
                // The ESC just seen starts one of its own, whatever was before it.
                if b == ESC {
                    pass(screen, &mut out, &std::mem::take(&mut self.held));
                    self.viewer_unfinished = true;
                    self.held.push(ESC);
                    run = i + 1;
                    continue;
                }
            }
            self.held.push(b);
            if let Seq::Done { maybe_query } = self.judge() {
                let seq = std::mem::take(&mut self.held);
                // The ESC of this ends any command before it, on the screen it arrived on.
                if self.loads.in_command() {
                    self.loads.command(screen);
                }
                if seq != b"\x1b\\" {
                    self.end_apc(screen);
                }
                if maybe_query {
                    let before = screen.pending_responses();
                    screen.write(&seq);
                    if screen.pending_responses() == before {
                        out.extend_from_slice(&seq); // not answered here: the viewer may
                    } else if self.viewer_unfinished {
                        // Taking this out would leave the viewer in what this one ended.
                        out.extend_from_slice(b"\x1b\\");
                    }
                } else {
                    if let Some(on) = win32_input(&seq) {
                        self.win32_input = Some(on);
                    }
                    if seq == b"\x1b_" {
                        self.apc = Some(screen.pending_responses());
                    }
                    let alt = seq.starts_with(b"\x1b[?").then(|| screen.alt_screen());
                    pass(screen, &mut out, &seq);
                    if alt.is_some_and(|alt| alt != screen.alt_screen()) {
                        self.loads.switched = Some(screen.images_generation());
                    }
                    // An APC ends with ST; whatever the screen answered to it goes,
                    // and an answer means a viewer that shows images answers too.
                    if seq == b"\x1b\\"
                        && let Some(keep) = self.apc.take()
                    {
                        if screen.pending_responses() > keep
                            && let Some(image) = image.filter(|s| graphics::expects_answer(s))
                            && let Some(key) = graphics::key(&image)
                        {
                            self.query += 1;
                            self.queries.push((key, self.query));
                            self.unanswered.push(self.query);
                            if !self.waiting {
                                self.waiting = true;
                                self.wait += 1;
                                self.wait_started = true;
                                self.behind_from = Some(keep);
                            }
                        }
                        screen.drop_responses_after(keep);
                    }
                }
                self.viewer_unfinished = false;
                self.loads.scan = Scan::Outside;
                // A full reset drops what either screen was loading.
                if seq == b"\x1bc" {
                    self.loads.set_open(false, false);
                    self.loads.set_open(true, false);
                }
                if seq == b"\x1b_" {
                    self.loads.scan = Scan::Kind;
                    self.loads.read_head(&input[i + 1..]);
                }
                run = i + 1;
            }
        }
        if self.held.is_empty() {
            pass(screen, &mut out, &input[run..]);
        }
        out
    }

    /// An APC cut short by another sequence: what the screen answered to it goes, and a
    /// viewer's is left to end the same way.
    fn end_apc(&mut self, screen: &mut Screen) {
        if let Some(keep) = self.apc.take() {
            screen.write(b"\x1b\\");
            screen.drop_responses_after(keep);
            self.viewer_unfinished = true;
        }
    }

    /// A viewer missed a sequence too large to replay. Consume just its tail,
    /// capturing a repaint at ground even if this read also starts another image.
    pub fn feed_to_ground(&mut self, screen: &mut Screen, input: &[u8]) -> (Vec<u8>, Option<(usize, Vec<u8>)>) {
        let mut bytes = std::mem::take(&mut self.held);
        bytes.extend_from_slice(input);
        let Some(n) = screen.write_until_ground(&bytes) else {
            return (bytes, None);
        };
        if let Some(keep) = self.apc.take() {
            screen.drop_responses_after(keep);
        }
        if self.loads.in_command() {
            self.loads.command(screen);
        }
        self.loads.scan = Scan::Outside;
        // Output is still flowing: images wait for it to rest, like any other repaint's.
        #[cfg(test)]
        crate::viewers::tests::PAINTS.with(|n| n.set(n.get() + 1));
        let paint = if screen.has_images() { screen.paint_text(0) } else { screen.repaint() };
        let mut out = bytes[..n].to_vec();
        out.extend(self.feed(screen, &bytes[n..]));
        (out, Some((n, paint)))
    }

    /// Writes a half-received sequence to the screen (before a snapshot).
    pub fn flush(&mut self, screen: &mut Screen) {
        screen.write(&std::mem::take(&mut self.held));
    }

    fn judge(&self) -> Seq {
        let h = &self.held;
        let last = *h.last().expect("held starts with ESC");
        match h.get(1) {
            None => Seq::Pending,
            Some(b'[') if h.len() > 2 && (0x40..=0x7e).contains(&last) => {
                Seq::Done { maybe_query: b"cnpqtu".contains(&last) }
            }
            Some(b'[') if h.len() > MAX_CSI => Seq::Done { maybe_query: false },
            Some(b'[') => Seq::Pending,
            Some(b']') | Some(b'P') => {
                let ended = last == BEL && h[1] == b']' || h.ends_with(b"\x1b\\") && h.len() > 3;
                if ended {
                    let body = &h[2..];
                    let query = match h[1] {
                        b']' => body.contains(&b'?'),
                        _ => body.starts_with(b"$q") || body.starts_with(b"+q"),
                    };
                    Seq::Done { maybe_query: query }
                } else if h.len() > MAX_STRING {
                    Seq::Done { maybe_query: false }
                } else {
                    Seq::Pending
                }
            }
            Some(_) => Seq::Done { maybe_query: false },
        }
    }
}

pub(crate) fn da1_len(bytes: &[u8]) -> Option<usize> {
    let rest = bytes.strip_prefix(b"\x1b[?")?;
    let n = rest.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
    (rest.get(n) == Some(&b'c')).then_some(3 + n + 1)
}

fn without_resize_reports(replies: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(replies.len());
    let mut i = 0;
    while i < replies.len() {
        if let Some(rest) = replies[i..].strip_prefix(b"\x1b[48;") {
            let n = rest.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
            if rest.get(n) == Some(&b't') {
                i += 5 + n + 1;
                continue;
            }
        }
        out.push(replies[i]);
        i += 1;
    }
    out
}

/// `CSI ? … 9001 … h` or `l`: whether it turns win32-input-mode on.
fn win32_input(seq: &[u8]) -> Option<bool> {
    let (last, params) = seq.strip_prefix(b"\x1b[?")?.split_last()?;
    let on = match last {
        b'h' => true,
        b'l' => false,
        _ => return None,
    };
    params.split(|b| *b == b';').any(|p| p == b"9001").then_some(on)
}

fn pass(screen: &mut Screen, out: &mut Vec<u8>, bytes: &[u8]) {
    if !bytes.is_empty() {
        screen.write(bytes);
        out.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_escape_interrupted_by_another_does_not_hide_what_follows() {
        for prefix in [
            &b"\x1b"[..],
            b"\x1b[",
            b"\x1b[12;",
            b"\x1b]2;abandoned",
            b"\x1b]2;abandoned\x1b",
            b"\x1b]2;abandoned\x1b\x1b",
        ] {
            let mut stream = prefix.to_vec();
            stream.extend(b"\x1b_Ga=T,f=32,s=2,v=1,i=2,q=2,m=1;AQID\x1b\\tail");
            let (relay, screen) = fed(&stream);
            assert!(relay.loads.now().open, "{prefix:?}");
            assert!(relay.held.is_empty(), "{prefix:?}");
            assert!(screen.plain_text().contains("tail"), "{prefix:?}");
        }
    }

    #[test]
    fn an_escape_interrupting_a_command_ends_it() {
        let (relay, _screen) = fed(b"\x1b_Ga=t,f=32,s=1,v=1,i=1,m=1,q=2;AAAA\x1b_Gm=0;AA==");
        assert!(relay.loads.in_command(), "the next one has begun");
        let (mut relay, mut screen) = fed(b"\x1b_Ga=t,f=32,s=1,v=1,i=1,m=1,q=2;AAAA\x1b\x1b");
        assert!(!relay.loads.in_command());
        assert!(relay.loads.now().open, "that one said more was to come");
        relay.feed(&mut screen, b"[m");
        assert!(!relay.loads.in_command());
        assert!(relay.loads.now().open, "and it isn't ended twice");
    }

    #[test]
    fn what_a_command_places_is_named_once() {
        const SENT: &[u8] = b"\x1b_Ga=t,f=32,s=1,v=1,i=3,q=2;AAAAAA==\x1b\\";
        let (mut relay, mut screen) = fed(SENT);
        let before = screen.images_named(&[3]);
        assert_eq!(before.len(), 1);
        assert_eq!(taken(&mut relay), (vec![], None), "it has this from the stream");
        relay.feed(&mut screen, b"\x1b_Ga=p,i=3,q=2\x1b\\\x1b_Ga=p,i=9,p=1,q=2\x1b\\");
        assert_eq!(taken(&mut relay), (before, None), "and none for what isn't there");
        assert_eq!(taken(&mut relay), (vec![], None), "just the once");
        let (mut relay, _screen) = fed(b"\x1b_Ga=q,f=32,s=1,v=1,i=3,q=2;AAAAAA==\x1b\\");
        assert_eq!(taken(&mut relay), (vec![], None));
    }

    #[test]
    fn a_change_of_screen_says_what_the_new_one_held_then() {
        let (mut relay, mut screen) = fed(b"\x1b[?47h\x1b_Ga=T,f=32,s=1,v=1,i=3,c=2,r=1,q=2;AAAAAA==\x1b\\");
        let switched = relay.loads.take_named().switched;
        assert!(switched.is_some_and(|at| at < screen.images_generation()), "that one came after");
        relay.feed(&mut screen, b"\x1b[?47l\x1b[?47h");
        let switched = relay.loads.take_named().switched;
        assert_eq!(switched, Some(screen.images_generation()), "and this time it was there before");
        relay.feed(&mut screen, b"\x1b[?25l\x1b[?1000h");
        assert_eq!(relay.loads.take_named().switched, None);
    }

    #[test]
    fn a_command_is_read_by_the_keys_real_programs_write() {
        let opens = |head: &str| {
            let (relay, _screen) = fed(format!("\x1b_G{head};AAAA\x1b\\").as_bytes());
            relay.loads.now().open
        };
        for head in ["m=1", "a=t,m=1", "a=T,m=1,q=2", "a=t,xy=1,m=1", "a=t,b,m=1", "a=t,m=1,o=z", "a=t,m=1,f=7"] {
            assert!(opens(head), "{head}");
        }
        for head in ["a=t", "a=t,m=0", "a=t,mm=1", "a=t,m=1,t=f", "a=q,m=1", "a=p,m=1"] {
            assert!(!opens(head), "{head}");
        }
        let (mut relay, _screen) = fed(b"\x1b_Ga=t,f=32,s=1,v=1,i=3,q=2;AAAAAA==\x1b\\\x1b_Ga=p,i=3,t=x,o=x,q=2\x1b\\");
        assert_eq!(relay.loads.take_named().images.len(), 1, "those keys mean nothing to a placement");
    }

    #[test]
    fn can_and_sub_end_a_command_as_an_escape_does() {
        for end in [0x18u8, 0x1a] {
            let mut stream = b"\x1b_Ga=t,f=32,s=1,v=1,i=1,m=1,q=2;AAAA\x1b\\\x1b_Gm=0;AA==".to_vec();
            stream.push(end);
            stream.extend(b"tail");
            let (relay, screen) = fed(&stream);
            assert_eq!(relay.loads.now().finished, 1, "{end}");
            assert!(!relay.loads.in_command());
            assert!(screen.plain_text().contains("tail"));
            assert!(screen.image_pixels(1).is_some());
        }
        let (relay, _screen) = fed(b"\x1b_Ga=t,m=1\x18x;y");
        assert!(relay.loads.now().open, "the head stops there too");
        let (relay, _screen) = fed(b"\x1b_Ga=t,f=32,s=1,v=1,i=1,m=1,q=2;AAAA\x1b\\\x18plain\x18text");
        assert!(relay.loads.now().open, "out of a command they say nothing");
    }

    #[test]
    fn what_a_viewer_is_passed_comes_to_the_same_as_the_screen_however_a_sequence_is_cut_short() {
        for stream in [
            &b"\x1b]2;title\x1b[6ntext"[..],
            b"\x1b\x1b[6ntext",
            b"\x1b[6\x1b[6ntext",
            b"\x1b]2;title\x1b7text",
            b"\x1b]10;?\x1b[6ntext",
            b"\x1b]10;?\x1b\x1b[6ntext",
            b"\x1b]2;title\x1b\x1b[6ntext",
            b"\x1b_Ga=q,i=1,s=1,v=1,f=32;AAAAAA==\x1b[6ntext",
            b"\x1b_Ga=q,i=1,s=1,v=1,f=32;AAAAAA==\x1b\x1b[6ntext",
            b"\x1b_x\x1b_y\x1b\\text",
        ] {
            let (mut relay, mut screen) = fed(b"");
            let mut viewer = Screen::new(80, 24, 1 << 20);
            for piece in [stream, b"\r\nend"] {
                viewer.write(&relay.feed(&mut screen, piece));
            }
            assert_eq!(viewer.plain_text(), screen.plain_text(), "{stream:?}");
            assert_eq!(viewer.title(), screen.title(), "{stream:?}");
            assert!(screen.plain_text().contains("text"), "{stream:?}");
        }
    }

    #[test]
    fn a_query_with_nothing_left_unfinished_goes_without_a_trace() {
        let (mut relay, mut screen) = fed(b"");
        assert_eq!(relay.feed(&mut screen, b"\x1b]2;title\x1b7\x1b[6nmore"), b"\x1b]2;title\x1b7more");
        assert_eq!(Relay::take_responses(&mut screen).len(), 6);
    }

    #[test]
    fn a_query_after_an_apc_cut_short_is_still_answered() {
        let (mut relay, mut screen) = fed(b"\x1b_Ga=q,i=1,s=1,v=1,f=32;AAAAAA==\x1b[6n\x1b\\");
        assert!(relay.apc.is_none());
        let replies = Relay::take_responses(&mut screen);
        assert!(replies.ends_with(b"R"), "the cursor's position: {replies:?}");
        assert!(!replies.windows(3).any(|w| w == b"\x1b_G"), "not the image's: {replies:?}");
        relay.feed(&mut screen, b"plain");
        assert!(screen.plain_text().contains("plain"));
    }

    fn taken(relay: &mut Relay) -> (Vec<u64>, Option<u64>) {
        let named = relay.loads.take_named();
        (named.images, named.switched)
    }

    #[test]
    fn tmux_passthrough_goes_through_as_it_came() {
        let short = b"\x1bPtmux;\x1b\x1b_Ga=T,f=32,s=1,v=1,i=1,q=2;AAAAAA==\x1b\x1b\\\x1b\\after".to_vec();
        for stream in [short.clone(), [&b"\x1bPtmux;"[..], &short[7..24], &vec![b'A'; 5000], &short[24..]].concat()] {
            let (mut relay, mut screen) = fed(b"");
            assert_eq!(relay.feed(&mut screen, &stream), stream);
            assert!(relay.held.is_empty());
            for split in (1..stream.len()).step_by(37).chain(5000..5030) {
                let split = split.min(stream.len() - 1);
                let (mut relay, mut screen) = fed(b"");
                let mut out = relay.feed(&mut screen, &stream[..split]);
                out.extend(relay.feed(&mut screen, &stream[split..]));
                assert_eq!(out, stream, "split at {split}");
            }
        }
    }

    #[test]
    fn viewers_get_every_byte_of_what_nothing_here_answers() {
        const BYTES: &[u8] = b"\x1b\x1b\x1b\x1b[]P_\\Gx;1m\x07\x18=,a";
        let mut state = 88172645463325252u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize
        };
        for _ in 0..3000 {
            let stream: Vec<u8> = (0..next() % 40).map(|_| BYTES[next() % BYTES.len()]).collect();
            let split = next() % (stream.len() + 1);
            let (mut relay, mut screen) = fed(b"");
            let mut out = relay.feed(&mut screen, &stream[..split]);
            out.extend(relay.feed(&mut screen, &stream[split..]));
            out.extend_from_slice(&relay.held);
            assert_eq!(out, stream);
        }
    }

    #[test]
    fn the_answer_to_a_query_after_an_apc_is_kept_however_the_query_is_cut_short() {
        let (mut relay, mut screen) = fed(b"\x1b_x\x1b]10;?\x1bXtext\x1b\\");
        assert!(relay.apc.is_none());
        let replies = Relay::take_responses(&mut screen);
        assert!(replies.windows(4).any(|w| w == b"]10;"), "{replies:?}");
        relay.feed(&mut screen, b"plain");
    }

    #[test]
    fn a_query_cut_short_is_answered_once() {
        let (mut relay, mut screen) = fed(b"");
        let out = relay.feed(&mut screen, b"\x1b]10;?\x1bX");
        assert!(screen.pending_responses() > 0, "or this isn't the case it's about");
        assert!(!out.windows(2).any(|w| w == b"]1"), "{out:?}");
    }

    #[test]
    fn an_escape_in_a_string_can_still_end_it_across_reads() {
        let (mut relay, mut screen) = fed(b"\x1b]2;title\x1b");
        relay.feed(&mut screen, b"\\after");
        assert!(relay.held.is_empty());
        assert_eq!(screen.title(), "title");
        assert!(screen.plain_text().contains("after"));
    }

    fn fed(input: &[u8]) -> (Relay, Screen) {
        let (mut relay, mut screen) = (Relay::default(), Screen::new(80, 24, 1 << 20));
        relay.feed(&mut screen, input);
        (relay, screen)
    }

    fn open_after(input: &[u8]) -> bool {
        fed(input).0.loads.now().open
    }

    #[test]
    fn a_load_is_open_from_its_first_chunk_until_its_last_however_the_bytes_arrive() {
        let chunks: &[u8] = b"\x1b_Ga=T,f=32,i=1,m=1;AAAA\x1b\\\x1b_Gm=1;BBBB\x1b\\";
        for step in [1, 7, chunks.len()] {
            let (mut relay, mut screen) = fed(b"");
            for part in chunks.chunks(step) {
                relay.feed(&mut screen, part);
            }
            assert!(relay.loads.now().open);
            relay.feed(&mut screen, b"\x1b_Gm=0;");
            assert!(relay.loads.now().open, "the last chunk hasn't ended");
            relay.feed(&mut screen, b"\x1b");
            assert!(relay.loads.now().open, "the relay still holds this ESC, so the screen hasn't run the chunk");
            relay.feed(&mut screen, b"\\");
            assert!(!relay.loads.now().open);
        }
    }

    #[test]
    fn what_the_screen_takes_for_a_load_is_what_this_takes_for_one() {
        // Whether the next image is taken for a continuation shows what the screen thinks.
        let next = b"\x1b_Ga=T,f=32,s=1,v=1,i=9,q=2;AAAAAA==\x1b\\";
        let first = "\x1b_Ga=t,f=32,s=1,v=1,i=1,q=2,m=1;AAAA\x1b\\";
        for after_first in [false, true] {
            for head in [
                "m=1",
                "m=2",
                "m=01",
                "m=0",
                "m=00",
                "m=1,m=0",
                "m=0,m=1",
                "a=t,m=1",
                "a=T,t=d,q=2,m=1",
                "q=2",
                "",
                "t=f,m=1",
                "a=p,i=1",
                "a=q,i=3,m=1",
                "a=d,d=a",
                "a=d,d=i,i=1",
                "a=f,i=1,m=0",
            ] {
                let input = format!("{}\x1b_G{head};AAAA\x1b\\", if after_first { first } else { "" });
                let (relay, mut screen) = fed(input.as_bytes());
                screen.write(next);
                let taken = screen.image_placements().iter().all(|p| p.image != 9);
                assert_eq!(relay.loads.now().open, taken, "{head:?}, after a first chunk: {after_first}");
            }
        }
    }

    #[test]
    fn other_sequences_around_a_command_do_not_hide_it_or_end_it() {
        let open = b"\x1b_Gm=1;AA\x1b\\";
        assert!(open_after(b"text \x1b[31m\x1b_Gm=1;AA\x1b\\"));
        assert!(open_after(b"\x1b_\x1b_Gm=1;\x1b\\"), "an APC cut short by another");
        assert!(open_after(b"\x1b_Gm=0;AA\x1b_Gm=1;BB\x1b\\"), "the ESC that ended one starts the next");
        assert!(open_after(b"\x1b_Gm=1;AA\x1b[0m"), "any ESC ends a command");
        assert!(!open_after(b"\x1b_Gm=1;AA\x1b\\\x1b_Gm=0;\x1b\x1b_Xnot ours\x1b\\"));
        for other in [&b"\x1b]2;title\x1b\\"[..], b"\x1b_Xother\x1b\\", b"plain", b"\x1b[31m"] {
            let mut input = open.to_vec();
            input.extend_from_slice(other);
            assert!(open_after(&input), "{other:?}");
        }
    }

    #[test]
    fn a_load_on_the_alternate_screen_alone_counts() {
        let (relay, _) = fed(b"\x1b[?1049h\x1b_Gm=1;AA\x1b\\");
        assert!(relay.loads.now().open);
    }

    #[test]
    fn what_has_finished_is_counted_once_and_a_reset_ends_both_screens() {
        let first = |relay: &Relay| relay.loads.now().finished;
        let (mut relay, mut screen) = fed(b"\x1b_Gm=1;AA\x1b\\");
        assert_eq!(first(&relay), 0, "still arriving");
        relay.feed(&mut screen, b"\x1b_Gm=0;AA\x1b\\");
        assert_eq!(first(&relay), 1);
        relay.feed(&mut screen, b"\x1b_Gm=0;AA\x1b\\");
        assert_eq!(first(&relay), 1, "a whole image wasn't arriving");
        relay.feed(&mut screen, b"\x1b_Gm=1;AA\x1b\\\x1b[?1049h\x1b_Gm=1;AA\x1b\\");
        assert!(relay.loads.now().open);
        relay.feed(&mut screen, b"\x1bc");
        assert!(!relay.loads.now().open);
        assert_eq!(first(&relay), 3);
    }

    #[test]
    fn each_screen_has_its_own_load_as_it_does_in_ghostty() {
        let (mut relay, mut screen) = fed(b"\x1b_Ga=t,f=32,s=1,v=1,i=1,m=1;AAAA\x1b\\");
        relay.feed(&mut screen, b"\x1b[?1049h");
        relay.feed(&mut screen, b"\x1b_Ga=t,f=32,s=1,v=1,i=2,m=1;AAAA\x1b\\\x1b_Gm=0;AA==\x1b\\");
        assert!(relay.loads.now().open, "that one ended, and the main screen's goes on");
        relay.feed(&mut screen, b"\x1b[?1049l\x1b_Gm=0;AA==\x1b\\");
        assert!(!relay.loads.now().open);
    }

    #[test]
    fn a_head_split_anywhere_is_read_whole_and_text_is_not_looked_at() {
        let chunk: &[u8] = b"plain \x1b_Gq=2,i=1,m=1;AAAA\x1b\\plain";
        for at in 0..chunk.len() {
            let (mut relay, mut screen) = fed(b"");
            relay.feed(&mut screen, &chunk[..at]);
            relay.feed(&mut screen, &chunk[at..]);
            assert!(relay.loads.now().open, "split at {at}");
        }
        let (relay, _) = fed(b"m=1;\x1b[0mm=1;");
        assert!(!relay.loads.now().open, "only after ESC _ G");
    }

    #[test]
    fn a_long_head_is_read_to_its_end_within_reason() {
        let padded = |keys: usize| format!("\x1b_G{}m=1;AA\x1b\\", "i=1,".repeat(keys));
        assert!(open_after(padded(200).as_bytes()));
        assert!(!open_after(padded(HEAD_MAX).as_bytes()), "past the limit, what follows isn't kept");
    }

    #[test]
    fn a_chunk_finished_by_a_viewer_recovering_ends_the_load() {
        let (mut relay, mut screen) = fed(b"\x1b_Ga=t,m=1;AAAA\x1b\\\x1b_Gm=0;AAAA");
        assert!(relay.loads.now().open);
        relay.feed_to_ground(&mut screen, b"AAAA\x1b\\");
        assert!(!relay.loads.now().open);
    }

    fn feed_bytewise(relay: &mut Relay, screen: &mut Screen, input: &[u8]) -> Vec<u8> {
        input.iter().flat_map(|b| relay.feed(screen, std::slice::from_ref(b))).collect()
    }

    fn feed_either_way(bytewise: bool, relay: &mut Relay, screen: &mut Screen, input: &[u8]) -> Vec<u8> {
        if bytewise { feed_bytewise(relay, screen, input) } else { relay.feed(screen, input) }
    }

    #[test]
    fn strips_only_the_queries_the_screen_answered() {
        for split in [false, true] {
            let (mut relay, mut screen) = fed(b"");
            let input = b"hi \x1b[31mred\x1b[0m \x1b[c\x1b]11;?\x07\x1b[6n\x1bP+q544e\x1b\\\x1b]2;title\x07 end";
            let out = feed_either_way(split, &mut relay, &mut screen, input);
            // Answered here: DA1, background color, cursor position. Not answered: XTGETTCAP.
            assert_eq!(
                String::from_utf8_lossy(&out),
                "hi \x1b[31mred\x1b[0m \x1bP+q544e\x1b\\\x1b]2;title\x07 end",
                "split={split}"
            );
            let replies = String::from_utf8_lossy(&screen.take_responses()).into_owned();
            assert!(replies.starts_with("\x1b[?62;22c\x1b]11;rgb:") && replies.ends_with("R"), "{replies:?}");
            assert_eq!(screen.title(), "title");
        }
    }

    #[test]
    fn resize_reports_are_dropped_but_query_answers_are_kept() {
        let (mut relay, mut screen) = fed(b"");
        let out = relay.feed(&mut screen, b"\x1b[c\x1b[?2048h\x1b[6n");
        assert_eq!(out, b"\x1b[?2048h");
        screen.resize(100, 30);
        let replies = Relay::take_responses(&mut screen);
        assert_eq!(replies, b"\x1b[?62;22c\x1b[1;1R");
    }

    #[test]
    fn images_are_answered_by_viewers_only() {
        // Kitty graphics: the screen can't show images; a viewer that can
        // answers. Two answers (the screen's EINVAL and the viewer's OK)
        // confused programs.
        for split in [false, true] {
            let (mut relay, mut screen) = fed(b"");
            let input = b"a\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\b\x1b_Gi=7,a=T,f=100;iVBORw0KGgo=\x1b\\c\x1b[6n";
            let out = feed_either_way(split, &mut relay, &mut screen, input);
            assert_eq!(
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&input[..input.len() - 4]),
                "split={split}"
            );
            let replies = String::from_utf8_lossy(&screen.take_responses()).into_owned();
            assert!(replies.ends_with('R') && !replies.contains("_G"), "only the cursor report: {replies:?}");
        }
    }

    #[test]
    fn replies_after_an_image_wait_for_a_viewer_to_answer() {
        // Kitty's query, then a feature query viewers may leave unanswered, then DA1.
        let probe = b"\x1b[6n\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b_Ga=q,i=32,e=T,U=1\x1b\\\x1b[c";
        let mut screen = Screen::new(80, 24, 1 << 20);
        let mut relay = Relay::default();
        relay.feed(&mut screen, probe);
        assert_eq!(relay.take_replies(&mut screen, false), (b"\x1b[1;1R\x1b[?62;22c".to_vec(), None), "no viewer");
        // A viewer: the reply before the image goes, DA1 waits for its answer.
        relay.feed(&mut screen, probe);
        let (now, wait) = relay.take_replies(&mut screen, true);
        assert_eq!(now, b"\x1b[1;1R");
        let mut answers = Answers::default();
        relay.expect_answers(std::iter::once(&mut answers));
        assert_eq!(relay.answered(&answers.feed(b"\x1b_Gi=31;OK\x1b\\")), b"\x1b[?62;22c");
        assert!(relay.expire(wait.expect("a wait began")).is_empty(), "nothing left once answered");
        // No answer in time: DA1 goes when the wait ends.
        relay.feed(&mut screen, probe);
        let (now, wait) = relay.take_replies(&mut screen, true);
        assert_eq!(now, b"\x1b[1;1R");
        assert_eq!(relay.expire(wait.expect("a wait began")), b"\x1b[?62;22c");
    }

    #[test]
    fn recovery_finds_ground_between_images_even_when_every_read_ends_mid_image() {
        for opener in [b"\x1b_Gm=1;".as_slice(), b"\x1b]2;", b"\x1bPtmux;", b"\x1b^"] {
            for split_st in [false, true] {
                let (mut relay, mut screen) = fed(b"");
                let mut prefix = b"before\r\n".to_vec();
                prefix.extend_from_slice(opener);
                prefix.extend(vec![b'A'; 70_000]);
                if split_st {
                    prefix.push(ESC);
                }
                let passed = relay.feed(&mut screen, &prefix);
                assert!(screen.continuation().is_none());
                let tail = if split_st { b"\\\x1b_Gm=1;AAAA".as_slice() } else { b"AA\x1b\\\x1b_Gm=1;AAAA" };
                let (out, recovered) = relay.feed_to_ground(&mut screen, tail);
                let (at, paint) = recovered.expect("ground inside the read, not at its end");
                assert_eq!([passed, out.clone()].concat(), [prefix, tail.to_vec()].concat());
                let mut viewer = Screen::new(80, 24, 1 << 20);
                viewer.write(b"\x1b_Gm=1;partial");
                viewer.write(b"\x18");
                viewer.write(&paint);
                viewer.write(&out[at..]);
                viewer.write(&relay.feed(&mut screen, b"AAAA\x1b\\\r\nafter"));
                let text = viewer.plain_text();
                assert!(text.contains("before") && text.contains("after"), "{text:?}");
                assert!(!text.contains("AAAA"), "image data leaked: {text:?}");
                assert!(screen.take_responses().is_empty(), "no core kitty acknowledgements");
            }
        }
    }

    #[test]
    fn only_da1_waits_and_answers_must_be_complete_and_correlated() {
        let mut screen = Screen::new(80, 24, 1 << 20);
        let mut relay = Relay::default();
        let mut viewer = Answers::default();
        let mut second = Answers::default();
        let probe = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c\x1b[6n\x1b[5n";
        relay.feed(&mut screen, probe);
        relay.expect_answers([&mut viewer, &mut second].into_iter());
        let (now, wait) = relay.take_replies(&mut screen, true);
        assert_eq!(now, b"\x1b[1;1R\x1b[0n", "prompt queries must not wait");
        for part in [
            b"\x1b[200~\x1b_Gi=31;OK\x1b\\\x1b[201~".as_slice(),
            b"\x1b[200~\x1b_Garbitrary pasted text\x1b[201~",
            b"\x1b_Garbitrary\x1b\\",
            b"\x1b_Gi=31,a=q;OK\x1b\\",
            b"\x1b",
            b"_",
            b"Gi=31;OK",
            b"\x1b",
        ] {
            assert!(relay.answered(&viewer.feed(part)).is_empty());
        }
        assert_eq!(relay.answered(&viewer.feed(b"\\")), b"\x1b[?62;22c");
        assert!(relay.expire(wait.unwrap()).is_empty());

        // The second viewer's duplicate belongs to the first query, even
        // when the program reuses its image ID for the next query.
        relay.feed(&mut screen, probe);
        relay.expect_answers([&mut viewer, &mut second].into_iter());
        let (_, wait) = relay.take_replies(&mut screen, true);
        assert!(relay.answered(&second.feed(b"\x1b_Gi=31;OK\x1b\\")).is_empty());
        assert_eq!(relay.answered(&viewer.feed(b"\x1b_Gi=31;OK\x1b\\")), b"\x1b[?62;22c");
        assert!(relay.expire(wait.unwrap()).is_empty());

        // An expired query's late answer cannot release a newer ID's DA1.
        relay.feed(&mut screen, probe);
        relay.expect_answers(std::iter::once(&mut viewer));
        let (_, wait) = relay.take_replies(&mut screen, true);
        assert_eq!(relay.expire(wait.unwrap()), b"\x1b[?62;22c");
        relay.feed(&mut screen, b"\x1b_Gi=32,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
        relay.expect_answers(std::iter::once(&mut viewer));
        let (_, wait) = relay.take_replies(&mut screen, true);
        assert!(relay.answered(&viewer.feed(b"\x1b_Gi=31;OK\x1b\\")).is_empty());
        assert_eq!(relay.answered(&viewer.feed(b"\x1b_Gi=32;OK\x1b\\")), b"\x1b[?62;22c");
        assert!(relay.expire(wait.unwrap()).is_empty());
    }

    #[test]
    fn no_viewer_preserves_reply_order_even_when_prompt_queries_follow_da1() {
        let mut screen = Screen::new(80, 24, 1 << 20);
        let mut relay = Relay::default();
        relay.feed(&mut screen, b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c\x1b[6n\x1b[5n");
        assert_eq!(relay.take_replies(&mut screen, false), (b"\x1b[?62;22c\x1b[1;1R\x1b[0n".to_vec(), None));
    }

    #[test]
    fn coalesced_queries_need_each_answer_but_skipped_feature_probes_do_not() {
        let mut screen = Screen::new(80, 24, 1 << 20);
        let mut relay = Relay::default();
        let mut viewer = Answers::default();
        relay.feed(&mut screen, b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b_Gi=32,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b_Ga=q,i=33,e=T,U=1\x1b\\\x1b[c");
        relay.expect_answers(std::iter::once(&mut viewer));
        let (_, wait) = relay.take_replies(&mut screen, true);
        assert!(relay.answered(&viewer.feed(b"\x1b_Gi=31;OK\x1b\\")).is_empty());
        assert_eq!(relay.answered(&viewer.feed(b"\x1b_Gi=32;ENOENT:no image\x1b\\")), b"\x1b[?62;22c");
        assert!(relay.expire(wait.unwrap()).is_empty());
    }

    #[test]
    fn payload_free_commands_wait_regardless_of_parameter_order() {
        for command in [b"\x1b_Ga=p,i=31\x1b\\".as_slice(), b"\x1b_Gi=31,a=p\x1b\\"] {
            let mut screen = Screen::new(80, 24, 1 << 20);
            let mut relay = Relay::default();
            let mut viewer = Answers::default();
            relay.feed(&mut screen, command);
            relay.feed(&mut screen, b"\x1b[c");
            relay.expect_answers(std::iter::once(&mut viewer));
            let (now, wait) = relay.take_replies(&mut screen, true);
            assert!(now.is_empty(), "{command:?}: DA1 must wait");
            assert!(wait.is_some(), "{command:?}: placement starts a wait");
            assert_eq!(relay.answered(&viewer.feed(b"\x1b_Gi=31;ENOENT: image not found\x1b\\")), b"\x1b[?62;22c");
        }
    }

    #[test]
    fn long_strings_pass_through_untouched() {
        let (mut relay, mut screen) = fed(b"");
        let long = format!("\x1b]52;c;{}?\x07after", "A".repeat(2000));
        let out = feed_bytewise(&mut relay, &mut screen, long.as_bytes());
        assert_eq!(out, long.as_bytes());
    }

    #[test]
    fn a_recovery_repaint_brings_no_images() {
        let mut screen = Screen::new(80, 24, 1 << 20);
        screen.write(b"\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=1,q=2;AAAAAA==\x1b\\");
        let (_, recovered) = Relay::default().feed_to_ground(&mut screen, b"x");
        let (_, paint) = recovered.expect("at ground");
        assert!(!paint.windows(4).any(|w| w == b"a=t,"), "only a viewer's first paint carries them");
    }
}
