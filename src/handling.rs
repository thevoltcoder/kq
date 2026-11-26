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

pub use super::rt::AudioState;

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
    pub pending_playback: RefCell<VecDeque<(PathBuf, i64)>>,
    pub pending_editor: Cell<Option<usize>>,
    pub poll_timer: RefCell<Option<Retained<NSTimer>>>,
    pub rebuild_at: Cell<Option<std::time::Instant>>,
    pub termination_confirmed: Cell<bool>,
    pub cue_id: Retained<NSTextField>,
    pub cue_name: Retained<NSTextField>,
    pub cue_table: Retained<NSTableView>,
    pub cues: RefCell<Vec<cue::Cue>>,
    pub active_cue: Cell<usize>,
    pub initial_cue_selection: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppIvars]
    #[name = "AppHandler"]
    pub struct AppHandler;

    unsafe impl NSObjectProtocol for AppHandler {}

    unsafe impl NSTextFieldDelegate for AppHandler {}

    unsafe impl NSApplicationDelegate for AppHandler {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn application_should_terminate_after_last_window_closed(
            &self,
            _sender: &NSApplication,
        ) -> bool {
            true
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn application_should_terminate(
            &self,
            _sender: &NSApplication,
        ) -> NSApplicationTerminateReply {
            let ivars = self.ivars();

            if ivars.termination_confirmed.get() {
                return NSApplicationTerminateReply::TerminateNow;
            }

            if !self.confirm_close() {
                return NSApplicationTerminateReply::TerminateCancel;
            }

            ivars.termination_confirmed.set(true);

            self.close_editors();
            self.finish_stop();

            NSApplicationTerminateReply::TerminateNow
        }
    }

    unsafe impl NSWindowDelegate for AppHandler {}

    unsafe impl NSControlTextEditingDelegate for AppHandler {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.update_selected_cue();
        }
    }

    unsafe impl NSTableViewDataSource for AppHandler {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows_in_table_view(&self, table: &NSTableView) -> NSInteger {
            let ivars = self.ivars();
            if std::ptr::eq(table, &*ivars.cue_table) {
                ivars.cues.borrow().len() as NSInteger
            } else if std::ptr::eq(table, &*ivars.available_table) {
                ivars.filtered.borrow().len() as NSInteger
            } else {
                ivars.installed.len() as NSInteger
            }
        }

        #[unsafe(method(tableView:objectValueForTableColumn:row:))]
        fn table_view_object_value(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> *mut NSObject {
            let ivars = self.ivars();
            if std::ptr::eq(table, &*ivars.cue_table) {
                let cues = ivars.cues.borrow();
                let Some(cue) = cues.get(row as usize) else {
                    return std::ptr::null_mut();
                };
                let text = match column.map(|c| c.identifier().to_string()).as_deref() {
                    Some("id") => cue.id.clone(),
                    Some("name") => cue.name.clone(),
                    Some("duration") => helpers::format_duration(cue.duration),
                    _ => return std::ptr::null_mut(),
                };
                return Retained::autorelease_ptr(NSString::from_str(&text)) as *mut NSObject;
            }
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
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> objc2::runtime::Bool {
            let ivars = self.ivars();

            if ivars.termination_confirmed.get() {
                return objc2::runtime::Bool::YES;
            }

            if !self.confirm_close() {
                return objc2::runtime::Bool::NO;
            }

            ivars.termination_confirmed.set(true);

            self.close_editors();
            self.finish_stop();

            objc2::runtime::Bool::YES
        }

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
            let ivars = self.ivars();
            let index = ivars.active_cue.get();

            let Some(cue) = ivars.cues.borrow().get(index).cloned() else {
                return;
            };

            if !helpers::is_audio_path(&cue.path) {
                self.set_error(format!("Cue {} has no valid audio file", index + 1));
                return;
            }

            self.start_playback(&cue.path, 0);

            let next = (index + 1).min(ivars.cues.borrow().len() - 1);
            ivars.active_cue.set(next);

            let selected = NSIndexSet::indexSetWithIndex(next);
            ivars.cue_table.selectRowIndexes_byExtendingSelection(&selected, false);
            self.load_selected_cue();
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
            self.update_selected_cue();
        }

        #[unsafe(method(onAddCue:))]
        fn on_add_cue(&self, _sender: Option<&AnyObject>) {
            self.add_cue();
        }

        #[unsafe(method(onRemoveCue:))]
        fn on_remove_cue(&self, _sender: Option<&AnyObject>) {
            self.remove_cue();
        }

        #[unsafe(method(onSaveCue:))]
        fn on_save_cue(&self, _sender: Option<&AnyObject>) {
            self.save_cue();
        }

        #[unsafe(method(onCueSelect:))]
        fn on_cue_select(&self, _sender: Option<&AnyObject>) {
            self.load_selected_cue();
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
        cue_id: Retained<NSTextField>,
        cue_name: Retained<NSTextField>,
        cue_table: Retained<NSTableView>,
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
            pending_playback: RefCell::new(VecDeque::new()),
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
            termination_confirmed: Cell::new(false),
            cue_id,
            cue_name,
            cue_table,
            cues: RefCell::new(vec![cue::Cue {
                id: String::new(),
                name: String::new(),
                path: PathBuf::new(),
                duration: 0.0,
            }]),
            active_cue: Cell::new(0),
            initial_cue_selection: Cell::new(false),
        });

        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let handler = this.clone();

        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            handler.poll_effects();
        });

        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.01, true, &block) };

        *this.ivars().poll_timer.borrow_mut() = Some(timer);

        for index in 0..this.ivars().installed.len() {
            this.request_node(index);
        }

        this
    }

    fn ensure_initial_cue_selection(&self) {
        let ivars = self.ivars();

        if ivars.initial_cue_selection.get() {
            return;
        }

        if ivars.cues.borrow().is_empty() {
            return;
        }

        ivars.cue_table.reloadData();

        let selected = NSIndexSet::indexSetWithIndex(0);
        ivars
            .cue_table
            .selectRowIndexes_byExtendingSelection(&selected, false);

        if ivars.cue_table.selectedRow() == 0 {
            ivars.active_cue.set(0);
            ivars.initial_cue_selection.set(true);
            self.load_selected_cue();
        }
    }

    fn confirm_close(&self) -> bool {
        let alert = NSAlert::new(self.mtm());

        alert.setMessageText(ns_string!("Are you sure you want to close?"));
        alert.setInformativeText(ns_string!("Any unsaved changes will be lost."));

        alert.addButtonWithTitle(ns_string!("Cancel"));
        alert.addButtonWithTitle(ns_string!("Close Anyway"));

        alert.runModal() == NSAlertSecondButtonReturn
    }

    fn close_editors(&self) {
        let editors = std::mem::take(&mut *self.ivars().editors.borrow_mut());

        for editor in editors {
            editor.window.close();
        }
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
            state
                .sh
                .gain
                .store(gain.to_bits(), std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn finish_stop(&self) {
        self.ivars().pending_playback.borrow_mut().clear();
        if let Some(timer) = self.ivars().fade.borrow_mut().take() {
            timer.invalidate();
        }
        if let Some(state) = self.ivars().audio.borrow().as_ref() {
            state.stop();
        }
    }

    pub fn begin_fade_stop(&self) {
        let Some(sh) = self.ivars().audio.borrow().as_ref().map(|s| s.sh.clone()) else {
            return;
        };
        let this = self.retain();
        let timer = helpers::fade_out(sh, move || this.finish_stop());
        *self.ivars().fade.borrow_mut() = Some(timer);
    }

    fn try_start_pending_playback(&self) {
        let ivars = self.ivars();
        let Some((path, frame)) = ivars.pending_playback.borrow_mut().pop_front() else {
            return;
        };
        if ivars.audio.borrow().is_none() {
            match rt::AudioState::new(ivars.installed.len()) {
                Ok(a) => {
                    println!("audio state created");
                    *ivars.audio.borrow_mut() = Some(a);
                    self.sync_chain();
                }
                Err(e) => {
                    println!("AudioState::new failed: {e}");
                    self.set_error(e);
                    return;
                }
            }
        }
        let enabled = ivars.enabled.borrow().clone();
        for (index, on) in enabled.iter().enumerate() {
            if *on && ivars.nodes.borrow()[index].is_none() {
                self.request_node(index);
                ivars
                    .pending_playback
                    .borrow_mut()
                    .push_front((path, frame));
                return;
            }
        }
        if let Some(state) = ivars.audio.borrow().as_ref() {
            match helpers::play_audio(state, &path, frame) {
                Ok(()) => {
                    self.apply_gain();
                    self.set_error(String::new());
                }
                Err(e) => self.set_error(e.to_string()),
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
        let t = Instant::now();
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
        println!("toggle took {:?}", t.elapsed());
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

    fn start_playback(&self, path: &Path, frame: i64) {
        println!("start_playback {path:?}");
        self.ivars()
            .pending_playback
            .borrow_mut()
            .push_back((path.to_path_buf(), frame));
        self.try_start_pending_playback();
    }

    fn poll_effects(&self) {
        self.ensure_initial_cue_selection();

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
                    self.ivars().pending_playback.borrow_mut().clear();

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

    fn add_cue(&self) {
        let ivars = self.ivars();
        let mut cues = ivars.cues.borrow_mut();

        cues.push(cue::Cue {
            id: String::new(),
            name: String::new(),
            path: PathBuf::new(),
            duration: 0.0,
        });

        let index = cues.len() - 1;
        drop(cues);

        ivars.active_cue.set(index);
        ivars.cue_table.reloadData();

        let selected = NSIndexSet::indexSetWithIndex(index);
        ivars
            .cue_table
            .selectRowIndexes_byExtendingSelection(&selected, false);

        ivars.cue_id.setStringValue(&NSString::new());
        ivars.cue_name.setStringValue(&NSString::new());
        ivars.field.setStringValue(&NSString::new());

        self.set_error(String::new());
    }

    fn remove_cue(&self) {
        let ivars = self.ivars();
        let row = ivars.cue_table.selectedRow();

        if row < 0 {
            return;
        }

        let mut cues = ivars.cues.borrow_mut();

        if cues.len() <= 1 {
            self.set_error("You must always have at least one cue".to_string());
            return;
        }

        let row = row as usize;
        if row >= cues.len() {
            return;
        }

        cues.remove(row);

        let active = ivars.active_cue.get();
        let next_active = if row < active {
            active - 1
        } else {
            active.min(cues.len() - 1)
        };

        drop(cues);

        ivars.active_cue.set(next_active);
        ivars.cue_table.reloadData();

        let selected = NSIndexSet::indexSetWithIndex(next_active);
        ivars
            .cue_table
            .selectRowIndexes_byExtendingSelection(&selected, false);

        self.load_selected_cue();
        self.set_error(String::new());
    }

    fn save_cue(&self) {
        let ivars = self.ivars();
        let row = ivars.active_cue.get();

        let id = ivars.cue_id.stringValue().to_string().trim().to_string();
        let name = ivars.cue_name.stringValue().to_string().trim().to_string();
        let path = PathBuf::from(ivars.field.stringValue().to_string().trim());

        if id.is_empty() {
            self.set_error("Cue ID is required".to_string());
            return;
        }

        if !helpers::is_audio_path(&path) {
            self.set_error(format!("Not a supported audio file: {}", path.display()));
            return;
        }

        let duration = match helpers::audio_duration(&path) {
            Ok(d) => d,
            Err(e) => {
                self.set_error(e.to_string());
                return;
            }
        };

        let mut cues = ivars.cues.borrow_mut();

        if row >= cues.len() {
            return;
        }

        if cues
            .iter()
            .enumerate()
            .any(|(i, cue)| i != row && cue.id == id)
        {
            self.set_error(format!("Cue ID {id} already exists"));
            return;
        }

        cues[row] = cue::Cue {
            id,
            name,
            path,
            duration,
        };

        drop(cues);

        ivars.cue_table.reloadData();
        self.set_error(String::new());
    }

    fn update_selected_cue(&self) {
        let ivars = self.ivars();
        let row = ivars.active_cue.get();

        let id = ivars.cue_id.stringValue().to_string();
        let name = ivars.cue_name.stringValue().to_string();
        let path = PathBuf::from(ivars.field.stringValue().to_string());

        let duration = if helpers::is_audio_path(&path) {
            helpers::audio_duration(&path).unwrap_or(0.0)
        } else {
            0.0
        };

        let mut cues = ivars.cues.borrow_mut();

        if let Some(cue) = cues.get_mut(row) {
            cue.id = id;
            cue.name = name;
            cue.path = path;
            cue.duration = duration;
        }

        drop(cues);
        ivars.cue_table.reloadData();
    }

    fn load_selected_cue(&self) {
        let ivars = self.ivars();
        let row = ivars.active_cue.get();
        let cue = {
            let cues = ivars.cues.borrow();
            let Some(cue) = cues.get(row) else {
                return;
            };
            cue.clone()
        };

        ivars.active_cue.set(row);
        ivars.cue_id.setStringValue(&NSString::from_str(&cue.id));
        ivars
            .cue_name
            .setStringValue(&NSString::from_str(&cue.name));
        ivars
            .field
            .setStringValue(&NSString::from_str(&cue.path.to_string_lossy()));

        self.set_error(String::new());
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
