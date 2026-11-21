use anyhow::Context;
use block2::RcBlock;
use clap::Parser;
use objc2::{rc::*, runtime::*, *};
use objc2_app_kit::*;
use objc2_audio_toolbox::*;
use objc2_avf_audio::*;
use objc2_foundation::*;
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    ptr::NonNull,
    time::Instant,
};

const FADE_SECS: f32 = 1.0;
const FADE_MIN_DB: f32 = -60.0;
const FADE_TICK_SECS: f64 = 1.0 / 60.0;
const AUDIO_EXTS: &[&str] = &["wav", "mp3", "m4a", "aac", "aif", "aiff", "caf", "flac"];

macro_rules! define_effects {
    (
        $(
            $variant:ident ($subtype:expr) {
                $(
                    $field:ident : $ty:ty = $default:expr => $param_id:expr
                ),* $(,)?
            }
        ),* $(,)?
    ) => {
        #[derive(Debug, Clone)]
        pub enum EffectCmd {
            $(
                $variant($variant),
            )*
        }

        $(
            #[derive(Debug, Clone)]
            pub struct $variant {
                $(
                    pub $field: $ty,
                )*
            }

            impl Default for $variant {
                fn default() -> Self {
                    Self {
                        $(
                            $field: $default,
                        )*
                    }
                }
            }
        )*

        impl std::str::FromStr for EffectCmd {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let (name, args) = s.split_once(':').unwrap_or((s, ""));
                match name.to_lowercase().as_str() {
                    $(
                        n if n == stringify!($variant).to_lowercase() => {
                            let mut cfg = $variant::default();
                            for pair in args.split(',').filter(|p| !p.is_empty()) {
                                let (k, v) = pair
                                    .split_once('=')
                                    .ok_or_else(|| format!("expected key=value, got '{pair}'"))?;
                                match k {
                                    $(
                                        k if k == stringify!($field) => {
                                            cfg.$field = v
                                                .parse::<$ty>()
                                                .map_err(|e| format!("invalid value for {k}: {e}"))?;
                                        }
                                    )*
                                    _ => {
                                        return Err(format!(
                                            "unknown parameter '{k}' for {}",
                                            stringify!($variant)
                                        ))
                                    }
                                }
                            }
                            Ok(EffectCmd::$variant(cfg))
                        }
                    )*
                    _ => Err(format!("unknown effect '{name}'")),
                }
            }
        }

        pub const EFFECTS_HELP: &str = concat!(
            "Add an effect to the chain. Repeat the flag to stack effects in order.\n",
            "Format: NAME or NAME:key=value,key=value\n\n",
            "Effects (defaults shown):\n",
            $(
                "  ", stringify!($variant), ": ",
                $( stringify!($field), "=", stringify!($default), " ", )*
                "\n",
            )*
        );

        impl EffectCmd {
            pub fn build_node(&self) -> Retained<AVAudioUnitEffect> {
                match self {
                    $(
                        EffectCmd::$variant(cfg) => {
                            let desc = AudioComponentDescription {
                                componentType: kAudioUnitType_Effect,
                                componentSubType: $subtype,
                                componentManufacturer: kAudioUnitManufacturer_Apple,
                                componentFlags: 0,
                                componentFlagsMask: 0,
                            };
                            unsafe {
                                let node = AVAudioUnitEffect::initWithAudioComponentDescription(
                                    AVAudioUnitEffect::alloc(),
                                    desc,
                                );
                                if let Some(tree) = node.AUAudioUnit().parameterTree() {
                                    $(
                                        if let Some(param) = tree.parameterWithAddress($param_id as u64) {
                                            param.setValue(cfg.$field as f32);
                                        }
                                    )*
                                }
                                node
                            }
                        }
                    )*
                }
            }
        }
    };
}

define_effects! {
    Lowpass(kAudioUnitSubType_LowPassFilter) {
        cutoff_hz: f32 = 8000.0 => kLowPassParam_CutoffFrequency,
        resonance: f32 = 0.0    => kLowPassParam_Resonance,
    },
    Highpass(kAudioUnitSubType_HighPassFilter) {
        cutoff_hz: f32 = 80.0 => kHipassParam_CutoffFrequency,
        resonance: f32 = 0.0  => kHipassParam_Resonance,
    },
    Bandpass(kAudioUnitSubType_BandPassFilter) {
        center_hz: f32 = 1000.0 => kBandpassParam_CenterFrequency,
        bandwidth: f32 = 1200.0 => kBandpassParam_Bandwidth,
    },
    Lowshelf(kAudioUnitSubType_LowShelfFilter) {
        cutoff_hz: f32 = 100.0 => kAULowShelfParam_CutoffFrequency,
        gain: f32 = 3.0        => kAULowShelfParam_Gain,
    },
    Highshelf(kAudioUnitSubType_HighShelfFilter) {
        cutoff_hz: f32 = 10000.0 => kHighShelfParam_CutOffFrequency,
        gain: f32 = 3.0          => kHighShelfParam_Gain,
    },
    Eq(kAudioUnitSubType_ParametricEQ) {
        center_hz: f32 = 1000.0 => kParametricEQParam_CenterFreq,
        q: f32 = 1.0            => kParametricEQParam_Q,
        gain: f32 = 3.0         => kParametricEQParam_Gain,
    },
    Eq4(kAudioUnitSubType_NBandEQ) {
        global_gain: f32 = 0.0 => kAUNBandEQParam_GlobalGain,
        b1_bypass: f32 = 0.0   => kAUNBandEQParam_BypassBand + 0,
        b1_type: f32 = 7.0     => kAUNBandEQParam_FilterType + 0,
        b1_hz: f32 = 100.0     => kAUNBandEQParam_Frequency + 0,
        b1_gain: f32 = 0.0     => kAUNBandEQParam_Gain + 0,
        b1_bw: f32 = 0.5       => kAUNBandEQParam_Bandwidth + 0,
        b2_bypass: f32 = 0.0   => kAUNBandEQParam_BypassBand + 1,
        b2_type: f32 = 0.0     => kAUNBandEQParam_FilterType + 1,
        b2_hz: f32 = 400.0     => kAUNBandEQParam_Frequency + 1,
        b2_gain: f32 = 0.0     => kAUNBandEQParam_Gain + 1,
        b2_bw: f32 = 0.5       => kAUNBandEQParam_Bandwidth + 1,
        b3_bypass: f32 = 0.0   => kAUNBandEQParam_BypassBand + 2,
        b3_type: f32 = 0.0     => kAUNBandEQParam_FilterType + 2,
        b3_hz: f32 = 2500.0    => kAUNBandEQParam_Frequency + 2,
        b3_gain: f32 = 0.0     => kAUNBandEQParam_Gain + 2,
        b3_bw: f32 = 0.5       => kAUNBandEQParam_Bandwidth + 2,
        b4_bypass: f32 = 0.0   => kAUNBandEQParam_BypassBand + 3,
        b4_type: f32 = 8.0     => kAUNBandEQParam_FilterType + 3,
        b4_hz: f32 = 10000.0   => kAUNBandEQParam_Frequency + 3,
        b4_gain: f32 = 0.0     => kAUNBandEQParam_Gain + 3,
        b4_bw: f32 = 0.5       => kAUNBandEQParam_Bandwidth + 3,
    },
    Graphiceq(kAudioUnitSubType_GraphicEQ) {
        bands: f32 = 0.0   => 10000,
        hz31: f32 = 0.0    => 0,
        hz63: f32 = 0.0    => 1,
        hz125: f32 = 0.0   => 2,
        hz250: f32 = 0.0   => 3,
        hz500: f32 = 0.0   => 4,
        hz1k: f32 = 0.0    => 5,
        hz2k: f32 = 0.0    => 6,
        hz4k: f32 = 0.0    => 7,
        hz8k: f32 = 0.0    => 8,
        hz16k: f32 = 0.0   => 9,
    },
    Compressor(kAudioUnitSubType_DynamicsProcessor) {
        threshold: f32 = -18.0         => kDynamicsProcessorParam_Threshold,
        headroom: f32 = 9.0            => kDynamicsProcessorParam_HeadRoom,
        expansion_ratio: f32 = 1.0     => kDynamicsProcessorParam_ExpansionRatio,
        expansion_threshold: f32 = 2.0 => kDynamicsProcessorParam_ExpansionThreshold,
        attack: f32 = 0.015            => kDynamicsProcessorParam_AttackTime,
        release: f32 = 0.15            => kDynamicsProcessorParam_ReleaseTime,
        gain: f32 = 3.0                => kDynamicsProcessorParam_OverallGain,
    },
    Limiter(kAudioUnitSubType_PeakLimiter) {
        attack: f32 = 0.001 => kLimiterParam_AttackTime,
        decay: f32 = 0.05   => kLimiterParam_DecayTime,
        pregain: f32 = 0.0  => kLimiterParam_PreGain,
    },
    Delay(kAudioUnitSubType_Delay) {
        mix: f32 = 25.0         => kDelayParam_WetDryMix,
        time: f32 = 0.3         => kDelayParam_DelayTime,
        feedback: f32 = 30.0    => kDelayParam_Feedback,
        cutoff_hz: f32 = 8000.0 => kDelayParam_LopassCutoff,
    },
    Reverb(kAudioUnitSubType_Reverb2) {
        mix: f32 = 25.0          => kReverb2Param_DryWetMix,
        gain: f32 = 0.0          => kReverb2Param_Gain,
        min_delay: f32 = 0.008   => kReverb2Param_MinDelayTime,
        max_delay: f32 = 0.05    => kReverb2Param_MaxDelayTime,
        decay_0hz: f32 = 2.0     => kReverb2Param_DecayTimeAt0Hz,
        decay_nyquist: f32 = 1.0 => kReverb2Param_DecayTimeAtNyquist,
        randomize: f32 = 100.0   => kReverb2Param_RandomizeReflections,
    },
    Matrixreverb(kAudioUnitSubType_MatrixReverb) {
        mix: f32 = 30.0              => kReverbParam_DryWetMix,
        small_large: f32 = 50.0      => kReverbParam_SmallLargeMix,
        small_size: f32 = 0.06       => kReverbParam_SmallSize,
        large_size: f32 = 0.5        => kReverbParam_LargeSize,
        pre_delay: f32 = 0.025       => kReverbParam_PreDelay,
        large_delay: f32 = 0.04      => kReverbParam_LargeDelay,
        small_density: f32 = 0.5     => kReverbParam_SmallDensity,
        large_density: f32 = 0.5     => kReverbParam_LargeDensity,
        large_delay_range: f32 = 0.3 => kReverbParam_LargeDelayRange,
        small_brightness: f32 = 0.5  => kReverbParam_SmallBrightness,
        large_brightness: f32 = 0.5  => kReverbParam_LargeBrightness,
        small_delay_range: f32 = 0.3 => kReverbParam_SmallDelayRange,
        mod_rate: f32 = 1.0          => kReverbParam_ModulationRate,
        mod_depth: f32 = 0.2         => kReverbParam_ModulationDepth,
    },
    Distortion(kAudioUnitSubType_Distortion) {
        delay: f32 = 0.1            => kDistortionParam_Delay,
        decay_rate: f32 = 1.0       => kDistortionParam_Decay,
        delay_mix: f32 = 0.0        => kDistortionParam_DelayMix,
        decimation: f32 = 0.0       => kDistortionParam_Decimation,
        rounding: f32 = 0.0         => kDistortionParam_Rounding,
        decimation_mix: f32 = 0.0   => kDistortionParam_DecimationMix,
        linear: f32 = 1.0           => kDistortionParam_LinearTerm,
        squared: f32 = 0.3          => kDistortionParam_SquaredTerm,
        cubic: f32 = 0.0            => kDistortionParam_CubicTerm,
        polynomial_mix: f32 = 100.0 => kDistortionParam_PolynomialMix,
        ring_freq1: f32 = 100.0     => kDistortionParam_RingModFreq1,
        ring_freq2: f32 = 100.0     => kDistortionParam_RingModFreq2,
        ring_balance: f32 = 50.0    => kDistortionParam_RingModBalance,
        ring_mix: f32 = 0.0         => kDistortionParam_RingModMix,
        soft_clip_gain: f32 = 0.0   => kDistortionParam_SoftClipGain,
        final_mix: f32 = 100.0      => kDistortionParam_FinalMix,
    },
}

#[derive(Parser)]
#[command(about = "Play an audio file using AVAudioEngine")]
struct Cli {
    file: Option<PathBuf>,

    #[arg(
        short,
        long,
        value_name = "NAME[:KEY=VAL,...]",
        help = "Effect to apply (see --help for the list)",
        long_help = EFFECTS_HELP
    )]
    effect: Vec<EffectCmd>,
}

fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.iter().any(|a| a.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

fn fade_gain(start_gain: f32, min_db: f32, progress: f32) -> f32 {
    if progress >= 1.0 || start_gain <= 0.0 {
        return 0.0;
    }
    let start_db = (20.0 * start_gain.log10()).max(min_db);
    let db = start_db + (min_db - start_db) * progress;
    10.0_f32.powf(db / 20.0)
}

struct AudioState {
    engine: Retained<AVAudioEngine>,
    player: Retained<AVAudioPlayerNode>,
    #[allow(dead_code)]
    nodes: Vec<Retained<AVAudioUnitEffect>>,
}

struct AppIvars {
    effects: Vec<EffectCmd>,
    field: Retained<NSTextField>,
    error_label: Retained<NSTextField>,
    audio: RefCell<Option<AudioState>>,
    fade: RefCell<Option<Retained<NSTimer>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppIvars]
    #[name = "AppHandler"]
    struct AppHandler;

    unsafe impl NSObjectProtocol for AppHandler {}

    unsafe impl NSApplicationDelegate for AppHandler {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn application_should_terminate_after_last_window_closed(
            &self,
            _sender: &NSApplication,
        ) -> bool {
            self.finish_stop();
            true
        }
    }

    impl AppHandler {
        #[unsafe(method(onPlay:))]
        fn on_play(&self, _sender: Option<&AnyObject>) {
            self.finish_stop();

            let text = self.ivars().field.stringValue().to_string();
            let path = PathBuf::from(text.trim());
            if !is_audio_path(&path) {
                self.set_error(format!("Not a supported audio file: {}", path.display()));
                return;
            }

            match start_audio(&path, &self.ivars().effects) {
                Ok(state) => {
                    *self.ivars().audio.borrow_mut() = Some(state);
                    self.set_error(String::new());
                }
                Err(e) => self.set_error(e.to_string()),
            }
        }

        #[unsafe(method(onStop:))]
        fn on_stop(&self, _sender: Option<&AnyObject>) {
            self.begin_fade_stop();
        }

        #[unsafe(method(onBackgroundClick:))]
        fn on_background_click(&self, sender: Option<&AnyObject>) {
            let field = &self.ivars().field;
            if let Some(gesture) = sender.and_then(|s| s.downcast_ref::<NSGestureRecognizer>()) {
                let p = gesture.locationInView(Some(field));
                let b = field.bounds();
                if p.x >= 0.0 && p.y >= 0.0 && p.x <= b.size.width && p.y <= b.size.height {
                    return;
                }
            }
            if let Some(window) = field.window() {
                window.makeFirstResponder(None);
            }
        }

        #[unsafe(method(onBrowse:))]
        fn on_browse(&self, _sender: Option<&AnyObject>) {
            let field = &self.ivars().field;
            let current = field.stringValue().to_string();

            let mut dialog = rfd::FileDialog::new()
                .set_title("Choose an audio file")
                .add_filter("Audio", AUDIO_EXTS);
            if let Some(dir) = Path::new(current.trim())
                .parent()
                .filter(|d| d.is_dir())
            {
                dialog = dialog.set_directory(dir);
            }

            if let Some(path) = dialog.pick_file() {
                field.setStringValue(&NSString::from_str(&path.to_string_lossy()));
            }
        }
    }
);

impl AppHandler {
    fn new(
        mtm: MainThreadMarker,
        effects: Vec<EffectCmd>,
        field: Retained<NSTextField>,
        error_label: Retained<NSTextField>,
    ) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(AppIvars {
            effects,
            field,
            error_label,
            audio: RefCell::new(None),
            fade: RefCell::new(None),
        });

        unsafe { msg_send![super(this), init] }
    }

    fn set_error(&self, message: String) {
        let label = &self.ivars().error_label;
        label.setStringValue(&NSString::from_str(&message));
        label.setAlphaValue(if message.is_empty() { 0.0 } else { 1.0 });
    }

    fn finish_stop(&self) {
        if let Some(timer) = self.ivars().fade.borrow_mut().take() {
            timer.invalidate();
        }
        if let Some(state) = self.ivars().audio.borrow_mut().take() {
            unsafe {
                state.player.stop();
                state.engine.stop();
            }
        }
    }

    fn begin_fade_stop(&self) {
        if self.ivars().fade.borrow().is_some() {
            return;
        }
        let Some(player) = self
            .ivars()
            .audio
            .borrow()
            .as_ref()
            .map(|s| s.player.clone())
        else {
            return;
        };

        let this = self.retain();
        let timer = fade_out(player, move || this.finish_stop());
        *self.ivars().fade.borrow_mut() = Some(timer);
    }
}

fn fade_out<F>(player: Retained<AVAudioPlayerNode>, on_complete: F) -> Retained<NSTimer>
where
    F: FnOnce() + 'static,
{
    let start_gain = unsafe { player.volume() };
    let start = Instant::now();
    let on_complete = RefCell::new(Some(on_complete));

    let block = RcBlock::new(move |timer: NonNull<NSTimer>| {
        let progress = start.elapsed().as_secs_f32() / FADE_SECS;
        if progress >= 1.0 {
            unsafe {
                player.setVolume(0.0);
                timer.as_ref().invalidate();
            }
            if let Some(callback) = on_complete.borrow_mut().take() {
                callback();
            }
        } else {
            unsafe { player.setVolume(fade_gain(start_gain, FADE_MIN_DB, progress)) };
        }
    });

    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(FADE_TICK_SECS, true, &block) }
}

fn start_audio(file_path: &Path, effects: &[EffectCmd]) -> anyhow::Result<AudioState> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(
        std::fs::canonicalize(file_path)?
            .to_str()
            .context("invalid utf-8 path")?,
    ));

    unsafe {
        let file = AVAudioFile::initForReading_error(AVAudioFile::alloc(), &url)?;
        let fmt = file.processingFormat();
        let engine = AVAudioEngine::new();
        let player = AVAudioPlayerNode::new();
        let mixer = engine.mainMixerNode();

        let nodes: Vec<_> = effects.iter().map(|e| e.build_node()).collect();

        engine.attachNode(&player);
        for node in &nodes {
            engine.attachNode(node);
        }

        let mut prev: &AVAudioNode = &player;
        for node in &nodes {
            engine.connect_to_format(prev, node, Some(&fmt));
            prev = node;
        }
        engine.connect_to_format(prev, &mixer, Some(&fmt));

        player.scheduleFile_atTime_completionHandler(&file, None, std::ptr::null_mut());
        engine.startAndReturnError()?;
        player.play();

        Ok(AudioState {
            engine,
            player,
            nodes,
        })
    }
}

fn make_button(
    title: &NSString,
    target: &AppHandler,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    unsafe { NSButton::buttonWithTitle_target_action(title, Some(target), Some(action), mtm) }
}

fn make_stack(
    views: &[Retained<NSView>],
    distribution: NSStackViewDistribution,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(views), mtm);
    stack.setDistribution(distribution);
    stack.setSpacing(10.0);
    stack
}

fn make_error_label(mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::textFieldWithString(&NSString::new(), mtm);
    label.setEditable(false);
    label.setSelectable(false);
    label.setBezeled(false);
    label.setDrawsBackground(false);
    label.setTextColor(Some(&NSColor::systemRedColor()));
    label.setFont(Some(&NSFont::systemFontOfSize(10.0)));
    label.setUsesSingleLineMode(false);
    label.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    label.setMaximumNumberOfLines(3);
    label.setAlphaValue(0.0);
    label
}

fn main() {
    let cli = Cli::parse();

    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);

    let initial = cli
        .file
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let field = NSTextField::textFieldWithString(&NSString::from_str(&initial), mtm);
    field.setPlaceholderString(Some(ns_string!("Path to an audio file")));
    if field.respondsToSelector(sel!(setAllowsWritingTools:)) {
        field.setAllowsWritingTools(false);
    }
    if field.respondsToSelector(sel!(setAllowsWritingToolsAffordance:)) {
        field.setAllowsWritingToolsAffordance(false);
    }

    let error_label = make_error_label(mtm);
    let handler = AppHandler::new(mtm, cli.effect, field.clone(), error_label.clone());
    app.setDelegate(Some(ProtocolObject::from_ref(&*handler)));

    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(480.0, 160.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    window.setTitle(ns_string!("kq"));
    window.setContentMinSize(NSSize::new(360.0, 120.0));

    let browse_btn = make_button(ns_string!("Browse…"), &handler, sel!(onBrowse:), mtm);
    let play_btn = make_button(ns_string!("Play"), &handler, sel!(onPlay:), mtm);
    let stop_btn = make_button(ns_string!("Stop"), &handler, sel!(onStop:), mtm);
    play_btn.setKeyEquivalent(ns_string!(" "));
    stop_btn.setKeyEquivalent(ns_string!("\u{1b}"));

    let file_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(field)),
            Retained::into_super(Retained::into_super(browse_btn)),
        ],
        NSStackViewDistribution::Fill,
        mtm,
    );
    let button_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(play_btn)),
            Retained::into_super(Retained::into_super(stop_btn)),
        ],
        NSStackViewDistribution::FillEqually,
        mtm,
    );
    let root = make_stack(
        &[
            Retained::into_super(file_row.clone()),
            Retained::into_super(button_row.clone()),
            Retained::into_super(Retained::into_super(error_label.clone())),
        ],
        NSStackViewDistribution::Fill,
        mtm,
    );
    root.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    root.setSpacing(14.0);
    root.setTranslatesAutoresizingMaskIntoConstraints(false);

    let content = window.contentView().unwrap();
    content.addSubview(&root);

    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        root.centerXAnchor()
            .constraintEqualToAnchor(&content.centerXAnchor()),
        root.centerYAnchor()
            .constraintEqualToAnchor(&content.centerYAnchor()),
        root.widthAnchor()
            .constraintEqualToAnchor_multiplier(&content.widthAnchor(), 0.85),
        file_row
            .widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        button_row
            .widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        error_label
            .widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        error_label.heightAnchor().constraintEqualToConstant(36.0),
    ]));

    let click = unsafe {
        NSClickGestureRecognizer::initWithTarget_action(
            mtm.alloc::<NSClickGestureRecognizer>(),
            Some(&handler),
            Some(sel!(onBackgroundClick:)),
        )
    };
    content.addGestureRecognizer(&click);

    window.center();
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(None);

    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
}
