use super::*;
use std::sync::atomic::Ordering;

pub const MAX_FRAMES: usize = 4096;
pub static CALLS: AtomicUsize = AtomicUsize::new(0);
const FX_WORDS: usize = 8;
const MAX_TRACKS: usize = 32;

#[repr(C)]
struct Asbd {
    rate: f64,
    id: u32,
    flags: u32,
    bpp: u32,
    fpp: u32,
    bpf: u32,
    ch: u32,
    bits: u32,
    rsv: u32,
}

#[repr(C)]
struct Desc {
    t: u32,
    s: u32,
    m: u32,
    f: u32,
    mask: u32,
}

#[repr(C)]
struct Buf {
    ch: u32,
    size: u32,
    data: *mut f32,
}

#[repr(C)]
struct Abl {
    n: u32,
    b: [Buf; 2],
}

type RenderFn = extern "C" fn(*mut c_void, *mut u32, *const c_void, u32, u32, *mut Abl) -> i32;

#[repr(C)]
struct CbStruct {
    proc_: RenderFn,
    refcon: *mut c_void,
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioUnitSetProperty(
        u: *mut c_void,
        id: u32,
        scope: u32,
        el: u32,
        data: *const c_void,
        size: u32,
    ) -> i32;

    fn AudioUnitRender(
        u: *mut c_void,
        flags: *mut u32,
        ts: *const c_void,
        bus: u32,
        frames: u32,
        io: *mut Abl,
    ) -> i32;

    fn AudioUnitInitialize(u: *mut c_void) -> i32;
    fn AudioUnitUninitialize(u: *mut c_void) -> i32;
    fn AudioComponentFindNext(c: *mut c_void, d: *const Desc) -> *mut c_void;
    fn AudioComponentInstanceNew(c: *mut c_void, out: *mut *mut c_void) -> i32;
    fn AudioOutputUnitStart(u: *mut c_void) -> i32;
}

fn fmt(rate: f64) -> Asbd {
    Asbd {
        rate,
        id: 0x6C70636D,
        flags: 41,
        bpp: 4,
        fpp: 1,
        bpf: 4,
        ch: 2,
        bits: 32,
        rsv: 0,
    }
}

unsafe fn set<T>(u: *mut c_void, id: u32, scope: u32, v: &T) -> i32 {
    unsafe {
        AudioUnitSetProperty(
            u,
            id,
            scope,
            0,
            v as *const T as *const c_void,
            size_of::<T>() as u32,
        )
    }
}

pub struct Track {
    l: *mut f32,
    r: *mut f32,
    pub total: AtomicUsize,
    pub filled: AtomicUsize,
    pub cancel: AtomicBool,
    pub rate: f64,
    position: AtomicU64,
    pub key: AtomicUsize,
    fx: [AtomicU64; FX_WORDS],
    _own: (Vec<f32>, Vec<f32>),
}

unsafe impl Send for Track {}
unsafe impl Sync for Track {}

impl Track {
    pub fn new(len: usize, rate: f64) -> Arc<Track> {
        let mut l = vec![0f32; len];
        let mut r = vec![0f32; len];

        Arc::new(Track {
            l: l.as_mut_ptr(),
            r: r.as_mut_ptr(),
            total: AtomicUsize::new(len),
            filled: AtomicUsize::new(0),
            cancel: AtomicBool::new(false),
            rate,
            position: AtomicU64::new(0.0f64.to_bits()),
            key: AtomicUsize::new(usize::MAX),
            fx: std::array::from_fn(|_| AtomicU64::new(0)),
            _own: (l, r),
        })
    }

    pub unsafe fn write(&self, at: usize, l: *const f32, r: *const f32, n: usize) {
        unsafe {
            copy_nonoverlapping(l, self.l.add(at), n);
            copy_nonoverlapping(r, self.r.add(at), n);
        }

        self.filled.store(at + n, Ordering::Release);
    }

    pub fn set_fx(&self, i: usize, on: bool) {
        if i >= FX_WORDS * 64 {
            return;
        }
        let bit = 1u64 << (i % 64);
        if on {
            self.fx[i / 64].fetch_or(bit, Ordering::Relaxed);
        } else {
            self.fx[i / 64].fetch_and(!bit, Ordering::Relaxed);
        }
    }

    fn fx_mask(&self) -> [u64; FX_WORDS] {
        std::array::from_fn(|i| self.fx[i].load(Ordering::Relaxed))
    }
}

struct Inner {
    tracks: Vec<Arc<Track>>,
    units: Vec<*mut c_void>,
}

unsafe impl Send for Inner {}

pub struct Shared {
    inner: Mutex<Inner>,
    pub enabled: Vec<AtomicBool>,
    pub pos: AtomicUsize,
    pub playing: AtomicBool,
    pub gain: AtomicU32,
    pub gain_l: AtomicU32,
    pub gain_r: AtomicU32,
    fade: AtomicU32,
    cur_in: UnsafeCell<[*mut f32; 2]>,
    scratch: UnsafeCell<[Vec<f32>; 6]>,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Shared {
    fn new(n: usize) -> Shared {
        Shared {
            inner: Mutex::new(Inner {
                tracks: Vec::new(),
                units: vec![null_mut(); n],
            }),
            enabled: (0..n).map(|_| AtomicBool::new(false)).collect(),
            pos: AtomicUsize::new(0),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(1f32.to_bits()),
            gain_l: AtomicU32::new(1f32.to_bits()),
            gain_r: AtomicU32::new(1f32.to_bits()),
            fade: AtomicU32::new(1f32.to_bits()),
            cur_in: UnsafeCell::new([null_mut(); 2]),
            scratch: UnsafeCell::new(std::array::from_fn(|_| vec![0f32; MAX_FRAMES])),
        }
    }

    pub fn fade(&self) -> f32 {
        f32::from_bits(self.fade.load(Ordering::Relaxed))
    }

    pub fn set_fade(&self, v: f32) {
        self.fade.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    pub fn gain_l(&self) -> f32 {
        f32::from_bits(self.gain_l.load(Ordering::Relaxed))
    }

    pub fn gain_r(&self) -> f32 {
        f32::from_bits(self.gain_r.load(Ordering::Relaxed))
    }

    pub fn set_gain(&self, v: f32) {
        self.gain.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn set_gain_l(&self, v: f32) {
        self.gain_l.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn set_gain_r(&self, v: f32) {
        self.gain_r.store(v.to_bits(), Ordering::Relaxed);
    }
}

unsafe fn deliver(io: &mut Abl, src: [*mut f32; 2], n: usize) {
    for c in 0..(io.n as usize).min(2) {
        io.b[c].size = (n * 4) as u32;

        if io.b[c].data.is_null() {
            io.b[c].data = src[c];
        } else if io.b[c].data != src[c] {
            unsafe {
                copy_nonoverlapping(src[c], io.b[c].data, n);
            }
        }
    }
}

extern "C" fn fx_input(
    rc: *mut c_void,
    _f: *mut u32,
    _t: *const c_void,
    _b: u32,
    frames: u32,
    io: *mut Abl,
) -> i32 {
    unsafe {
        let s = &*(rc as *const Shared);
        deliver(&mut *io, *s.cur_in.get(), frames as usize);
    }

    0
}

fn mask_has(mask: &[u64; FX_WORDS], i: usize) -> bool {
    i < FX_WORDS * 64 && (mask[i / 64] >> (i % 64)) & 1 == 1
}

unsafe fn mix_track(t: &Track, out: [*mut f32; 2], n: usize) {
    unsafe {
        let total = t.total.load(Ordering::Acquire);
        let filled = t.filled.load(Ordering::Acquire).min(total);
        let mut pos = f64::from_bits(t.position.load(Ordering::Relaxed));
        let step = t.rate / 44100.0;

        for frame in 0..n {
            let index = pos as usize;
            if index >= filled {
                break;
            }
            let next = (index + 1).min(filled.saturating_sub(1));
            let frac = (pos - index as f64) as f32;

            let (l, r) = (*t.l.add(index), *t.r.add(index));
            let (ln, rn) = (*t.l.add(next), *t.r.add(next));

            *out[0].add(frame) += l + (ln - l) * frac;
            *out[1].add(frame) += r + (rn - r) * frac;
            pos += step;
        }
        t.position.store(pos.to_bits(), Ordering::Relaxed);
    }
}

extern "C" fn render(
    rc: *mut c_void,
    _f: *mut u32,
    ts: *const c_void,
    _b: u32,
    frames: u32,
    io: *mut Abl,
) -> i32 {
    CALLS.fetch_add(1, Ordering::Relaxed);

    unsafe {
        let s = &*(rc as *const Shared);
        let n = frames as usize;
        if n > MAX_FRAMES {
            return 0;
        }

        let sc = &mut *s.scratch.get();
        let mix = [sc[0].as_mut_ptr(), sc[1].as_mut_ptr()];
        let cur0 = [sc[2].as_mut_ptr(), sc[3].as_mut_ptr()];
        let oth0 = [sc[4].as_mut_ptr(), sc[5].as_mut_ptr()];

        std::ptr::write_bytes(mix[0], 0, n);
        std::ptr::write_bytes(mix[1], 0, n);

        if let Ok(mut g) = s.inner.try_lock() {
            let count = g.tracks.len().min(MAX_TRACKS);
            let mut done = [false; MAX_TRACKS];

            for i in 0..count {
                if done[i] {
                    continue;
                }
                if g.tracks[i].cancel.load(Ordering::Relaxed) {
                    done[i] = true;
                    continue;
                }

                let mask = g.tracks[i].fx_mask();
                std::ptr::write_bytes(cur0[0], 0, n);
                std::ptr::write_bytes(cur0[1], 0, n);

                for j in i..count {
                    if done[j] {
                        continue;
                    }
                    let t = &g.tracks[j];
                    if t.cancel.load(Ordering::Relaxed) {
                        done[j] = true;
                        continue;
                    }
                    if t.fx_mask() != mask {
                        continue;
                    }
                    mix_track(t, cur0, n);
                    done[j] = true;
                }

                let mut cur = cur0;
                let mut oth = oth0;

                for (ui, &u) in g.units.iter().enumerate() {
                    if u.is_null() || !mask_has(&mask, ui) {
                        continue;
                    }

                    *s.cur_in.get() = cur;
                    let sz = (n * 4) as u32;
                    let mut abl = Abl {
                        n: 2,
                        b: [
                            Buf {
                                ch: 1,
                                size: sz,
                                data: oth[0],
                            },
                            Buf {
                                ch: 1,
                                size: sz,
                                data: oth[1],
                            },
                        ],
                    };
                    let mut fl = 0u32;

                    if AudioUnitRender(u, &mut fl, ts, 0, frames, &mut abl) == 0 {
                        for c in 0..2 {
                            if abl.b[c].data != oth[c] {
                                copy_nonoverlapping(abl.b[c].data, oth[c], n);
                            }
                        }
                        std::mem::swap(&mut cur, &mut oth);
                    }
                }

                for c in 0..2 {
                    for k in 0..n {
                        *mix[c].add(k) += *cur[c].add(k);
                    }
                }
            }

            g.tracks.retain(|t| {
                if t.cancel.load(Ordering::Relaxed) {
                    return false;
                }
                let total = t.total.load(Ordering::Acquire);
                let filled = t.filled.load(Ordering::Acquire).min(total);
                let pos = f64::from_bits(t.position.load(Ordering::Relaxed));
                !(pos >= total as f64 && filled >= total)
            });

            s.playing.store(!g.tracks.is_empty(), Ordering::Relaxed);
            if let Some(t) = g.tracks.first() {
                s.pos.store(
                    f64::from_bits(t.position.load(Ordering::Relaxed)) as usize,
                    Ordering::Relaxed,
                );
            }
        }

        let gn = f32::from_bits(s.gain.load(Ordering::Relaxed))
            * f32::from_bits(s.fade.load(Ordering::Relaxed));
        let gl = gn * f32::from_bits(s.gain_l.load(Ordering::Relaxed));
        let gr = gn * f32::from_bits(s.gain_r.load(Ordering::Relaxed));
        for i in 0..n {
            *mix[0].add(i) *= gl;
            *mix[1].add(i) *= gr;
        }

        deliver(&mut *io, mix, n);
    }

    0
}

unsafe fn init_au(u: *mut c_void, rate: f64, sh: &Arc<Shared>) -> i32 {
    unsafe {
        AudioUnitUninitialize(u);

        let f = fmt(rate);

        set(u, 8, 1, &f);
        set(u, 8, 2, &f);
        set(u, 14, 0, &(MAX_FRAMES as u32));

        let cb = CbStruct {
            proc_: fx_input,
            refcon: Arc::as_ptr(sh) as *mut c_void,
        };

        set(u, 23, 1, &cb);

        AudioUnitInitialize(u)
    }
}

pub struct AudioState {
    pub sh: Arc<Shared>,
    _out: *mut c_void,
    rate: Cell<f64>,
    next_fx: RefCell<Vec<bool>>,
    next_key: Cell<usize>,
    last: RefCell<Option<Arc<Track>>>,
}

impl AudioState {
    pub fn new(n: usize) -> Result<AudioState, String> {
        let sh = Arc::new(Shared::new(n));

        unsafe {
            let d = Desc {
                t: 0x61756F75,
                s: 0x64656620,
                m: 0x6170706C,
                f: 0,
                mask: 0,
            };

            let c = AudioComponentFindNext(null_mut(), &d);

            if c.is_null() {
                return Err("No output audio unit".into());
            }

            let mut u: *mut c_void = null_mut();

            if AudioComponentInstanceNew(c, &mut u) != 0 {
                return Err("Could not create output audio unit".into());
            }

            let cb = CbStruct {
                proc_: render,
                refcon: Arc::as_ptr(&sh) as *mut c_void,
            };

            let a = set(u, 23, 1, &cb);
            let b = set(u, 8, 1, &fmt(44100.0));
            let c = set(u, 14, 0, &(MAX_FRAMES as u32));
            let d = AudioUnitInitialize(u);
            let e = AudioOutputUnitStart(u);

            println!("out unit: cb={a} fmt={b} mfps={c} init={d} start={e}");

            if d != 0 || e != 0 {
                return Err("Could not start output audio unit".into());
            }

            Ok(AudioState {
                sh,
                _out: u,
                rate: Cell::new(44100.0),
                next_fx: RefCell::new(Vec::new()),
                next_key: Cell::new(usize::MAX),
                last: RefCell::new(None),
            })
        }
    }

    pub fn add_unit(&self, i: usize, u: *mut c_void, on: bool) {
        if !self.sh.inner.lock().unwrap().units[i].is_null() {
            return;
        }

        if unsafe { init_au(u, self.rate.get(), &self.sh) } != 0 {
            return;
        }

        self.sh.enabled[i].store(on, Ordering::Relaxed);
        self.sh.inner.lock().unwrap().units[i] = u;
    }

    pub fn set_next_fx(&self, fx: &[bool]) {
        *self.next_fx.borrow_mut() = fx.to_vec();
    }

    pub fn set_next_key(&self, key: usize) {
        self.next_key.set(key);
    }

    pub fn is_playing_key(&self, key: usize) -> bool {
        self.sh
            .inner
            .lock()
            .unwrap()
            .tracks
            .iter()
            .any(|t| t.key.load(Ordering::Relaxed) == key && !t.cancel.load(Ordering::Relaxed))
    }

    pub fn set_last_fx(&self, i: usize, on: bool) {
        if let Some(t) = self.last.borrow().as_ref() {
            t.set_fx(i, on);
        }
    }

    pub fn play(&self, track: Arc<Track>, frame: usize) {
        let frame = if frame >= track.total.load(Ordering::Relaxed) {
            0
        } else {
            frame
        };
        track
            .position
            .store((frame as f64).to_bits(), Ordering::Relaxed);

        for (i, &on) in self.next_fx.borrow().iter().enumerate() {
            track.set_fx(i, on);
        }
        track.key.store(self.next_key.get(), Ordering::Relaxed);
        *self.last.borrow_mut() = Some(track.clone());

        let mut g = self.sh.inner.lock().unwrap();
        g.tracks.push(track);
        self.sh.set_fade(1.0);
        self.sh.playing.store(true, Ordering::Release);
    }

    pub fn stop(&self) {
        let mut g = self.sh.inner.lock().unwrap();

        for track in &g.tracks {
            track.cancel.store(true, Ordering::Relaxed);
        }

        g.tracks.clear();

        self.sh.playing.store(false, Ordering::Relaxed);
        self.sh.pos.store(0, Ordering::Relaxed);
        self.sh.set_fade(1.0);
    }
}
