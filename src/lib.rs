pub use anyhow::Context;
pub use block2::RcBlock;
pub use objc2::{rc::*, runtime::*, *};
pub use objc2_app_kit::*;
pub use objc2_audio_toolbox::*;
pub use objc2_avf_audio::*;
pub use objc2_core_audio_kit::*;
pub use objc2_foundation::*;
pub use std::{
    cell::{Cell, OnceCell, RefCell},
    ffi::c_void,
    path::{Path, PathBuf},
    ptr::{NonNull, null, null_mut},
    time::Instant,
};

const FADE_SECS: f32 = 1.0;
const FADE_MIN_DB: f32 = -60.0;
const FADE_TICK_SECS: f64 = 1.0 / 60.0;
const AUDIO_EXTS: &[&str] = &["wav", "mp3", "m4a", "aac", "aif", "aiff", "caf", "flac"];
const COCOA_UI_PROPERTY: u32 = 31;

pub mod custom;
pub mod handling;
pub mod helpers;
pub mod rt;
