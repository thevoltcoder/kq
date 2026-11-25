use std::{
    cell::{Cell, UnsafeCell},
    ffi::c_void,
    ptr::{copy_nonoverlapping, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{
            AtomicBool, AtomicU32, AtomicUsize,
            Ordering::{Acquire, Relaxed, Release},
        },
    },
};

pub const MAX_FRAMES: usize = 4096;
pub static CALLS: AtomicUsize = AtomicUsize::new(0);

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
    fn AudioOutputUnitStop(u: *mut c_void) -> i32;
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
            _own: (l, r),
        })
    }

    pub unsafe fn write(&self, at: usize, l: *const f32, r: *const f32, n: usize) {
        unsafe {
            copy_nonoverlapping(l, self.l.add(at), n);
            copy_nonoverlapping(r, self.r.add(at), n);
        }
        self.filled.store(at + n, Release);
    }
}

struct Inner {
    track: Option<Arc<Track>>,
    units: Vec<*mut c_void>,
}
unsafe impl Send for Inner {}

pub struct Shared {
    inner: Mutex<Inner>,
    pub enabled: Vec<AtomicBool>,
    pub pos: AtomicUsize,
    pub playing: AtomicBool,
    pub gain: AtomicU32,
    fade: AtomicU32,
    cur_in: UnsafeCell<[*mut f32; 2]>,
    scratch: UnsafeCell<[Vec<f32>; 4]>,
}
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Shared {
    fn new(n: usize) -> Shared {
        Shared {
            inner: Mutex::new(Inner {
                track: None,
                units: vec![null_mut(); n],
            }),
            enabled: (0..n).map(|_| AtomicBool::new(false)).collect(),
            pos: AtomicUsize::new(0),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(1f32.to_bits()),
            fade: AtomicU32::new(1f32.to_bits()),
            cur_in: UnsafeCell::new([null_mut(); 2]),
            scratch: UnsafeCell::new(std::array::from_fn(|_| vec![0f32; MAX_FRAMES])),
        }
    }

    pub fn set_fade(&self, v: f32) {
        self.fade.store(v.to_bits(), Relaxed);
    }
}

unsafe fn deliver(io: &mut Abl, src: [*mut f32; 2], n: usize) {
    for c in 0..(io.n as usize).min(2) {
        io.b[c].size = (n * 4) as u32;
        if io.b[c].data.is_null() {
            io.b[c].data = src[c];
        } else if io.b[c].data != src[c] {
            unsafe { copy_nonoverlapping(src[c], io.b[c].data, n) };
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

extern "C" fn render(
    rc: *mut c_void,
    _f: *mut u32,
    ts: *const c_void,
    _b: u32,
    frames: u32,
    io: *mut Abl,
) -> i32 {
    CALLS.fetch_add(1, Relaxed);
    unsafe {
        let s = &*(rc as *const Shared);
        let n = frames as usize;
        if n > MAX_FRAMES {
            return 0;
        }
        let sc = &mut *s.scratch.get();
        let mut cur = [sc[0].as_mut_ptr(), sc[1].as_mut_ptr()];
        let mut oth = [sc[2].as_mut_ptr(), sc[3].as_mut_ptr()];
        std::ptr::write_bytes(cur[0], 0, n);
        std::ptr::write_bytes(cur[1], 0, n);

        if let Ok(g) = s.inner.try_lock() {
            if s.playing.load(Relaxed) {
                if let Some(t) = g.track.as_ref() {
                    let pos = s.pos.load(Relaxed);
                    let total = t.total.load(Acquire);
                    let avail = t.filled.load(Acquire).min(total).saturating_sub(pos).min(n);
                    if avail > 0 {
                        copy_nonoverlapping(t.l.add(pos), cur[0], avail);
                        copy_nonoverlapping(t.r.add(pos), cur[1], avail);
                        s.pos.store(pos + avail, Relaxed);
                    }
                    if pos + avail >= total {
                        s.playing.store(false, Relaxed);
                    }
                    for (i, &u) in g.units.iter().enumerate() {
                        if u.is_null() || !s.enabled[i].load(Relaxed) {
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
                }
            }
        }

        let gn = f32::from_bits(s.gain.load(Relaxed)) * f32::from_bits(s.fade.load(Relaxed));
        for c in 0..2 {
            for i in 0..n {
                *cur[c].add(i) *= gn;
            }
        }
        deliver(&mut *io, cur, n);
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
    out: *mut c_void,
    rate: Cell<f64>,
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
                out: u,
                rate: Cell::new(44100.0),
            })
        }
    }

    fn set_rate(&self, rate: f64) {
        if self.rate.get() == rate {
            return;
        }
        let g = self.sh.inner.lock().unwrap();
        unsafe {
            AudioOutputUnitStop(self.out);
            AudioUnitUninitialize(self.out);
            set(self.out, 8, 1, &fmt(rate));
            AudioUnitInitialize(self.out);
            for &u in &g.units {
                if !u.is_null() {
                    init_au(u, rate, &self.sh);
                }
            }
            AudioOutputUnitStart(self.out);
        }
        self.rate.set(rate);
    }

    pub fn add_unit(&self, i: usize, u: *mut c_void, on: bool) {
        if !self.sh.inner.lock().unwrap().units[i].is_null() {
            return;
        }
        if unsafe { init_au(u, self.rate.get(), &self.sh) } != 0 {
            return;
        }
        self.sh.enabled[i].store(on, Relaxed);
        self.sh.inner.lock().unwrap().units[i] = u;
    }

    pub fn play(&self, track: Arc<Track>, frame: usize) {
        self.set_rate(track.rate);
        let frame = if frame >= track.total.load(Relaxed) {
            0
        } else {
            frame
        };
        let mut g = self.sh.inner.lock().unwrap();
        if let Some(old) = g.track.replace(track) {
            old.cancel.store(true, Relaxed);
        }
        self.sh.pos.store(frame, Relaxed);
        self.sh.set_fade(1.0);
        self.sh.playing.store(true, Release);
    }

    pub fn stop(&self) {
        self.sh.playing.store(false, Relaxed);
        self.sh.set_fade(1.0);
    }
}
