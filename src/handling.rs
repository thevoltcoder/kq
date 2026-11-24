use super::*;

#[repr(C)]
pub struct RawAudioUnit {
    _private: [u8; 0],
}

pub type Slot = (usize, Retained<AVAudioUnitEffect>, bool);

unsafe impl RefEncode for RawAudioUnit {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct(
        "ComponentInstanceRecord",
        &[Encoding::Array(1, &Encoding::LongLong)],
    ));
}

#[derive(Clone)]
pub struct Installed {
    pub name: String,
    pub desc: AudioComponentDescription,
}

pub struct Editor {
    pub index: usize,
    pub window: Retained<NSWindow>,
    #[allow(dead_code)]
    pub factory: Option<Retained<AnyObject>>,
}

pub struct AudioState {
    pub engine: Retained<AVAudioEngine>,
    pub player: Retained<AVAudioPlayerNode>,
    pub pre_mixer: Retained<AVAudioMixerNode>,
    pub chain: Vec<(usize, Retained<AVAudioUnitEffect>)>,
    pub fade_mixer: Retained<AVAudioMixerNode>,
    pub format: Retained<AVAudioFormat>,
    pub start_frame: i64,
    pub path: PathBuf,
    pub file: Retained<AVAudioFile>,
}

pub struct AppIvars {
    pub field: Retained<NSTextField>,
    pub error_label: Retained<NSTextField>,
    pub search: Retained<NSSearchField>,
    pub available_table: Retained<NSTableView>,
    pub installed: Vec<Installed>,
    pub nodes: RefCell<Vec<Option<Retained<AVAudioUnitEffect>>>>,
    pub filtered: RefCell<Vec<usize>>,
    pub enabled: RefCell<Vec<bool>>,
    pub editors: RefCell<Vec<Editor>>,
    pub audio: RefCell<Option<AudioState>>,
    pub fade: RefCell<Option<Retained<NSTimer>>>,
    pub gain: Cell<f32>,
    pub gain_l: Cell<f32>,
    pub gain_r: Cell<f32>,
    pub loading: RefCell<Vec<bool>>,
    pub effect_tx: std::sync::mpsc::Sender<(usize, Result<usize, String>)>,
    pub effect_rx: RefCell<std::sync::mpsc::Receiver<(usize, Result<usize, String>)>>,
    pub pending_playback: RefCell<Option<(PathBuf, i64)>>,
    pub pending_editor: Cell<Option<usize>>,
    pub poll_timer: RefCell<Option<Retained<NSTimer>>>,
    pub rebuild_at: Cell<Option<std::time::Instant>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppIvars]
    #[name = "AppHandler"]
    pub struct AppHandler;

    unsafe impl NSObjectProtocol for AppHandler {}

    unsafe impl NSApplicationDelegate for AppHandler {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn application_should_terminate_after_last_window_closed(
            &self,
            _sender: &NSApplication,
        ) -> bool {
            self.finish_stop();
            if let Some(state) = self.ivars().audio.borrow().as_ref() {
                helpers::teardown(state);
            }
            true
        }
    }

    unsafe impl NSTableViewDataSource for AppHandler {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows_in_table_view(&self, table: &NSTableView) -> NSInteger {
            if std::ptr::eq(table, &*self.ivars().available_table) {
                self.ivars().filtered.borrow().len() as NSInteger
            } else {
                self.ivars().installed.len() as NSInteger
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
                Some(row as usize)
            };
            match index {
                Some(i) if i < ivars.installed.len() => {
                    let mark = if ivars.enabled.borrow()[i] { "● " } else { "○ " };
                    let title = NSString::from_str(&format!("{mark}{}", ivars.installed[i].name));
                    Retained::autorelease_ptr(title) as *mut NSObject
                }
                _ => std::ptr::null_mut(),
            }
        }
    }

    impl AppHandler {
        #[unsafe(method(onSelect:))]
        fn on_select(&self, _sender: Option<&AnyObject>) {
            let row = self.ivars().available_table.clickedRow();
            if row < 0 {
                return;
            }
            let picked = self.ivars().filtered.borrow().get(row as usize).copied();
            if let Some(index) = picked {
                self.request_node(index);
            }
        }

        #[unsafe(method(onPlay:))]
        fn on_play(&self, _sender: Option<&AnyObject>) {
            self.finish_stop();

            let text = self.ivars().field.stringValue().to_string();
            let path = PathBuf::from(text.trim());
            if !helpers::is_audio_path(&path) {
                self.set_error(format!("Not a supported audio file: {}", path.display()));
                return;
            }
            self.start_playback(&path, 0);
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

        #[unsafe(method(onEnable:))]
        fn on_enable(&self, _sender: Option<&AnyObject>) {
            self.set_selected_enabled(true);
        }

        #[unsafe(method(onDisable:))]
        fn on_disable(&self, _sender: Option<&AnyObject>) {
            self.set_selected_enabled(false);
        }

        #[unsafe(method(onEdit:))]
        fn on_edit(&self, _sender: Option<&AnyObject>) {
            self.edit_selected();
        }

        #[unsafe(method(onAvailableDouble:))]
        fn on_available_double(&self, _sender: Option<&AnyObject>) {
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
    pub fn new(
        mtm: MainThreadMarker,
        field: Retained<NSTextField>,
        error_label: Retained<NSTextField>,
        search: Retained<NSSearchField>,
        available_table: Retained<NSTableView>,
    ) -> Retained<Self> {
        let installed = helpers::list_installed_effects();
        let filtered = (0..installed.len()).collect();
        let enabled = vec![false; installed.len()];
        let nodes = (0..installed.len()).map(|_| None).collect();
        let loading = vec![false; installed.len()];

        let (effect_tx, effect_rx) = std::sync::mpsc::channel();

        let this = mtm.alloc::<Self>().set_ivars(AppIvars {
            field,
            error_label,
            search,
            available_table,
            installed,
            nodes: RefCell::new(nodes),
            loading: RefCell::new(loading),
            effect_tx,
            effect_rx: RefCell::new(effect_rx),
            pending_playback: RefCell::new(None),
            pending_editor: Cell::new(None),
            poll_timer: RefCell::new(None),
            filtered: RefCell::new(filtered),
            enabled: RefCell::new(enabled),
            editors: RefCell::new(Vec::new()),
            audio: RefCell::new(None),
            fade: RefCell::new(None),
            gain: Cell::new(1.0),
            gain_l: Cell::new(1.0),
            gain_r: Cell::new(1.0),
            rebuild_at: Cell::new(None),
        });

        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let handler = this.clone();

        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            handler.poll_effects();
        });

        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.001, true, &block) };

        *this.ivars().poll_timer.borrow_mut() = Some(timer);

        for index in 0..this.ivars().installed.len() {
            this.request_node(index);
        }

        this
    }

    fn db_to_lin(db: f64) -> f32 {
        if db <= -79.9 {
            0.0
        } else {
            10f32.powf(db as f32 / 20.0)
        }
    }

    pub fn set_gain(&self, db: f64) {
        self.ivars().gain.set(Self::db_to_lin(db));
        self.apply_gain();
    }

    pub fn set_gain_l(&self, _db: f64) {}

    pub fn set_gain_r(&self, _db: f64) {}

    fn apply_gain(&self) {
        let gain = self.ivars().gain.get();
        if let Some(state) = self.ivars().audio.borrow().as_ref() {
            unsafe {
                state.engine.mainMixerNode().setOutputVolume(gain);
            }
        }
    }

    pub fn set_error(&self, message: String) {
        let label = &self.ivars().error_label;
        label.setStringValue(&NSString::from_str(&message));
        label.setAlphaValue(if message.is_empty() { 0.0 } else { 1.0 });
    }

    fn request_node(&self, index: usize) {
        let ivars = self.ivars();
        if index >= ivars.installed.len() || ivars.nodes.borrow()[index].is_some() {
            return;
        }
        {
            let mut loading = ivars.loading.borrow_mut();
            if loading[index] {
                return;
            }
            loading[index] = true;
        }
        let desc = ivars.installed[index].desc;
        if desc.componentSubType == u32::from_be_bytes(*b"nbeq") {
            let node = helpers::instantiate_builtin_eq();
            ivars.nodes.borrow_mut()[index] = Some(node);
            ivars.loading.borrow_mut()[index] = false;
            self.effect_ready(index);
            return;
        }
        helpers::instantiate_effect_async(desc, index, ivars.effect_tx.clone());
    }

    fn set_selected_enabled(&self, on: bool) {
        let ivars = self.ivars();
        let row = ivars.available_table.selectedRow();
        if row < 0 {
            return;
        }
        let picked = ivars.filtered.borrow().get(row as usize).copied();
        let Some(index) = picked else {
            return;
        };
        self.set_effect(index, on);
        ivars.available_table.reloadData();
        let set = NSIndexSet::indexSetWithIndex(row as usize);
        ivars
            .available_table
            .selectRowIndexes_byExtendingSelection(&set, false);
    }

    pub fn set_effect(&self, index: usize, on: bool) {
        let ivars = self.ivars();
        {
            let mut enabled = ivars.enabled.borrow_mut();
            if index >= enabled.len() || enabled[index] == on {
                return;
            }
            enabled[index] = on;
        }
        if on {
            self.request_node(index);
        }
        if let Some(state) = ivars.audio.borrow().as_ref() {
            helpers::set_effect_enabled(state, index, on);
        }
    }

    pub fn edit_selected(&self) {
        let ivars = self.ivars();
        let row = ivars.available_table.selectedRow();
        if row < 0 {
            return;
        }
        let index = ivars.filtered.borrow().get(row as usize).copied();
        let Some(index) = index else {
            return;
        };
        {
            let editors = ivars.editors.borrow();
            if let Some(editor) = editors.iter().find(|e| e.index == index) {
                ivars.pending_editor.set(None);
                editor.window.makeKeyAndOrderFront(None);
                return;
            }
        }
        if let Some(node) = ivars.nodes.borrow()[index].clone() {
            let name = ivars.installed[index].name.clone();
            self.open_editor(index, &node, name);
        } else {
            ivars.pending_editor.set(Some(index));
            self.request_node(index);
        }
    }

    pub fn open_editor(&self, index: usize, node: &AVAudioUnitEffect, title: String) {
        let mtm = self.mtm();
        let raw = helpers::raw_audio_unit(node);

        match helpers::cocoa_view(raw) {
            Some((view, factory)) => {
                let window = helpers::editor_window(mtm, &view, &title);
                self.ivars().editors.borrow_mut().push(Editor {
                    index,
                    window,
                    factory: Some(factory),
                });
                self.set_error(String::new());
            }
            None => {
                let view = unsafe {
                    AUGenericView::initWithAudioUnit(mtm.alloc::<AUGenericView>(), raw as AudioUnit)
                };

                let window = helpers::editor_window(mtm, &view, &title);

                self.ivars().editors.borrow_mut().push(Editor {
                    index,
                    window,
                    factory: None,
                });

                self.set_error(String::new());
            }
        }
    }

    pub fn finish_stop(&self) {
        self.ivars().pending_playback.borrow_mut().take();
        if let Some(timer) = self.ivars().fade.borrow_mut().take() {
            timer.invalidate();
        }
        let state = self.ivars().audio.borrow_mut().take();
        if let Some(state) = state {
            helpers::teardown(&state);
        }
    }

    pub fn begin_fade_stop(&self) {
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
        let timer = helpers::fade_out(player, move || this.finish_stop());
        *self.ivars().fade.borrow_mut() = Some(timer);
    }

    fn start_playback(&self, path: &Path, frame: i64) {
        *self.ivars().pending_playback.borrow_mut() = Some((path.to_path_buf(), frame));

        self.try_start_pending_playback();
    }

    fn try_start_pending_playback(&self) {
        let ivars = self.ivars();
        if ivars.audio.borrow().is_some() {
            return;
        }
        let Some((path, frame)) = ivars.pending_playback.borrow().clone() else {
            return;
        };
        let enabled = ivars.enabled.borrow().clone();
        for (index, on) in enabled.iter().enumerate() {
            if *on && ivars.nodes.borrow()[index].is_none() {
                self.request_node(index);
                return;
            }
        }
        ivars.pending_playback.borrow_mut().take();
        let slots = self.loaded_slots();
        match helpers::start_audio(&path, &slots, frame) {
            Ok(state) => {
                *ivars.audio.borrow_mut() = Some(state);
                self.apply_gain();
                self.set_error(String::new());
            }
            Err(error) => {
                self.set_error(error.to_string());
            }
        }
    }

    fn poll_effects(&self) {
        loop {
            let result = self.ivars().effect_rx.borrow().try_recv();

            let (index, result) = match result {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            };

            if index >= self.ivars().installed.len() {
                continue;
            }

            self.ivars().loading.borrow_mut()[index] = false;

            match result {
                Ok(raw) => {
                    let unit =
                        unsafe { Retained::<AVAudioUnit>::from_raw(raw as *mut AVAudioUnit) };

                    let Some(unit) = unit else {
                        self.set_error("Received a null Audio Unit".to_string());
                        continue;
                    };

                    match unit.downcast::<AVAudioUnitEffect>() {
                        Ok(effect) => {
                            self.ivars().nodes.borrow_mut()[index] = Some(effect);
                            self.effect_ready(index);
                        }
                        Err(_) => {
                            self.set_error(format!(
                                "Audio Unit {} is not an AVAudioUnitEffect",
                                self.ivars().installed[index].name
                            ));
                        }
                    }
                }

                Err(error) => {
                    self.ivars().pending_playback.borrow_mut().take();

                    if self.ivars().pending_editor.get() == Some(index) {
                        self.ivars().pending_editor.set(None);
                    }

                    self.set_error(format!(
                        "Failed to instantiate {}: {}",
                        self.ivars().installed[index].name,
                        error
                    ));
                }
            }
        }
    }

    fn effect_ready(&self, index: usize) {
        self.sync_chain();
        if self.ivars().pending_editor.get() == Some(index) {
            self.ivars().pending_editor.set(None);

            let node = self.ivars().nodes.borrow()[index].as_ref().cloned();

            if let Some(node) = node {
                let name = self.ivars().installed[index].name.clone();
                self.open_editor(index, &node, name);
            }
        }

        self.try_start_pending_playback();
    }

    fn loaded_slots(&self) -> Vec<Slot> {
        let ivars = self.ivars();
        let enabled = ivars.enabled.borrow();
        ivars
            .nodes
            .borrow()
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.as_ref().map(|n| (i, n.clone(), enabled[i])))
            .collect()
    }

    fn sync_chain(&self) {
        let ivars = self.ivars();
        if ivars.fade.borrow().is_some() {
            return;
        }
        let slots = self.loaded_slots();
        let mut audio = ivars.audio.borrow_mut();
        let Some(state) = audio.as_mut() else {
            return;
        };
        for (index, node, on) in &slots {
            helpers::insert_effect(state, *index, node, *on);
        }
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "KqBackgroundView"]
    pub struct BackgroundView;

    impl BackgroundView {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(window) = self.window() {
                window.makeFirstResponder(None);
            }
        }
    }
);
