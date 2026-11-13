//! A simulated Skitch: answers the same commands as the real machine and
//! walks through its states on its own (homing, the Accept and Start
//! presses, thread changes), so the sewing flow can be tried and tested
//! without one.

use super::protocol::{self, Status, cmd};
use super::session::Transport;
use crate::formats::pen::{self, COLOR_END};
use anyhow::{Result, bail};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How the simulated machine behaves.
#[derive(Clone, Copy, Debug)]
pub struct SimConfig {
    /// Stitches sewn a second.
    pub stitches_per_s: f64,
    /// Seconds the simulated person takes to do each thing on the machine
    /// (home it, press Accept or Start, change the thread).
    pub person_s: f64,
    /// Starts needing to be homed, as a machine just turned on does.
    pub needs_homing: bool,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            stitches_per_s: 60.0,
            person_s: 2.5,
            needs_homing: true,
        }
    }
}

struct State {
    cfg: SimConfig,
    status: Status,
    error: u8,
    since: Instant,
    last: Instant,
    connected: bool,
    /// The pattern: decoded records.
    records: Vec<(i32, i32, u16, u16)>,
    incoming: Vec<u8>,
    expected: usize,
    uuid: [u8; 16],
    /// Where the needle is (a record index, fractional while sewing).
    cursor: f64,
    sewn_s: f64,
    total: u32,
    /// The last layout sent: where in the hoop it was told to sew.
    layout: Option<Vec<u8>>,
    /// A made-up settings block (the real one's layout isn't known).
    settings: Vec<u8>,
    /// The last full reply (what a second read returns).
    reply: Vec<u8>,
    /// Commands to answer short, and how many more times.
    short: Vec<(u16, u32)>,
    /// Paused with the machine's own button: the simulated person doesn't
    /// press Start again by themselves.
    held: bool,
}

/// A simulated machine. Clones share it: hand one to the session as its
/// link and keep one to poke at (tests make thread breaks with it).
#[derive(Clone)]
pub struct Sim(Arc<Mutex<State>>);

impl Sim {
    pub fn new(cfg: SimConfig) -> Sim {
        let now = Instant::now();
        Sim(Arc::new(Mutex::new(State {
            cfg,
            status: if cfg.needs_homing {
                Status::Initial
            } else {
                Status::Idle
            },
            error: protocol::NO_ERROR,
            since: now,
            last: now,
            connected: true,
            records: Vec::new(),
            incoming: Vec::new(),
            expected: 0,
            uuid: [0; 16],
            cursor: 0.0,
            sewn_s: 0.0,
            total: 1_234_567,
            layout: None,
            settings: vec![
                0x00, 0x01, 0x05, 0x03, 0x00, 0x00, 0x02, 0x58, 0x01, 0x00, 0x00, 0x00,
            ],
            reply: Vec::new(),
            short: Vec::new(),
            held: false,
        })))
    }

    pub fn status(&self) -> Status {
        let mut s = self.0.lock().unwrap();
        s.tick();
        s.status
    }

    /// Where the needle is (record index).
    pub fn stitch(&self) -> usize {
        self.0.lock().unwrap().cursor as usize
    }

    /// Stop sewing with error `code`, as a thread break does.
    pub fn fail(&self, code: u8) {
        let mut s = self.0.lock().unwrap();
        s.tick();
        s.error = code;
        s.set(Status::Interrupted);
    }

    /// The Start/Stop button on the machine. While it sews, that pauses
    /// it: a Skitch then says it's ready to sew (not paused), the needle
    /// where it stopped, and waits for the button again.
    pub fn press_start_stop(&self) {
        let mut s = self.0.lock().unwrap();
        s.tick();
        match s.status {
            Status::Sewing => {
                s.held = true;
                s.set(Status::ReadyToSew);
            }
            Status::ReadyToSew => {
                s.held = false;
                s.set(Status::Sewing);
            }
            _ => {}
        }
    }

    /// The simulated person fixes the problem and clears the error.
    pub fn clear_error(&self) {
        self.0.lock().unwrap().error = protocol::NO_ERROR;
    }

    /// The move the last layout asked for (units), and its pattern center.
    pub fn layout_move(&self) -> Option<((i16, i16), (i16, i16))> {
        let s = self.0.lock().unwrap();
        let p = s.layout.as_ref()?;
        let at = |o: usize| i16::from_le_bytes([p[o], p[o + 1]]);
        Some(((at(0), at(2)), (at(20), at(22))))
    }

    /// Change one byte of the made-up settings (as Artspira would).
    pub fn set_setting(&self, k: usize, v: u8) {
        if let Some(b) = self.0.lock().unwrap().settings.get_mut(k) {
            *b = v;
        }
    }

    /// Answer `command` with just its id, the next `times` times (the
    /// full reply turns up on a second read, as a busy machine's does).
    pub fn answer_short(&self, command: u16, times: u32) {
        self.0.lock().unwrap().short.push((command, times));
    }

    /// Lose the connection (the machine turned off, or out of range).
    pub fn drop_link(&self) {
        self.0.lock().unwrap().connected = false;
    }
}

impl State {
    fn set(&mut self, st: Status) {
        self.status = st;
        self.since = Instant::now();
    }

    /// Move the simulation on to now.
    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        let waited = now.duration_since(self.since).as_secs_f64();
        let person = self.cfg.person_s;
        match self.status {
            // Hoop off, Accept, the arm homes, hoop back on.
            Status::Initial if waited >= person * 2.0 => self.set(Status::Idle),
            // The person presses Accept; the hoop goes round the outline.
            Status::WaitingForMaskTrace if waited >= person => self.set(Status::MaskTracing),
            Status::MaskTracing if waited >= person => self.set(Status::MaskTraceComplete),
            // The person presses Start.
            Status::ReadyToSew if !self.held && waited >= person => self.set(Status::Sewing),
            // The person rethreads and presses Start.
            Status::WaitingForColorChange if waited >= person * 1.5 => self.set(Status::Sewing),
            Status::Sewing => self.sew(dt),
            _ => {}
        }
    }

    fn sew(&mut self, dt: f64) {
        let n = self.records.len();
        if n == 0 {
            self.set(Status::Complete);
            return;
        }
        let from = self.cursor;
        let to = (from + dt * self.cfg.stitches_per_s).min(n as f64);
        self.sewn_s += dt;
        // A color's last stitch: stop after it for the thread change, at
        // the next color's first (where a real Skitch says it is).
        let stop = (from.floor() as usize..to.ceil() as usize)
            .find(|&k| k < n && self.records[k].2 == COLOR_END && (k as f64) >= from);
        let count = |a: f64, b: f64| (b.floor() - a.floor()).max(0.0) as u32;
        match stop {
            Some(k) if (k as f64) < to => {
                self.total += count(from, (k + 1) as f64);
                self.cursor = (k + 1) as f64;
                self.set(Status::WaitingForColorChange);
            }
            _ if to >= n as f64 => {
                self.total += count(from, n as f64);
                self.cursor = n as f64;
                self.set(Status::Complete);
            }
            _ => {
                self.total += count(from, to);
                self.cursor = to;
            }
        }
    }

    fn reply(&mut self, command: u16, payload: &[u8]) -> Vec<u8> {
        let mut r = command.to_be_bytes().to_vec();
        match command {
            cmd::MACHINE_STATE => r.extend([self.status.byte(), 0, self.error]),
            cmd::MACHINE_INFO => {
                let mut d = [0u8; 52];
                d[0..2].copy_from_slice(&100u16.to_be_bytes());
                d[2..11].copy_from_slice(b"SIM000001");
                d[16..22].copy_from_slice(&[0x5a, 0x1d, 0x00, 0x00, 0x00, 0x01]);
                d[24..26].copy_from_slice(&5u16.to_be_bytes());
                d[29..31].copy_from_slice(&1000u16.to_le_bytes());
                d[31..33].copy_from_slice(&1000u16.to_le_bytes());
                d[35] = 0;
                d[39..50].copy_from_slice(b"PP1 (sim)  ");
                r.extend(d);
            }
            cmd::SERVICE_COUNT => {
                r.extend((self.total / 10).to_le_bytes());
                r.extend(self.total.to_le_bytes());
            }
            cmd::PATTERN_SEWING_INFO => {
                let k = (self.cursor as usize).min(self.records.len().saturating_sub(1));
                let (x, y) = self.records.get(k).map(|r| (r.0, r.1)).unwrap_or((0, 0));
                r.extend((self.cursor as u16).to_le_bytes());
                r.extend((self.sewn_s as i16).to_le_bytes());
                r.extend(0i16.to_le_bytes());
                r.extend((x as i16).to_le_bytes());
                r.extend((y as i16).to_le_bytes());
            }
            cmd::EMB_SEWING_INFO_REQUEST => {
                let n = self.records.len() as u16;
                let (mut l, mut t, mut rt, mut b) = (0i16, 0i16, 0i16, 0i16);
                for &(x, y, _, _) in &self.records {
                    l = l.min(x as i16);
                    t = t.min(y as i16);
                    rt = rt.max(x as i16);
                    b = b.max(y as i16);
                }
                for v in [l, t, rt, b] {
                    r.extend(v.to_le_bytes());
                }
                // The real machine sews about 400 a minute.
                r.extend(((n as u32 * 60 / 400) as u16).to_le_bytes());
                r.extend(n.to_le_bytes());
                r.extend(400u16.to_le_bytes());
            }
            cmd::PATTERN_UUID_REQUEST => r.extend(self.uuid),
            cmd::EMB_SEWING_DATA_DELETE => {
                self.records.clear();
                self.uuid = [0; 16];
                self.cursor = 0.0;
                if self.status != Status::Initial {
                    self.set(Status::Idle);
                }
                r.push(0);
            }
            cmd::SEND_DATA_INFO => {
                self.expected = payload
                    .get(1..5)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
                    .unwrap_or(0);
                self.incoming.clear();
                r.push(0);
            }
            cmd::LAYOUT_SEND => {
                self.layout = Some(payload.to_vec());
                r.push(0);
            }
            cmd::MACHINE_SETTING_INFO => r.extend(self.settings.iter()),
            cmd::EMB_UUID_SEND => {
                let ok = self.incoming.len() == self.expected && self.expected > 0;
                if ok {
                    self.records = pen::decode(&self.incoming);
                    self.uuid = payload.try_into().unwrap_or([0; 16]);
                    self.cursor = 0.0;
                    self.sewn_s = 0.0;
                    if !matches!(self.status, Status::Initial) {
                        self.set(Status::Idle);
                    }
                }
                r.push(if ok { 0 } else { 1 });
            }
            cmd::MASK_TRACE => {
                if matches!(
                    self.status,
                    Status::Idle | Status::MaskTraceComplete | Status::ReadyToSew
                ) && !self.records.is_empty()
                {
                    self.set(Status::WaitingForMaskTrace);
                    r.push(0);
                } else {
                    r.push(1);
                }
            }
            cmd::START_SEWING => {
                let ok = matches!(
                    self.status,
                    Status::MaskTraceComplete
                        | Status::Paused
                        | Status::Interrupted
                        | Status::Complete
                ) && self.error == protocol::NO_ERROR;
                if ok {
                    if self.status == Status::Complete {
                        self.cursor = 0.0;
                        self.sewn_s = 0.0;
                    }
                    self.set(Status::ReadyToSew);
                }
                r.push(if ok { 0 } else { 1 });
            }
            cmd::NEEDLE_MODE_INSTRUCTIONS => {
                let k = payload
                    .get(0..2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .unwrap_or(0);
                self.cursor = (k as f64).min(self.records.len() as f64);
                r.push(0);
            }
            _ => r.push(0),
        }
        r
    }
}

impl Transport for Sim {
    fn request(&mut self, command: u16, payload: &[u8]) -> Result<Vec<u8>> {
        let mut s = self.0.lock().unwrap();
        if !s.connected {
            bail!("the machine went away");
        }
        s.tick();
        let r = s.reply(command, payload);
        s.reply = r.clone();
        if let Some(n) = s.short.iter_mut().find(|(c, n)| *c == command && *n > 0) {
            n.1 -= 1;
            return Ok(r[..2].to_vec());
        }
        Ok(r)
    }

    fn read(&mut self) -> Result<Vec<u8>> {
        Ok(self.0.lock().unwrap().reply.clone())
    }

    fn send(&mut self, command: u16, payload: &[u8]) -> Result<()> {
        let mut s = self.0.lock().unwrap();
        if !s.connected {
            bail!("the machine went away");
        }
        if command == cmd::SEND_DATA && payload.len() > 5 {
            // Offset, data, checksum.
            s.incoming.extend_from_slice(&payload[4..payload.len() - 1]);
        }
        Ok(())
    }

    fn connected(&mut self) -> bool {
        self.0.lock().unwrap().connected
    }

    fn close(&mut self) {}

    fn settle(&self) -> Duration {
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::protocol::Layout;
    use crate::machine::session::Session;

    fn wait_for(sim: &Sim, st: Status) {
        for _ in 0..400 {
            if sim.status() == st {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("never got to {st:?} (at {:?})", sim.status());
    }

    #[test]
    fn the_simulated_machine_goes_through_a_whole_job() {
        let sim = Sim::new(SimConfig {
            stitches_per_s: 4000.0,
            person_s: 0.02,
            needs_homing: true,
        });
        let mut s = Session::open(Box::new(sim.clone())).unwrap();
        assert_eq!(s.state().unwrap().0, Status::Initial);
        assert_eq!(s.info().unwrap().max_width, 1000);
        wait_for(&sim, Status::Idle);
        // Two colors.
        let input: Vec<_> = (0..300)
            .map(|k| pen::InStitch {
                x: (k % 50) as f64 * 10.0,
                y: (k / 50) as f64 * 10.0,
                cmd: 0,
                color: usize::from(k >= 150),
            })
            .collect();
        let data = pen::encode_stitches(&input);
        let mut seen = 0.0;
        s.upload(&data, Layout::centered(data.bounds), [7; 16], |f| seen = f)
            .unwrap();
        assert_eq!(seen, 1.0);
        assert_eq!(s.pattern_uuid().unwrap(), Some([7; 16]));
        assert_eq!(
            s.pattern_info().unwrap().total_stitches as usize,
            data.stitches
        );
        s.mask_trace().unwrap();
        assert_eq!(s.state().unwrap().0, Status::WaitingForMaskTrace);
        wait_for(&sim, Status::MaskTraceComplete);
        s.start_sewing().unwrap();
        wait_for(&sim, Status::WaitingForColorChange);
        let at = s.progress().unwrap().current_stitch as usize;
        assert_eq!(
            data.block_at(at),
            Some(1),
            "waits at the second color's first stitch"
        );
        assert_eq!(data.thread_at(at, true), Some(1), "change to the second");
        wait_for(&sim, Status::Complete);
        assert_eq!(s.progress().unwrap().current_stitch as usize, data.stitches);
    }

    #[test]
    fn short_replies_dont_stop_an_upload() {
        let sim = Sim::new(SimConfig {
            needs_homing: false,
            ..SimConfig::default()
        });
        let mut s = Session::open(Box::new(sim.clone())).unwrap();
        let input: Vec<_> = (0..40)
            .map(|k| pen::InStitch {
                x: k as f64 * 10.0,
                y: 0.0,
                cmd: 0,
                color: 0,
            })
            .collect();
        let data = pen::encode_stitches(&input);
        // The delete and layout answer with just their id; the size check
        // and the id answer late.
        sim.answer_short(cmd::EMB_SEWING_DATA_DELETE, 1);
        sim.answer_short(cmd::LAYOUT_SEND, 1);
        sim.answer_short(cmd::SEND_DATA_INFO, 1);
        sim.answer_short(cmd::EMB_UUID_SEND, 1);
        s.upload(&data, Layout::centered(data.bounds), [3; 16], |_| {})
            .unwrap();
        assert_eq!(s.pattern_uuid().unwrap(), Some([3; 16]));

        // One that never answers names the step.
        struct Mute(Sim);
        impl Transport for Mute {
            fn request(&mut self, c: u16, p: &[u8]) -> Result<Vec<u8>> {
                let r = self.0.request(c, p)?;
                Ok(if c == cmd::SEND_DATA_INFO {
                    r[..2].to_vec()
                } else {
                    r
                })
            }
            fn send(&mut self, c: u16, p: &[u8]) -> Result<()> {
                self.0.send(c, p)
            }
            fn connected(&mut self) -> bool {
                true
            }
            fn close(&mut self) {}
        }
        let mut s = Session::open(Box::new(Mute(sim))).unwrap();
        let e = s
            .upload(&data, Layout::centered(data.bounds), [4; 16], |_| {})
            .unwrap_err();
        assert!(format!("{e}").contains("SEND_DATA_INFO"), "{e}");
    }

    #[test]
    fn a_thread_break_stops_it_until_started_again() {
        let sim = Sim::new(SimConfig {
            stitches_per_s: 50.0,
            person_s: 0.01,
            needs_homing: false,
        });
        let mut s = Session::open(Box::new(sim.clone())).unwrap();
        let input: Vec<_> = (0..400)
            .map(|k| pen::InStitch {
                x: k as f64,
                y: 0.0,
                cmd: 0,
                color: 0,
            })
            .collect();
        let data = pen::encode_stitches(&input);
        s.upload(&data, Layout::centered(data.bounds), [1; 16], |_| {})
            .unwrap();
        s.mask_trace().unwrap();
        wait_for(&sim, Status::MaskTraceComplete);
        s.start_sewing().unwrap();
        wait_for(&sim, Status::Sewing);
        std::thread::sleep(Duration::from_millis(100));
        sim.fail(0x60);
        let (st, err) = s.state().unwrap();
        assert_eq!((st, err), (Status::Interrupted, 0x60));
        // Back a few stitches, fixed, started again.
        let at = s.progress().unwrap().current_stitch;
        s.set_stitch(at.saturating_sub(6)).unwrap();
        assert_eq!(s.progress().unwrap().current_stitch, at.saturating_sub(6));
        assert!(s.start_sewing().is_err() || sim.status() == Status::Interrupted);
        sim.clear_error();
        s.start_sewing().unwrap();
        wait_for(&sim, Status::Sewing);
    }

    #[test]
    fn an_unpaired_machine_is_told_apart() {
        struct Silent;
        impl Transport for Silent {
            fn request(&mut self, _: u16, _: &[u8]) -> Result<Vec<u8>> {
                Ok(Vec::new())
            }
            fn send(&mut self, _: u16, _: &[u8]) -> Result<()> {
                Ok(())
            }
            fn connected(&mut self) -> bool {
                true
            }
            fn close(&mut self) {}
        }
        let e = Session::open(Box::new(Silent)).err().unwrap();
        assert!(
            e.downcast_ref::<crate::machine::session::NotPaired>()
                .is_some()
        );
    }
}
