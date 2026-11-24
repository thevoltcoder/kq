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

fn schedule_from(state: &handling::AudioState, frame: i64) -> i64 {
    unsafe {
        let length = state.file.length();
        let frame = if frame >= length { 0 } else { frame.max(0) };
        state
            .player
            .scheduleSegment_startingFrame_frameCount_atTime_completionHandler(
                &state.file,
                frame,
                (length - frame) as u32,
                None,
                null_mut(),
            );
        frame
    }
}

pub fn current_frame(state: &handling::AudioState) -> i64 {
    unsafe {
        let Some(node_time) = state.player.lastRenderTime() else {
            return state.start_frame;
        };
        let Some(player_time) = state.player.playerTimeForNodeTime(&node_time) else {
            return state.start_frame;
        };
        state.start_frame + player_time.sampleTime().max(0)
    }
}

fn connect_chain(state: &mut handling::AudioState, slots: &[handling::Slot]) {
    unsafe {
        let mut next: Vec<(usize, Retained<AVAudioUnitEffect>)> = Vec::new();
        for (index, effect, on) in slots {
            let already = state.chain.iter().any(|(i, _)| i == index);
            if !already {
                state.engine.attachNode(&**effect);
            }
            effect.setBypass(!*on);
            next.push((*index, effect.clone()));
        }
        state.chain = next;

        let source: &AVAudioNode = &state.pre_mixer;
        let output: &AVAudioNode = &state.fade_mixer;
        let mut nodes: Vec<&AVAudioNode> = vec![source];
        for (_, effect) in &state.chain {
            nodes.push(&**effect);
        }
        nodes.push(output);
        for pair in nodes.windows(2) {
            state
                .engine
                .connect_to_format(pair[0], pair[1], Some(&state.format));
        }
    }
}

pub fn insert_effect(
    state: &mut handling::AudioState,
    index: usize,
    effect: &Retained<AVAudioUnitEffect>,
    on: bool,
) {
    if state.chain.iter().any(|(i, _)| *i == index) {
        return;
    }

    unsafe {
        effect.setBypass(!on);
        state.engine.attachNode(&**effect);

        let node: &AVAudioNode = &**effect;
        let mixer: &AVAudioNode = &state.fade_mixer;
        let pre: &AVAudioNode = &state.pre_mixer;
        let upstream: &AVAudioNode = match state.chain.last() {
            Some((_, last)) => &**last,
            None => pre,
        };

        let format = state.format.clone();

        state.engine.disconnectNodeOutput(upstream);
        state
            .engine
            .connect_to_format(upstream, node, Some(&format));
        state.engine.connect_to_format(node, mixer, Some(&format));

        println!(
            "after connect: up={:?} fx_in={:?} fx_out={:?} running={} playing={}",
            format,
            node.inputFormatForBus(0),
            node.outputFormatForBus(0),
            state.engine.isRunning(),
            state.player.isPlaying()
        );
    }

    state.chain.push((index, effect.clone()));
}

pub fn set_effect_enabled(state: &handling::AudioState, index: usize, enabled: bool) {
    if let Some((_, effect)) = state
        .chain
        .iter()
        .find(|(effect_index, _)| *effect_index == index)
    {
        unsafe {
            effect.setBypass(!enabled);
        }
    }
}

pub fn teardown(state: &handling::AudioState) {
    unsafe {
        state.player.stop();
        state.engine.stop();
        for (_, effect) in &state.chain {
            state.engine.detachNode(effect);
        }
        state.engine.detachNode(&state.fade_mixer);
        state.engine.detachNode(&state.player);
        state.engine.detachNode(&state.pre_mixer);
    }
}

fn open_audio_file(file_path: &Path) -> anyhow::Result<Retained<AVAudioFile>> {
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

pub fn play_audio(
    state: &mut handling::AudioState,
    file_path: &Path,
    start_frame: i64,
) -> anyhow::Result<()> {
    let file = open_audio_file(file_path)?;
    unsafe {
        state.player.stop();
    }
    state.file = file;
    state.path = file_path.to_path_buf();
    state.start_frame = schedule_from(state, start_frame);
    unsafe {
        state.player.play();
    }
    Ok(())
}

pub fn start_audio(
    file_path: &Path,
    slots: &[handling::Slot],
    start_frame: i64,
) -> anyhow::Result<handling::AudioState> {
    let file = open_audio_file(file_path)?;
    let mut state = unsafe {
        handling::AudioState {
            engine: AVAudioEngine::new(),
            player: AVAudioPlayerNode::new(),
            pre_mixer: AVAudioMixerNode::new(),
            chain: Vec::new(),
            fade_mixer: AVAudioMixerNode::new(),
            format: file.processingFormat(),
            start_frame: 0,
            path: file_path.to_path_buf(),
            file,
        }
    };

    unsafe {
        state.engine.attachNode(&state.player);
        state.engine.attachNode(&state.pre_mixer);
        state.engine.attachNode(&state.fade_mixer);
        let main_mixer = state.engine.mainMixerNode();
        state
            .engine
            .connect_to_format(&*state.player, &*state.pre_mixer, None);
        state
            .engine
            .connect_to_format(&*state.fade_mixer, &*main_mixer, None);
        state.fade_mixer.setOutputVolume(1.0);
    }

    connect_chain(&mut state, slots);
    state.start_frame = schedule_from(&state, start_frame);

    unsafe {
        state.engine.prepare();
        if let Err(error) = state.engine.startAndReturnError() {
            let info: Retained<NSDictionary> = msg_send![&*error, userInfo];
            let description: Retained<NSString> = msg_send![&*info, description];

            println!("Audio engine error: {error:?}");
            println!("Error details: {description}");

            teardown(&state);
            return Err(anyhow::anyhow!("Failed to start audio engine: {error:?}"));
        }
        state.player.play();
    }

    Ok(state)
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
