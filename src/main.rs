use anyhow::Context;
use block2::RcBlock;
use objc2::{rc::*, runtime::*, *};
use objc2_app_kit::*;
use objc2_audio_toolbox::*;
use objc2_avf_audio::*;
use objc2_core_audio_kit::AUGenericView;
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
const COCOA_UI_PROPERTY: u32 = 31;

#[repr(C)]
struct RawAudioUnit {
    _private: [u8; 0],
}

unsafe impl RefEncode for RawAudioUnit {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct(
        "ComponentInstanceRecord",
        &[Encoding::Array(1, &Encoding::LongLong)],
    ));
}

fn raw_audio_unit(node: &AVAudioUnitEffect) -> *mut RawAudioUnit {
    unsafe { msg_send![node, audioUnit] }
}

struct Installed {
    name: String,
    desc: AudioComponentDescription,
}

struct ChainItem {
    index: usize,
    node: Retained<AVAudioUnitEffect>,
}

struct Editor {
    #[allow(dead_code)]
    window: Retained<NSWindow>,
    #[allow(dead_code)]
    factory: Option<Retained<AnyObject>>,
}

fn list_installed_effects() -> Vec<Installed> {
    unsafe {
        let manager = AVAudioUnitComponentManager::sharedAudioUnitComponentManager();
        let mut list: Vec<Installed> = Vec::new();
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
                    .map(|c| Installed {
                        name: format!("{} - {}", c.manufacturerName(), c.name()),
                        desc: c.audioComponentDescription(),
                    }),
            );
        }
        list.sort_by_key(|i| i.name.to_lowercase());
        list
    }
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

fn cocoa_view(unit: *mut RawAudioUnit) -> Option<(Retained<NSView>, Retained<AnyObject>)> {
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

fn editor_window(mtm: MainThreadMarker, view: &NSView, title: &str) -> Retained<NSWindow> {
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

struct AudioState {
    engine: Retained<AVAudioEngine>,
    player: Retained<AVAudioPlayerNode>,
    nodes: Vec<Retained<AVAudioUnitEffect>>,
}

struct AppIvars {
    field: Retained<NSTextField>,
    error_label: Retained<NSTextField>,
    search: Retained<NSSearchField>,
    available_table: Retained<NSTableView>,
    chain_table: Retained<NSTableView>,
    installed: Vec<Installed>,
    filtered: RefCell<Vec<usize>>,
    chain: RefCell<Vec<ChainItem>>,
    editors: RefCell<Vec<Editor>>,
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

    unsafe impl NSTableViewDataSource for AppHandler {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows_in_table_view(&self, table: &NSTableView) -> NSInteger {
            if std::ptr::eq(table, &*self.ivars().available_table) {
                self.ivars().filtered.borrow().len() as NSInteger
            } else {
                self.ivars().chain.borrow().len() as NSInteger
            }
        }

        #[unsafe(method(tableView:objectValueForTableColumn:row:))]
        fn table_view_object_value(
            &self,
            table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> *mut NSObject {
            let ivars = self.ivars();
            let index = if std::ptr::eq(table, &*ivars.available_table) {
                ivars.filtered.borrow().get(row as usize).copied()
            } else {
                ivars.chain.borrow().get(row as usize).map(|c| c.index)
            };
            match index {
                Some(i) => {
                    let title = NSString::from_str(&ivars.installed[i].name);
                    Retained::autorelease_ptr(title) as *mut NSObject
                }
                None => std::ptr::null_mut(),
            }
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

            let nodes: Vec<Retained<AVAudioUnitEffect>> = self
                .ivars()
                .chain
                .borrow()
                .iter()
                .map(|c| c.node.clone())
                .collect();

            match start_audio(&path, nodes) {
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

        #[unsafe(method(onSearch:))]
        fn on_search(&self, _sender: Option<&AnyObject>) {
            let ivars = self.ivars();
            let query = ivars.search.stringValue().to_string().to_lowercase();
            let matches: Vec<usize> = ivars
                .installed
                .iter()
                .enumerate()
                .filter(|(_, i)| i.name.to_lowercase().contains(&query))
                .map(|(n, _)| n)
                .collect();
            *ivars.filtered.borrow_mut() = matches;
            ivars.available_table.reloadData();
        }

        #[unsafe(method(onAdd:))]
        fn on_add(&self, _sender: Option<&AnyObject>) {
            self.add_selected();
        }

        #[unsafe(method(onAvailableDouble:))]
        fn on_available_double(&self, _sender: Option<&AnyObject>) {
            self.add_selected();
        }

        #[unsafe(method(onRemove:))]
        fn on_remove(&self, _sender: Option<&AnyObject>) {
            let ivars = self.ivars();
            let row = ivars.chain_table.selectedRow();
            if row < 0 {
                return;
            }
            let mut chain = ivars.chain.borrow_mut();
            if (row as usize) < chain.len() {
                chain.remove(row as usize);
            }
            drop(chain);
            ivars.chain_table.reloadData();
        }

        #[unsafe(method(onEdit:))]
        fn on_edit(&self, _sender: Option<&AnyObject>) {
            self.edit_selected();
        }

        #[unsafe(method(onChainDouble:))]
        fn on_chain_double(&self, _sender: Option<&AnyObject>) {
            self.edit_selected();
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
        field: Retained<NSTextField>,
        error_label: Retained<NSTextField>,
        search: Retained<NSSearchField>,
        available_table: Retained<NSTableView>,
        chain_table: Retained<NSTableView>,
    ) -> Retained<Self> {
        let installed = list_installed_effects();
        let filtered = (0..installed.len()).collect();
        let this = mtm.alloc::<Self>().set_ivars(AppIvars {
            field,
            error_label,
            search,
            available_table,
            chain_table,
            installed,
            filtered: RefCell::new(filtered),
            chain: RefCell::new(Vec::new()),
            editors: RefCell::new(Vec::new()),
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

    fn add_selected(&self) {
        let ivars = self.ivars();
        let row = ivars.available_table.selectedRow();
        if row < 0 {
            return;
        }
        let picked = ivars.filtered.borrow().get(row as usize).copied();
        if let Some(index) = picked {
            let desc = ivars.installed[index].desc;
            let node: Retained<AVAudioUnitEffect> = unsafe {
                if desc.componentSubType == u32::from_be_bytes(*b"nbeq") {
                    Retained::into_super(AVAudioUnitEQ::initWithNumberOfBands(
                        AVAudioUnitEQ::alloc(),
                        4,
                    ))
                } else {
                    AVAudioUnitEffect::initWithAudioComponentDescription(
                        AVAudioUnitEffect::alloc(),
                        desc,
                    )
                }
            };
            ivars.chain.borrow_mut().push(ChainItem { index, node });
            ivars.chain_table.reloadData();
        }
    }

    fn edit_selected(&self) {
        let ivars = self.ivars();
        let row = ivars.chain_table.selectedRow();
        if row < 0 {
            return;
        }
        let picked = ivars
            .chain
            .borrow()
            .get(row as usize)
            .map(|c| (c.node.clone(), ivars.installed[c.index].name.clone()));
        if let Some((node, name)) = picked {
            self.open_editor(&node, name);
        }
    }

    fn open_editor(&self, node: &AVAudioUnitEffect, title: String) {
        let mtm = self.mtm();

        match cocoa_view(raw_audio_unit(node)) {
            Some((view, factory)) => {
                let window = editor_window(mtm, &view, &title);
                self.ivars().editors.borrow_mut().push(Editor {
                    window,
                    factory: Some(factory),
                });
                self.set_error(String::new());
            }
            None => {
                let view = unsafe {
                    AUGenericView::initWithAudioUnit(
                        mtm.alloc::<AUGenericView>(),
                        raw_audio_unit(node) as AudioUnit,
                    )
                };

                let window = editor_window(mtm, &view, &title);

                self.ivars().editors.borrow_mut().push(Editor {
                    window,
                    factory: None,
                });

                self.set_error(String::new());
            }
        }
    }

    fn finish_stop(&self) {
        if let Some(timer) = self.ivars().fade.borrow_mut().take() {
            timer.invalidate();
        }
        if let Some(state) = self.ivars().audio.borrow_mut().take() {
            unsafe {
                state.player.stop();
                state.engine.stop();
                for node in &state.nodes {
                    state.engine.detachNode(node);
                }
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

fn start_audio(
    file_path: &Path,
    nodes: Vec<Retained<AVAudioUnitEffect>>,
) -> anyhow::Result<AudioState> {
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

fn make_list(
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

fn padded_tab_content(view: &NSView, inset: f64, mtm: MainThreadMarker) -> Retained<NSView> {
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

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "KqBackgroundView"]
    struct BackgroundView;

    impl BackgroundView {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(window) = self.window() {
                window.makeFirstResponder(None);
            }
        }
    }
);

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);

    let field = NSTextField::textFieldWithString(&NSString::new(), mtm);
    field.setEditable(true);
    field.setSelectable(true);
    field.setPlaceholderString(Some(ns_string!("Path to an audio file")));
    if field.respondsToSelector(sel!(setAllowsWritingTools:)) {
        field.setAllowsWritingTools(false);
    }
    if field.respondsToSelector(sel!(setAllowsWritingToolsAffordance:)) {
        field.setAllowsWritingToolsAffordance(false);
    }

    let search = NSSearchField::new(mtm);
    search.setEditable(true);
    search.setSelectable(true);
    search.setPlaceholderString(Some(ns_string!("Search installed effects")));
    search.setSendsSearchStringImmediately(true);

    let (available_scroll, available_table) = make_list(150.0, mtm);
    let (chain_scroll, chain_table) = make_list(150.0, mtm);

    let error_label = make_error_label(mtm);
    let handler = AppHandler::new(
        mtm,
        field.clone(),
        error_label.clone(),
        search.clone(),
        available_table.clone(),
        chain_table.clone(),
    );
    app.setDelegate(Some(ProtocolObject::from_ref(&*handler)));
    unsafe {
        available_table.setDataSource(Some(ProtocolObject::from_ref(&*handler)));
        chain_table.setDataSource(Some(ProtocolObject::from_ref(&*handler)));
        available_table.setTarget(Some(&handler));
        available_table.setDoubleAction(Some(sel!(onAvailableDouble:)));
        chain_table.setTarget(Some(&handler));
        chain_table.setDoubleAction(Some(sel!(onChainDouble:)));
        search.setTarget(Some(&handler));
        search.setAction(Some(sel!(onSearch:)));
    }

    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(680.0, 380.0));
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
    window.setContentMinSize(NSSize::new(560.0, 340.0));

    let browse_btn = make_button(ns_string!("Browse…"), &handler, sel!(onBrowse:), mtm);
    let play_btn = make_button(ns_string!("Play"), &handler, sel!(onPlay:), mtm);
    let stop_btn = make_button(ns_string!("Stop"), &handler, sel!(onStop:), mtm);
    let add_btn = make_button(ns_string!("+"), &handler, sel!(onAdd:), mtm);
    let remove_btn = make_button(ns_string!("−"), &handler, sel!(onRemove:), mtm);
    let edit_btn = make_button(ns_string!("Edit"), &handler, sel!(onEdit:), mtm);

    play_btn.setKeyEquivalent(ns_string!(" "));
    stop_btn.setKeyEquivalent(ns_string!("\u{1b}"));

    let file_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(field.clone())),
            Retained::into_super(Retained::into_super(browse_btn)),
        ],
        NSStackViewDistribution::Fill,
        mtm,
    );

    let cname_field = NSTextField::textFieldWithString(&NSString::new(), mtm);
    cname_field.setPlaceholderString(Some(ns_string!("Cue Name")));

    let cid_field = NSTextField::textFieldWithString(&NSString::new(), mtm);
    cid_field.setPlaceholderString(Some(ns_string!("Cue ID")));

    let metadata_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(cid_field.clone())),
            Retained::into_super(Retained::into_super(cname_field.clone())),
        ],
        NSStackViewDistribution::FillEqually,
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

    let edit_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(add_btn)),
            Retained::into_super(Retained::into_super(remove_btn)),
            Retained::into_super(Retained::into_super(edit_btn)),
        ],
        NSStackViewDistribution::FillEqually,
        mtm,
    );

    let left_col = make_stack(
        &[
            Retained::into_super(Retained::into_super(Retained::into_super(search.clone()))),
            Retained::into_super(available_scroll.clone()),
        ],
        NSStackViewDistribution::Fill,
        mtm,
    );
    left_col.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);

    let right_col = make_stack(
        &[
            Retained::into_super(edit_row.clone()),
            Retained::into_super(chain_scroll.clone()),
        ],
        NSStackViewDistribution::Fill,
        mtm,
    );
    right_col.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);

    let columns = make_stack(
        &[
            Retained::into_super(left_col.clone()),
            Retained::into_super(right_col.clone()),
        ],
        NSStackViewDistribution::FillEqually,
        mtm,
    );
    columns.setSpacing(16.0);

    let description_text = NSTextView::new(mtm);
    description_text.setFrameSize(NSSize::new(500.0, 500.0));
    description_text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    description_text.setEditable(true);
    description_text.setSelectable(true);
    description_text.setRichText(false);
    description_text.setHorizontallyResizable(false);
    description_text.setVerticallyResizable(true);
    description_text.setMinSize(NSSize::new(0.0, 0.0));
    description_text.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
    description_text.setTextContainerInset(NSSize::new(8.0, 8.0));
    description_text.setString(&NSString::new());

    let text_container = unsafe { description_text.textContainer().unwrap() };
    text_container.setWidthTracksTextView(true);
    text_container.setContainerSize(NSSize::new(f64::MAX, f64::MAX));

    let description_scroll = NSScrollView::new(mtm);
    description_scroll.setDocumentView(Some(&description_text));
    description_scroll.setHasVerticalScroller(true);
    description_scroll.setHasHorizontalScroller(false);
    description_scroll.setBorderType(NSBorderType::BezelBorder);
    description_scroll.setTranslatesAutoresizingMaskIntoConstraints(false);

    let description_content = NSView::new(mtm);
    description_content.setTranslatesAutoresizingMaskIntoConstraints(false);

    file_row.setTranslatesAutoresizingMaskIntoConstraints(false);
    metadata_row.setTranslatesAutoresizingMaskIntoConstraints(false);

    description_content.addSubview(&file_row);
    description_content.addSubview(&metadata_row);
    description_content.addSubview(&description_scroll);

    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        file_row
            .leadingAnchor()
            .constraintEqualToAnchor(&description_content.leadingAnchor()),
        file_row
            .trailingAnchor()
            .constraintEqualToAnchor(&description_content.trailingAnchor()),
        file_row
            .topAnchor()
            .constraintEqualToAnchor(&description_content.topAnchor()),
        file_row.heightAnchor().constraintEqualToConstant(28.0),
        metadata_row
            .leadingAnchor()
            .constraintEqualToAnchor(&description_content.leadingAnchor()),
        metadata_row
            .trailingAnchor()
            .constraintEqualToAnchor(&description_content.trailingAnchor()),
        metadata_row
            .topAnchor()
            .constraintEqualToAnchor_constant(&file_row.bottomAnchor(), 12.0),
        metadata_row.heightAnchor().constraintEqualToConstant(28.0),
        description_scroll
            .leadingAnchor()
            .constraintEqualToAnchor(&description_content.leadingAnchor()),
        description_scroll
            .trailingAnchor()
            .constraintEqualToAnchor(&description_content.trailingAnchor()),
        description_scroll
            .topAnchor()
            .constraintEqualToAnchor_constant(&metadata_row.bottomAnchor(), 12.0),
        description_scroll
            .bottomAnchor()
            .constraintEqualToAnchor(&description_content.bottomAnchor()),
        cid_field
            .widthAnchor()
            .constraintEqualToAnchor_multiplier(&cname_field.widthAnchor(), 0.2),
    ]));

    let description_tab_content = padded_tab_content(&description_content, 16.0, mtm);
    let filters_tab_content = padded_tab_content(&columns, 16.0, mtm);

    let description_tab = NSTabViewItem::new();
    description_tab.setLabel(ns_string!("Info"));
    description_tab.setView(Some(&description_tab_content));

    let filters_tab = NSTabViewItem::new();
    filters_tab.setLabel(ns_string!("Filters"));
    filters_tab.setView(Some(&filters_tab_content));

    let tabs = NSTabView::new(mtm);
    tabs.addTabViewItem(&description_tab);
    tabs.addTabViewItem(&filters_tab);

    let root = make_stack(
        &[
            Retained::into_super(button_row.clone()),
            Retained::into_super(tabs.clone()),
            Retained::into_super(Retained::into_super(error_label.clone())),
        ],
        NSStackViewDistribution::Fill,
        mtm,
    );
    root.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    root.setSpacing(14.0);
    root.setTranslatesAutoresizingMaskIntoConstraints(false);

    let background: Retained<BackgroundView> =
        unsafe { msg_send![mtm.alloc::<BackgroundView>(), init] };

    window.setContentView(Some(&background));

    let content: &NSView = &background;
    content.addSubview(&root);

    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        root.centerXAnchor()
            .constraintEqualToAnchor(&content.centerXAnchor()),
        root.centerYAnchor()
            .constraintEqualToAnchor(&content.centerYAnchor()),
        root.widthAnchor()
            .constraintEqualToAnchor_multiplier(&content.widthAnchor(), 0.85),
        button_row
            .widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        tabs.widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        tabs.heightAnchor().constraintEqualToConstant(260.0),
        error_label
            .widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        error_label.heightAnchor().constraintEqualToConstant(36.0),
    ]));

    content.layoutSubtreeIfNeeded();
    tabs.layoutSubtreeIfNeeded();
    columns.layoutSubtreeIfNeeded();
    description_content.layoutSubtreeIfNeeded();

    window.center();
    window.makeKeyAndOrderFront(None);

    content.layoutSubtreeIfNeeded();
    tabs.layoutSubtreeIfNeeded();
    columns.layoutSubtreeIfNeeded();
    description_content.layoutSubtreeIfNeeded();

    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
}
