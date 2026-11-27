use super::*;

pub fn raw_audio_unit(node: &AVAudioUnit) -> *mut handling::RawAudioUnit {
    unsafe { msg_send![node, audioUnit] }
}

pub fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.iter().any(|a| a.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

pub fn list_installed_effects() -> Vec<handling::Installed> {
    unsafe {
        let manager = AVAudioUnitComponentManager::sharedAudioUnitComponentManager();
        let mut list: Vec<handling::Installed> = Vec::new();
        for ty in [kAudioUnitType_Effect, kAudioUnitType_MusicEffect] {
            let wildcard = AudioComponentDescription {
                componentType: ty,
                componentSubType: 0,
                componentManufacturer: 0,
                componentFlags: 0,
                componentFlagsMask: 0,
            };
            list.extend(
                manager
                    .componentsMatchingDescription(wildcard)
                    .iter()
                    .map(|c| handling::Installed {
                        name: format!("{} - {}", c.manufacturerName(), c.name()),
                        desc: c.audioComponentDescription(),
                    }),
            );
        }
        list.sort_by_key(|i| i.name.to_lowercase());
        list
    }
}

pub fn fade_gain(start_gain: f32, min_db: f32, progress: f32) -> f32 {
    if progress >= 1.0 || start_gain <= 0.0 {
        return 0.0;
    }
    let start_db = (20.0 * start_gain.log10()).max(min_db);
    let db = start_db + (min_db - start_db) * progress;
    10.0_f32.powf(db / 20.0)
}

pub fn cocoa_view(
    unit: *mut handling::RawAudioUnit,
) -> Option<(Retained<NSView>, Retained<AnyObject>)> {
    unsafe {
        let au = unit as AudioUnit;
        let mut size: u32 = 0;
        let mut writable: u8 = 0;
        let status = AudioUnitGetPropertyInfo(
            au,
            COCOA_UI_PROPERTY,
            kAudioUnitScope_Global,
            0,
            &mut size,
            &mut writable,
        );
        if status != 0 || (size as usize) < 2 * std::mem::size_of::<usize>() {
            return None;
        }

        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        let mut got = size;
        let status = AudioUnitGetProperty(
            au,
            COCOA_UI_PROPERTY,
            kAudioUnitScope_Global,
            0,
            NonNull::from(&mut buffer[0]).cast::<std::ffi::c_void>(),
            NonNull::from(&mut got),
        );
        if status != 0 {
            return None;
        }

        let url = Retained::from_raw(buffer[0] as *mut NSURL)?;
        let class_name = Retained::from_raw(buffer[1] as *mut NSString)?;

        let bundle = NSBundle::bundleWithURL(&url)?;
        bundle.load();
        let class = NSClassFromString(&class_name)?;

        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        let factory: Retained<AnyObject> = msg_send![allocated, init];
        let view: Option<Retained<NSView>> =
            msg_send![&*factory, uiViewForAudioUnit: au, withSize: NSSize::new(0.0, 0.0)];
        view.map(|v| (v, factory))
    }
}

pub fn editor_window(mtm: MainThreadMarker, view: &NSView, title: &str) -> Retained<NSWindow> {
    let mut size = view.frame().size;
    if size.width < 1.0 || size.height < 1.0 {
        size = NSSize::new(480.0, 320.0);
    }
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            NSRect::new(NSPoint::new(0.0, 0.0), size),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    window.setContentView(Some(view));
    window.center();
    window.makeKeyAndOrderFront(None);
    window
}

pub fn fade_out<F>(
    sh: std::sync::Arc<rt::Shared>,
    duration: f64,
    on_complete: F,
) -> Retained<NSTimer>
where
    F: FnOnce() + 'static,
{
    let start = Instant::now();
    let start_fade = sh.fade();
    let on_complete = RefCell::new(Some(on_complete));

    let block = RcBlock::new(move |timer: NonNull<NSTimer>| {
        let progress = (start.elapsed().as_secs_f64() / duration.max(0.001)).min(1.0) as f32;

        sh.set_fade(start_fade * (1.0 - progress));

        if progress >= 1.0 {
            sh.set_fade(0.0);
            unsafe { timer.as_ref().invalidate() };

            if let Some(cb) = on_complete.borrow_mut().take() {
                cb();
            }
        }
    });

    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(FADE_TICK_SECS, true, &block) }
}

pub fn current_frame(state: &handling::AudioState) -> i64 {
    state.sh.pos.load(std::sync::atomic::Ordering::Relaxed) as i64
}

pub fn insert_effect(
    state: &handling::AudioState,
    index: usize,
    effect: &Retained<AVAudioUnitEffect>,
    on: bool,
) {
    state.add_unit(index, raw_audio_unit(effect) as *mut std::ffi::c_void, on);
}

pub fn set_effect_enabled(state: &handling::AudioState, index: usize, enabled: bool) {
    state.sh.enabled[index].store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn play_audio(state: &handling::AudioState, path: &Path, frame: i64) -> anyhow::Result<()> {
    let track = load_track(path)?;
    state.play(track, frame.max(0) as usize);
    Ok(())
}

fn load_track(path: &Path) -> anyhow::Result<std::sync::Arc<rt::Track>> {
    use std::sync::atomic::Ordering::Relaxed;
    struct S(Retained<AVAudioFile>);
    unsafe impl Send for S {}

    let file = open_audio_file(path)?;
    let (len, rate, ch) = unsafe {
        let f = file.processingFormat();
        (
            file.length() as usize,
            f.sampleRate(),
            f.channelCount() as usize,
        )
    };
    let track = rt::Track::new(len, rate);
    let t = track.clone();
    let s = S(file);

    std::thread::spawn(move || unsafe {
        let s = s;
        let fmt = s.0.processingFormat();
        let cls = AnyClass::get(c"AVAudioPCMBuffer").unwrap();
        let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
        let buf: Retained<AnyObject> =
            msg_send![alloc, initWithPCMFormat: &*fmt, frameCapacity: 8192u32];
        let mut done = 0usize;
        while done < len && !t.cancel.load(Relaxed) {
            let n = (len - done).min(8192) as u32;
            let mut err: *mut NSError = std::ptr::null_mut();
            let ok: objc2::runtime::Bool =
                msg_send![&*s.0, readIntoBuffer: &*buf, frameCount: n, error: &mut err];
            if !ok.as_bool() {
                println!("decode failed at {done}: {err:?}");
                break;
            }
            let got: u32 = msg_send![&*buf, frameLength];
            if got == 0 {
                break;
            }
            let data: *const *const f32 = msg_send![&*buf, floatChannelData];
            let l = *data;
            let r = if ch > 1 { *data.add(1) } else { l };
            t.write(done, l, r, got as usize);
            done += got as usize;
        }
        t.total.store(done, std::sync::atomic::Ordering::Release);
        println!(
            "decoded {done}/{len}, render calls={}",
            rt::CALLS.load(Relaxed)
        );
    });
    Ok(track)
}

pub fn instantiate_builtin_eq() -> Retained<AVAudioUnitEffect> {
    unsafe {
        Retained::into_super(AVAudioUnitEQ::initWithNumberOfBands(
            AVAudioUnitEQ::alloc(),
            4,
        ))
    }
}

pub fn instantiate_effect_async(
    desc: AudioComponentDescription,
    index: usize,
    tx: std::sync::mpsc::Sender<(usize, Result<usize, String>)>,
) {
    let block = RcBlock::new(move |unit: *mut AVAudioUnit, error: *mut NSError| {
        let result = if !unit.is_null() {
            match unsafe { Retained::<AVAudioUnit>::retain(unit) } {
                Some(unit) => Ok(Retained::into_raw(unit) as usize),
                None => Err("Failed to retain instantiated Audio Unit".to_string()),
            }
        } else {
            let message = if error.is_null() {
                "Audio Unit instantiation failed without an NSError".to_string()
            } else {
                unsafe { (&*error).localizedDescription().to_string() }
            };

            Err(message)
        };

        let _ = tx.send((index, result));
    });

    unsafe {
        AVAudioUnit::instantiateWithComponentDescription_options_completionHandler(
            desc,
            AudioComponentInstantiationOptions::empty(),
            &block,
        );
    }
}

pub fn open_audio_file(file_path: &Path) -> anyhow::Result<Retained<AVAudioFile>> {
    let url = NSURL::from_file_path(file_path).context("invalid audio file path")?;
    let allocated = AVAudioFile::alloc();
    let mut error: *mut NSError = std::ptr::null_mut();
    let file: Option<Retained<AVAudioFile>> =
        unsafe { msg_send![allocated, initForReading: &*url, error: &mut error] };
    println!(
        "{:#?}",
        file.clone().map(|f| unsafe { f.processingFormat() })
    );
    file.ok_or_else(|| anyhow::anyhow!("Failed to open audio file: {:?}", error))
}

pub fn set_buffer_frames(want: u32) -> Result<u32, String> {
    use coreaudio_sys::{
        AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
        AudioObjectSetPropertyData, AudioValueRange, kAudioDevicePropertyBufferFrameSize,
        kAudioDevicePropertyBufferFrameSizeRange, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyElementMaster, kAudioObjectPropertyScopeGlobal,
        kAudioObjectSystemObject,
    };
    unsafe {
        let mut dev: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMaster,
        };
        let s = AudioObjectGetPropertyData(
            kAudioObjectSystemObject,
            &addr,
            0,
            null(),
            &mut size,
            &mut dev as *mut _ as *mut c_void,
        );
        if s != 0 {
            return Err(format!("get default device: {s}"));
        }

        let range_addr = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyBufferFrameSizeRange,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMaster,
        };
        let mut range = AudioValueRange {
            mMinimum: 0.0,
            mMaximum: 0.0,
        };
        let mut rsize = std::mem::size_of::<AudioValueRange>() as u32;
        let s = AudioObjectGetPropertyData(
            dev,
            &range_addr,
            0,
            null(),
            &mut rsize,
            &mut range as *mut _ as *mut c_void,
        );
        if s != 0 {
            return Err(format!("get range: {s}"));
        }
        let frames = (want as f64).clamp(range.mMinimum, range.mMaximum) as u32;

        let size_addr = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyBufferFrameSize,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMaster,
        };
        let s = AudioObjectSetPropertyData(
            dev,
            &size_addr,
            0,
            null(),
            std::mem::size_of::<u32>() as u32,
            &frames as *const _ as *const c_void,
        );
        if s != 0 {
            return Err(format!("set buffer size: {s}"));
        }

        let mut got: u32 = 0;
        let mut gsize = std::mem::size_of::<u32>() as u32;
        let s = AudioObjectGetPropertyData(
            dev,
            &size_addr,
            0,
            null(),
            &mut gsize,
            &mut got as *mut _ as *mut c_void,
        );
        if s != 0 {
            return Err(format!("read back: {s}"));
        }
        Ok(got)
    }
}

pub fn make_button(
    title: &NSString,
    target: &handling::AppHandler,
    action: Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    unsafe { NSButton::buttonWithTitle_target_action(title, Some(target), Some(action), mtm) }
}

pub fn make_stack(
    views: &[Retained<NSView>],
    distribution: NSStackViewDistribution,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(views), mtm);
    stack.setDistribution(distribution);
    stack.setSpacing(10.0);
    stack
}

pub fn make_list(
    height: f64,
    mtm: MainThreadMarker,
) -> (Retained<NSScrollView>, Retained<NSTableView>) {
    let table = NSTableView::new(mtm);
    let column =
        NSTableColumn::initWithIdentifier(mtm.alloc::<NSTableColumn>(), ns_string!("name"));
    column.setResizingMask(NSTableColumnResizingOptions::AutoresizingMask);
    table.addTableColumn(&column);
    table.setHeaderView(None);
    table.setColumnAutoresizingStyle(
        NSTableViewColumnAutoresizingStyle::UniformColumnAutoresizingStyle,
    );
    table.setAllowsEmptySelection(true);
    table.setAllowsMultipleSelection(false);

    let scroll = NSScrollView::new(mtm);
    scroll.setDocumentView(Some(&table));
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(NSBorderType::BezelBorder);
    scroll
        .heightAnchor()
        .constraintGreaterThanOrEqualToConstant(height)
        .setActive(true);
    scroll.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
    (scroll, table)
}

pub fn make_faderslider(
    mtm: MainThreadMarker,
    label: &str,
    on_change: Option<Box<dyn Fn(f64)>>,
) -> (
    Retained<NSView>,
    Retained<NSSlider>,
    Retained<NSTextField>,
    Retained<custom::FaderSlider>,
) {
    let controller = custom::FaderSlider::new(mtm);

    let container = NSView::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(70.0, 150.0)),
    );
    container.setTranslatesAutoresizingMaskIntoConstraints(false);

    let label_view = NSTextField::labelWithString(&NSString::from_str(label), mtm);
    label_view.setFrame(NSRect::new(NSPoint::new(5.0, 0.0), NSSize::new(60.0, 14.0)));
    label_view.setAlignment(NSTextAlignment::Center);
    label_view.setControlSize(NSControlSize::Small);

    let tf_frame = NSRect::new(NSPoint::new(10.0, 16.0), NSSize::new(50.0, 20.0));
    let text_field = NSTextField::initWithFrame(mtm.alloc(), tf_frame);
    text_field.setAlignment(NSTextAlignment::Center);

    let slider_frame = NSRect::new(NSPoint::new(23.0, 46.0), NSSize::new(24.0, 100.0));
    let slider = NSSlider::initWithFrame(mtm.alloc(), slider_frame);
    slider.setMinValue(-80.0);
    slider.setMaxValue(24.0);
    slider.setDoubleValue(0.0);

    container.addSubview(&label_view);
    container.addSubview(&text_field);
    container.addSubview(&slider);

    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        container.widthAnchor().constraintEqualToConstant(70.0),
        container.heightAnchor().constraintEqualToConstant(150.0),
    ]));

    controller.attach(slider.clone(), text_field.clone());

    if let Some(cb) = on_change {
        controller.set_on_change(cb);
    }

    (container, slider, text_field, controller)
}

pub fn make_error_label(mtm: MainThreadMarker) -> Retained<NSTextField> {
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

pub fn padded_tab_content(view: &NSView, inset: f64, mtm: MainThreadMarker) -> Retained<NSView> {
    let container = NSView::new(mtm);

    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(view);

    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        view.leadingAnchor()
            .constraintEqualToAnchor_constant(&container.leadingAnchor(), inset),
        view.trailingAnchor()
            .constraintEqualToAnchor_constant(&container.trailingAnchor(), -inset),
        view.topAnchor()
            .constraintEqualToAnchor_constant(&container.topAnchor(), inset),
        view.bottomAnchor()
            .constraintEqualToAnchor_constant(&container.bottomAnchor(), -inset),
    ]));

    container
}

pub fn audio_duration(path: &Path) -> anyhow::Result<f64> {
    let file = open_audio_file(path)?;
    let (len, rate) = unsafe { (file.length() as f64, file.processingFormat().sampleRate()) };
    Ok(len / rate)
}

pub fn format_duration(secs: f64) -> String {
    let ms = (secs * 1000.0).round() as u64;
    format!("{}:{:02}.{:03}", ms / 60000, (ms / 1000) % 60, ms % 1000)
}

pub fn make_cue_list(
    height: f64,
    mtm: MainThreadMarker,
) -> (Retained<NSScrollView>, Retained<NSTableView>) {
    let table = NSTableView::new(mtm);
    for (ident, title, width) in [
        (ns_string!("id"), ns_string!("ID"), 80.0),
        (ns_string!("name"), ns_string!("Name"), 240.0),
        (ns_string!("duration"), ns_string!("Duration"), 90.0),
    ] {
        let column = NSTableColumn::initWithIdentifier(mtm.alloc::<NSTableColumn>(), ident);
        column.setTitle(title);
        column.setWidth(width);
        column.setResizingMask(NSTableColumnResizingOptions::AutoresizingMask);
        table.addTableColumn(&column);
    }
    table.setColumnAutoresizingStyle(
        NSTableViewColumnAutoresizingStyle::UniformColumnAutoresizingStyle,
    );
    table.setAllowsEmptySelection(true);
    table.setAllowsMultipleSelection(false);

    let scroll = NSScrollView::new(mtm);
    scroll.setDocumentView(Some(&table));
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(NSBorderType::BezelBorder);
    scroll
        .heightAnchor()
        .constraintGreaterThanOrEqualToConstant(height)
        .setActive(true);
    scroll.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
    (scroll, table)
}
