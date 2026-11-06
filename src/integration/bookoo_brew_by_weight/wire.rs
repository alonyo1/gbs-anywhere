//! Virtual scale framing and BOOKOO command confirmation. See
//! third-party/scale-bridge/NOTICE for the Apache-2.0 reference implementation.
use crate::integration::bookoo_bluetooth::protocol;
use anyhow::{bail, ensure};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub(super) const FRESH: Duration = Duration::from_secs(1);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
pub(super) struct Sample {
    pub grams: f64,
    pub timer: u32,
    pub battery: u8,
}
impl Sample {
    pub fn decode(p: &[u8]) -> anyhow::Result<Self> {
        let grams =
            protocol::grams(p).ok_or_else(|| anyhow::anyhow!("invalid BOOKOO measurement"))?;
        ensure!(p[13] <= 100, "invalid BOOKOO battery reading");
        Ok(Self {
            grams,
            timer: u32::from_be_bytes([0, p[2], p[3], p[4]]),
            battery: p[13],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Tare,
    Start,
    Stop,
    Reset,
}
impl Action {
    pub fn bytes(self) -> Vec<u8> {
        match self {
            Self::Tare => protocol::TARE.to_vec(),
            Self::Start => vec![3, 10, 4, 0, 0, 10],
            Self::Stop => vec![3, 10, 5, 0, 0, 13],
            Self::Reset => vec![3, 10, 6, 0, 0, 12],
        }
    }
    fn ack(self) -> u8 {
        match self {
            Self::Tare => 0x80,
            Self::Start => 0,
            Self::Stop => 0x20,
            Self::Reset => 0x40,
        }
    }
}

pub(super) fn packet(op: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xef, 0xdd, op];
    out.extend_from_slice(payload);
    let mut sums = [0u8; 2];
    for (i, b) in payload.iter().enumerate() {
        sums[i % 2] = sums[i % 2].wrapping_add(*b);
    }
    out.extend_from_slice(&sums);
    out
}

#[derive(Default)]
struct Framer {
    bytes: Vec<u8>,
}
impl Framer {
    fn feed(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<(u8, Vec<u8>)>> {
        ensure!(bytes.len() <= 256, "virtual scale command is too large");
        let mut frames = Vec::new();
        for b in bytes {
            self.bytes.push(*b);
            ensure!(self.bytes.len() <= 64, "virtual scale frame is too large");
            if self.bytes.len() == 1 && *b != 0xef {
                self.bytes.clear();
                continue;
            }
            if self.bytes.len() == 2 && *b != 0xdd {
                self.bytes.clear();
                if *b == 0xef {
                    self.bytes.push(*b);
                }
                continue;
            }
            if self.bytes.len() < 4 {
                continue;
            }
            let op = self.bytes[2];
            let n = match op {
                0 | 13 => 2,
                4 => 1,
                11 => 15,
                38 => 3,
                12 => usize::from(self.bytes[3]),
                _ => bail!("unsupported virtual scale opcode"),
            };
            ensure!((1..=59).contains(&n), "invalid virtual scale frame length");
            if self.bytes.len() < n + 5 {
                continue;
            }
            let p = self.bytes[3..3 + n].to_vec();
            ensure!(
                packet(op, &p) == self.bytes,
                "invalid virtual scale checksum"
            );
            self.bytes.clear();
            frames.push((op, p));
        }
        Ok(frames)
    }
}

struct Control {
    action: Action,
    operation: Action,
    sent: Option<Instant>,
    before_timer: u32,
    stopped_since: Option<(u32, Instant)>,
    awaiting_delivery: bool,
}
struct Reply {
    bytes: Vec<u8>,
    created: Instant,
    completes_control: bool,
}

pub(super) struct Core {
    pub sample: Sample,
    updated: Instant,
    decoder: Framer,
    pub paired: bool,
    streaming: bool,
    dirty: bool,
    controls: VecDeque<Control>,
    replies: VecDeque<Reply>,
    events: VecDeque<(Action, Instant)>,
}
impl Core {
    pub fn new(sample: Sample, now: Instant) -> Self {
        Self {
            sample,
            updated: now,
            decoder: Framer::default(),
            paired: false,
            streaming: false,
            dirty: false,
            controls: VecDeque::new(),
            replies: VecDeque::new(),
            events: VecDeque::new(),
        }
    }
    pub fn check(&self, now: Instant) -> anyhow::Result<()> {
        ensure!(
            now.saturating_duration_since(self.updated) <= FRESH,
            "BOOKOO readings stopped; stop the Mini manually"
        );
        if let Some(r) = self.replies.front() {
            ensure!(
                now.saturating_duration_since(r.created) <= FRESH,
                "Mini is not accepting scale replies; stop the Mini manually"
            );
        }
        if let Some(c) = self.controls.front()
            && let Some(sent) = c.sent
        {
            ensure!(
                c.awaiting_delivery || now.saturating_duration_since(sent) <= CONTROL_TIMEOUT,
                "BOOKOO did not confirm the scale command; stop the Mini manually"
            );
        }
        Ok(())
    }
    fn reply(
        &mut self,
        bytes: Vec<u8>,
        now: Instant,
        completes_control: bool,
    ) -> anyhow::Result<()> {
        ensure!(self.replies.len() < 12, "too many pending Mini replies");
        self.replies.push_back(Reply {
            bytes,
            created: now,
            completes_control,
        });
        Ok(())
    }
    fn enqueue(&mut self, action: Action, now: Instant) -> anyhow::Result<()> {
        // A retransmission must not tare or reset a physical timer again.
        if self.controls.back().is_some_and(|c| c.action == action) {
            return Ok(());
        }
        ensure!(
            self.controls.len() < 4 && self.events.len() < 16,
            "too many pending scale commands"
        );
        self.controls.push_back(Control {
            action,
            operation: if action == Action::Start {
                Action::Reset
            } else {
                action
            },
            sent: None,
            before_timer: 0,
            stopped_since: None,
            awaiting_delivery: false,
        });
        self.events.push_back((action, now));
        if action == Action::Tare {
            self.dirty = false;
        }
        Ok(())
    }
    pub fn command(&mut self, bytes: &[u8], now: Instant) -> anyhow::Result<()> {
        self.check(now)?;
        for (op, p) in self.decoder.feed(bytes)? {
            if op == 11 && p.len() == 15 {
                ensure!(
                    self.controls.is_empty(),
                    "Mini restarted scale pairing during a command"
                );
                if self.paired {
                    ensure!(self.events.len() < 16, "too many scale events");
                    self.events.push_back((Action::Reset, now));
                }
                self.paired = true;
                self.streaming = false;
                self.dirty = false;
                self.reply(
                    packet(8, &[9, self.sample.battery, 2, 16, 1, 0, 1, 1, 0]),
                    now,
                    false,
                )?;
                continue;
            }
            ensure!(self.paired, "Mini scale command arrived before handshake");
            match (op, p.as_slice()) {
                (0, [2, 0]) => {}
                (4, [0]) => self.enqueue(Action::Tare, now)?,
                (12, [9, 0, 1, 1, 2, 2, 5, 3, 4]) => {
                    self.streaming = true;
                    self.dirty = true;
                }
                (38, [0, 0, v @ 0..=2]) => self.enqueue(
                    [Action::Start, Action::Stop, Action::Reset][usize::from(*v)],
                    now,
                )?,
                (38, [k, a, b])
                    if (*k == 2 && *a == 0 && *b <= 1)
                        || (*k == 4 && *a == 0 && *b == 0)
                        || *k == 5
                        || (*k == 6 && *a == 0 && *b == 0)
                        || (*k == 7 && *a <= 3 && *b == 50) =>
                {
                    if *k == 6 {
                        self.streaming = false;
                        self.dirty = false;
                    }
                    self.reply(packet(12, &[4, 11, 0, k << 5]), now, false)?;
                }
                _ => bail!("unsupported Mini scale command; stop the Mini manually"),
            }
        }
        Ok(())
    }
    pub fn pending_controls(&self) -> bool {
        !self.controls.is_empty()
    }
    pub fn event(&mut self) -> Option<(Action, Instant)> {
        self.events.pop_front()
    }
    pub fn next_write(&mut self, now: Instant) -> anyhow::Result<Option<Action>> {
        self.check(now)?;
        let Some(c) = self.controls.front_mut() else {
            return Ok(None);
        };
        if c.sent.is_some() || c.awaiting_delivery {
            return Ok(None);
        }
        c.sent = Some(now);
        c.before_timer = self.sample.timer;
        Ok(Some(c.operation))
    }
    pub fn reading(&mut self, s: Sample, now: Instant) -> anyhow::Result<()> {
        ensure!(
            now >= self.updated,
            "BOOKOO measurement timestamps went backwards"
        );
        // A new reading cannot hide a preceding gap or expired command.
        self.check(now)?;
        self.sample = s;
        self.updated = now;
        self.dirty = true;
        let Some(c) = self.controls.front_mut() else {
            return Ok(());
        };
        let Some(sent) = c.sent else { return Ok(()) };
        if c.awaiting_delivery || now <= sent {
            return Ok(());
        }
        let confirmed = match c.operation {
            Action::Tare => s.grams.abs() <= 0.10,
            Action::Reset => s.timer == 0,
            Action::Start => {
                let delta = s.timer.wrapping_sub(c.before_timer) & 0xffffff;
                delta > 0 && delta < 0x800000
            }
            Action::Stop => match c.stopped_since {
                Some((timer, at)) if timer == s.timer => {
                    now.saturating_duration_since(at) >= Duration::from_millis(200)
                }
                _ => {
                    c.stopped_since = Some((s.timer, now));
                    false
                }
            },
        };
        if !confirmed {
            return Ok(());
        }
        if c.action == Action::Start && c.operation == Action::Reset {
            c.operation = Action::Start;
            c.sent = None;
        } else {
            c.awaiting_delivery = true;
            let ack = c.action.ack();
            self.reply(packet(12, &[4, 11, 0, ack]), now, true)?;
        }
        Ok(())
    }
    pub fn output(&self, now: Instant) -> anyhow::Result<Option<Vec<u8>>> {
        self.check(now)?;
        if let Some(r) = self.replies.front() {
            return Ok(Some(r.bytes.clone()));
        }
        if !self.streaming
            || !self.dirty
            || self
                .controls
                .iter()
                .any(|c| c.action == Action::Tare && !c.awaiting_delivery)
        {
            return Ok(None);
        }
        let raw = (self.sample.grams.abs() * 100.0).round() as u32;
        let b = raw.to_le_bytes();
        // No fabricated stability or zero flag: use only the current sample.
        Ok(Some(packet(
            12,
            &[
                8,
                5,
                b[0],
                b[1],
                b[2],
                b[3],
                2,
                if self.sample.grams < 0.0 { 2 } else { 0 },
            ],
        )))
    }
    pub fn delivered(&mut self) {
        if let Some(r) = self.replies.pop_front() {
            if r.completes_control {
                self.controls.pop_front();
            }
        } else {
            self.dirty = false;
        }
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
