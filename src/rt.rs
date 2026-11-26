use std::{
    cell::{Cell, UnsafeCell},
    ffi::c_void,
    ptr::{copy_nonoverlapping, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{
            AtomicBool, AtomicU32, AtomicU64, AtomicUsize,
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
                tracks: Vec::new(),
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

        let mut has_tracks = false;

        if let Ok(mut g) = s.inner.try_lock() {
            let mut i = 0;

            while i < g.tracks.len() {
                let t = &g.tracks[i];

                if t.cancel.load(Relaxed) {
                    g.tracks.remove(i);
                    continue;
                }

                let total = t.total.load(Acquire);
                let filled = t.filled.load(Acquire).min(total);

                let mut pos = f64::from_bits(t.position.load(Relaxed));
                let step = t.rate / 44100.0;

                let mut produced = false;

                for frame in 0..n {
                    let index = pos as usize;

                    if index >= filled {
                        break;
                    }

                    let next = (index + 1).min(filled.saturating_sub(1));
                    let frac = (pos - index as f64) as f32;

                    let left = *t.l.add(index);
                    let right = *t.r.add(index);

                    let left_next = *t.l.add(next);
                    let right_next = *t.r.add(next);

                    *cur[0].add(frame) += left + (left_next - left) * frac;
                    *cur[1].add(frame) += right + (right_next - right) * frac;

                    pos += step;
                    produced = true;
                }

                t.position.store(pos.to_bits(), Relaxed);

                if produced {
                    has_tracks = true;
                }

                let finished = pos >= total as f64 && filled >= total;

                if finished {
                    g.tracks.remove(i);
                } else {
                    i += 1;
                }
            }

            s.playing.store(!g.tracks.is_empty(), Relaxed);

            if let Some(t) = g.tracks.first() {
                s.pos
                    .store(f64::from_bits(t.position.load(Relaxed)) as usize, Relaxed);
            }

            if has_tracks {
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
    _out: *mut c_void,
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
                _out: u,
                rate: Cell::new(44100.0),
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

        self.sh.enabled[i].store(on, Relaxed);
        self.sh.inner.lock().unwrap().units[i] = u;
    }

    pub fn play(&self, track: Arc<Track>, frame: usize) {
        let frame = if frame >= track.total.load(Relaxed) {
            0
        } else {
            frame
        };

        track.position.store((frame as f64).to_bits(), Relaxed);

        let mut g = self.sh.inner.lock().unwrap();

        g.tracks.push(track);

        self.sh.set_fade(1.0);
        self.sh.playing.store(true, Release);
    }

    pub fn stop(&self) {
        let mut g = self.sh.inner.lock().unwrap();

        for track in &g.tracks {
            track.cancel.store(true, Relaxed);
        }

        g.tracks.clear();

        self.sh.playing.store(false, Relaxed);
        self.sh.pos.store(0, Relaxed);
        self.sh.set_fade(1.0);
    }
}
