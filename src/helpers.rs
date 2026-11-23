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

pub fn fade_out<F>(player: Retained<AVAudioPlayerNode>, on_complete: F) -> Retained<NSTimer>
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

pub fn set_matrix_gain(unit: &AVAudioUnit, input: u32, output: u32, lin: f32) {
    let element = (input << 16) | output;
    unsafe {
        let status = AudioUnitSetParameter(
            raw_audio_unit(unit) as AudioUnit,
            0,
            kAudioUnitScope_Global,
            element,
            lin,
            0,
        );
        if status != 0 {
            eprintln!("matrix gain {input}->{output} failed: {status}");
        }
    }
}

fn init_matrix_volumes(unit: &AVAudioUnit) {
    unsafe {
        let au = raw_audio_unit(unit) as AudioUnit;
        let set = |scope: u32, element: u32, value: f32| {
            let status = AudioUnitSetParameter(au, 0, scope, element, value, 0);
            if status != 0 {
                eprintln!("matrix volume scope {scope} element {element} failed: {status}");
            }
        };
        set(kAudioUnitScope_Global, 0xFFFF_FFFF, 1.0);
        for ch in 0..2u32 {
            set(kAudioUnitScope_Input, ch, 1.0);
            set(kAudioUnitScope_Output, ch, 1.0);
        }
        for i in 0..2u32 {
            for o in 0..2u32 {
                set(
                    kAudioUnitScope_Global,
                    (i << 16) | o,
                    if i == o { 1.0 } else { 0.0 },
                );
            }
        }
    }
}

pub fn start_audio(
    file_path: &Path,
    nodes: Vec<Retained<AVAudioUnitEffect>>,
) -> anyhow::Result<handling::AudioState> {
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
        let fade_mixer = AVAudioMixerNode::new();
        let matrix = make_matrix_mixer();

        if matrix.is_none() {
            eprintln!("matrix mixer could not be instantiated");
        }

        engine.attachNode(&player);
        engine.attachNode(&fade_mixer);
        for node in &nodes {
            engine.attachNode(node);
        }
        if let Some(m) = &matrix {
            engine.attachNode(m);
        }

        engine.connect_to_format(&player, &fade_mixer, Some(&fmt));

        let mut prev: &AVAudioNode = &fade_mixer;
        for node in &nodes {
            engine.connect_to_format(prev, node, Some(&fmt));
            prev = node;
        }
        match &matrix {
            Some(m) => {
                engine.connect_to_format(prev, m, Some(&fmt));
                engine.connect_to_format(m, &mixer, Some(&fmt));
            }
            None => engine.connect_to_format(prev, &mixer, Some(&fmt)),
        }

        player.scheduleFile_atTime_completionHandler(&file, None, std::ptr::null_mut());
        engine.startAndReturnError()?;
        if let Some(m) = &matrix {
            init_matrix_volumes(m);
        }
        player.play();

        Ok(handling::AudioState {
            engine,
            player,
            nodes,
            fade_mixer,
            matrix,
        })
    }
}

pub fn rewire(state: &mut handling::AudioState, nodes: Vec<Retained<AVAudioUnitEffect>>) {
    unsafe {
        let fmt = state.fade_mixer.outputFormatForBus(0);
        let same = |a: &Retained<AVAudioUnitEffect>, b: &Retained<AVAudioUnitEffect>| {
            Retained::as_ptr(a) == Retained::as_ptr(b)
        };
        for old in &state.nodes {
            if !nodes.iter().any(|n| same(n, old)) {
                state.engine.detachNode(old);
            }
        }
        for node in &nodes {
            if !state.nodes.iter().any(|o| same(o, node)) {
                state.engine.attachNode(node);
            }
        }
        let mut prev: &AVAudioNode = &state.fade_mixer;
        for node in &nodes {
            state.engine.connect_to_format(prev, node, Some(&fmt));
            prev = node;
        }
        match &state.matrix {
            Some(m) => state.engine.connect_to_format(prev, m, Some(&fmt)),
            None => {
                let main = state.engine.mainMixerNode();
                state.engine.connect_to_format(prev, &main, Some(&fmt));
            }
        }
        state.nodes = nodes;
    }
}

struct SendUnit(Option<Retained<AVAudioUnit>>);
unsafe impl Send for SendUnit {}

fn make_matrix_mixer() -> Option<Retained<AVAudioUnit>> {
    let desc = AudioComponentDescription {
        componentType: u32::from_be_bytes(*b"aumx"),
        componentSubType: u32::from_be_bytes(*b"mxmx"),
        componentManufacturer: u32::from_be_bytes(*b"appl"),
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    let (tx, rx) = std::sync::mpsc::channel::<SendUnit>();
    let block = RcBlock::new(move |unit: *mut AVAudioUnit, _err: *mut NSError| {
        let retained = unsafe { Retained::retain(unit) };
        let _ = tx.send(SendUnit(retained));
    });
    unsafe {
        AVAudioUnit::instantiateWithComponentDescription_options_completionHandler(
            desc,
            AudioComponentInstantiationOptions(0),
            &block,
        );
    }
    rx.recv_timeout(std::time::Duration::from_secs(3)).ok()?.0
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
        .constraintEqualToConstant(height)
        .setActive(true);
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
