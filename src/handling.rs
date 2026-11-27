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

fn cue_key(cue: &cue::Cue) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cue.id.hash(&mut hasher);
    cue.path.hash(&mut hasher);
    hasher.finish() as usize
}

pub struct GainControls {
    pub gain_slider: Retained<NSSlider>,
    pub gain_field: Retained<NSTextField>,
    pub gain_l_slider: Retained<NSSlider>,
    pub gain_l_field: Retained<NSTextField>,
    pub gain_r_slider: Retained<NSSlider>,
    pub gain_r_field: Retained<NSTextField>,
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
    pub loading: RefCell<Vec<bool>>,
    pub effect_tx: std::sync::mpsc::Sender<(usize, Result<usize, String>)>,
    pub effect_rx: RefCell<std::sync::mpsc::Receiver<(usize, Result<usize, String>)>>,
    pub pending_playback: RefCell<VecDeque<(PathBuf, i64, usize)>>,
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
    pub chain: RefCell<Vec<bool>>,
    pub playing_cue: Cell<Option<usize>>,
    pub cue_duration: Retained<NSTextField>,
    pub fade_target: Retained<NSTextField>,
    pub stop_after_fade: Retained<NSPopUpButton>,
    pub auto_continue: Retained<NSPopUpButton>,
    pub gain_controls: RefCell<Option<GainControls>>,
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
            self.update_selected_cue();

            let ivars = self.ivars();
            let index = ivars.active_cue.get();

            let Some(cue) = ivars.cues.borrow().get(index).cloned() else {
                return;
            };

            if let Some(target) = &cue.fade_target {
                if ivars.fade.borrow().is_some() {
                    self.set_error("A fade is already in progress".to_string());
                    return;
                }

                let target_index = {
                    let cues = ivars.cues.borrow();
                    cues.iter().position(|c| c.id == *target)
                };

                let Some(target_index) = target_index else {
                    self.set_error(format!("Fade target {target} does not exist"));
                    return;
                };

                if ivars.playing_cue.get() != Some(target_index)
                    || ivars.audio.borrow().is_none()
                {
                    self.set_error(format!("Fade target {target} is not currently playing"));
                    return;
                }

                self.begin_fade_stop(
                    cue.duration,
                    cue.stop_after_fade,
                    cue.gain,
                    cue.gain_l,
                    cue.gain_r,
                );
            } else {
                if !helpers::is_audio_path(&cue.path) {
                    self.set_error(format!("Cue {} has no valid audio file", index + 1));
                    return;
                }

                let key = cue_key(&cue);

                let queued = ivars
                    .pending_playback
                    .borrow()
                    .iter()
                    .any(|(_, _, k)| *k == key);

                let live = ivars
                    .audio
                    .borrow()
                    .as_ref()
                    .map_or(false, |s| s.is_playing_key(key));

                if !queued && !live {
                    ivars.playing_cue.set(Some(index));
                    self.start_playback(&cue.path, 0, &cue.effects, key);
                }
            }

            let len = ivars.cues.borrow().len();
            let next = (index + 1).min(len.saturating_sub(1));

            ivars.active_cue.set(next);

            let selected = NSIndexSet::indexSetWithIndex(next);
            ivars.cue_table.selectRowIndexes_byExtendingSelection(&selected, false);
            self.load_selected_cue();
        }

        #[unsafe(method(onStop:))]
        fn on_stop(&self, _sender: Option<&AnyObject>) {
            self.begin_fade_stop(FADE_SECS as f64, true, 0.0, 0.0, 0.0);
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
            let ivars = self.ivars();
            let row = ivars.cue_table.selectedRow();
            if row < 0 {
                return;
            }
            self.update_selected_cue();
            ivars.active_cue.set(row as usize);
            self.load_selected_cue();
        }

        #[unsafe(method(onAddFadeCue:))]
        fn on_add_fade_cue(&self, _sender: Option<&AnyObject>) {
            self.add_fade_cue();
        }

        #[unsafe(method(onCueOptionsChanged:))]
        fn on_cue_options_changed(&self, _sender: Option<&AnyObject>) {
            self.update_selected_cue();
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
        cue_duration: Retained<NSTextField>,
        fade_target: Retained<NSTextField>,
        stop_after_fade: Retained<NSPopUpButton>,
        auto_continue: Retained<NSPopUpButton>,
        cue_table: Retained<NSTableView>,
    ) -> Retained<Self> {
        let installed = helpers::list_installed_effects();
        let n = installed.len();
        let filtered = (0..n).collect();
        let enabled = vec![false; n];
        let nodes = (0..n).map(|_| None).collect();
        let loading = vec![false; n];

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
            gain_controls: RefCell::new(None),
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
                effects: vec![false; n],
                fade_target: None,
                stop_after_fade: true,
                auto_continue: cue::AutoContinue::None,
                gain: 1.0,
                gain_l: 1.0,
                gain_r: 1.0,
            }]),
            active_cue: Cell::new(0),
            initial_cue_selection: Cell::new(false),
            chain: RefCell::new(vec![false; n]),
            playing_cue: Cell::new(None),
            cue_duration,
            fade_target,
            stop_after_fade,
            auto_continue,
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

    pub fn set_gain_controls(
        &self,
        gain_slider: Retained<NSSlider>,
        gain_field: Retained<NSTextField>,
        gain_l_slider: Retained<NSSlider>,
        gain_l_field: Retained<NSTextField>,
        gain_r_slider: Retained<NSSlider>,
        gain_r_field: Retained<NSTextField>,
    ) {
        *self.ivars().gain_controls.borrow_mut() = Some(GainControls {
            gain_slider,
            gain_field,
            gain_l_slider,
            gain_l_field,
            gain_r_slider,
            gain_r_field,
        });

        self.load_cue_levels(self.ivars().active_cue.get());
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

    fn lin_to_db(gain: f32) -> f64 {
        if gain <= 0.0001 {
            -80.0
        } else {
            (20.0 * gain.log10()).clamp(-80.0, 24.0) as f64
        }
    }

    pub fn set_gain(&self, db: f64) {
        self.set_cue_gain(db, 0);
    }

    pub fn set_gain_l(&self, db: f64) {
        self.set_cue_gain(db, 1);
    }

    pub fn set_gain_r(&self, db: f64) {
        self.set_cue_gain(db, 2);
    }

    fn set_cue_gain(&self, db: f64, channel: usize) {
        let ivars = self.ivars();
        let index = ivars.active_cue.get();

        {
            let mut cues = ivars.cues.borrow_mut();
            let Some(cue) = cues.get_mut(index) else {
                return;
            };

            match channel {
                0 => cue.gain = Self::db_to_lin(db),
                1 => cue.gain_l = Self::db_to_lin(db),
                _ => cue.gain_r = Self::db_to_lin(db),
            }
        }

        if ivars.playing_cue.get() == Some(index) {
            self.apply_gain_for(index);
        }
    }

    fn apply_gain_for(&self, index: usize) {
        let ivars = self.ivars();

        let Some((gain, gain_l, gain_r)) = ivars
            .cues
            .borrow()
            .get(index)
            .map(|cue| (cue.gain, cue.gain_l, cue.gain_r))
        else {
            return;
        };

        if let Some(state) = ivars.audio.borrow().as_ref() {
            state
                .sh
                .gain
                .store(gain.to_bits(), std::sync::atomic::Ordering::Relaxed);
            state
                .sh
                .gain_l
                .store(gain_l.to_bits(), std::sync::atomic::Ordering::Relaxed);
            state
                .sh
                .gain_r
                .store(gain_r.to_bits(), std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn load_cue_levels(&self, index: usize) {
        let ivars = self.ivars();

        let Some((gain, gain_l, gain_r)) = ivars
            .cues
            .borrow()
            .get(index)
            .map(|cue| (cue.gain, cue.gain_l, cue.gain_r))
        else {
            return;
        };

        let controls = ivars.gain_controls.borrow();
        let Some(controls) = controls.as_ref() else {
            return;
        };

        for (value, slider, field) in [
            (gain, &controls.gain_slider, &controls.gain_field),
            (gain_l, &controls.gain_l_slider, &controls.gain_l_field),
            (gain_r, &controls.gain_r_slider, &controls.gain_r_field),
        ] {
            let db = Self::lin_to_db(value);
            slider.setDoubleValue(db);
            field.setStringValue(&NSString::from_str(&format!("{db:.1} dB")));
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

        self.ivars().playing_cue.set(None);
    }

    fn begin_fade_stop(
        &self,
        duration: f64,
        stop_after: bool,
        target_gain: f32,
        target_gain_l: f32,
        target_gain_r: f32,
    ) {
        let ivars = self.ivars();

        let Some(sh) = ivars.audio.borrow().as_ref().map(|s| s.sh.clone()) else {
            return;
        };

        if let Some(timer) = ivars.fade.borrow_mut().take() {
            timer.invalidate();
        }

        let ordering = std::sync::atomic::Ordering::Relaxed;

        let start_gain = f32::from_bits(sh.gain.load(ordering));
        let start_gain_l = f32::from_bits(sh.gain_l.load(ordering));
        let start_gain_r = f32::from_bits(sh.gain_r.load(ordering));

        sh.set_fade(1.0);

        let started = std::time::Instant::now();
        let duration = duration.max(0.0);
        let this = self.retain();

        let block = RcBlock::new(move |timer: NonNull<NSTimer>| {
            let progress = if duration == 0.0 {
                1.0
            } else {
                (started.elapsed().as_secs_f64() / duration).clamp(0.0, 1.0) as f32
            };

            sh.set_gain(start_gain + (target_gain - start_gain) * progress);
            sh.set_gain_l(start_gain_l + (target_gain_l - start_gain_l) * progress);
            sh.set_gain_r(start_gain_r + (target_gain_r - start_gain_r) * progress);

            if progress >= 1.0 {
                sh.set_gain(target_gain);
                sh.set_gain_l(target_gain_l);
                sh.set_gain_r(target_gain_r);

                unsafe {
                    timer.as_ref().invalidate();
                }

                if stop_after {
                    this.finish_stop();
                } else {
                    this.ivars().fade.borrow_mut().take();
                }
            }
        });

        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(FADE_TICK_SECS, true, &block)
        };

        *ivars.fade.borrow_mut() = Some(timer);
    }

    fn try_start_pending_playback(&self) {
        let ivars = self.ivars();

        let Some((path, frame, key)) = ivars.pending_playback.borrow_mut().pop_front() else {
            return;
        };

        if ivars.audio.borrow().is_none() {
            match rt::AudioState::new(ivars.installed.len()) {
                Ok(a) => {
                    *ivars.audio.borrow_mut() = Some(a);
                    self.sync_chain();
                }
                Err(e) => {
                    self.set_error(e);
                    return;
                }
            }
        }

        let enabled = ivars.chain.borrow().clone();

        for (index, on) in enabled.iter().enumerate() {
            if *on && ivars.nodes.borrow()[index].is_none() {
                self.request_node(index);
                ivars
                    .pending_playback
                    .borrow_mut()
                    .push_front((path, frame, key));
                return;
            }
        }

        let cue_index = {
            let cues = ivars.cues.borrow();
            cues.iter().position(|cue| cue_key(cue) == key)
        };

        let started = {
            let audio = ivars.audio.borrow();
            let Some(state) = audio.as_ref() else {
                return;
            };

            state.set_next_fx(&enabled);
            state.set_next_key(key);

            match helpers::play_audio(state, &path, frame) {
                Ok(()) => {
                    if let Some(index) = cue_index {
                        self.apply_gain_for(index);
                    }
                    self.set_error(String::new());
                    true
                }
                Err(e) => {
                    self.set_error(e.to_string());
                    false
                }
            }
        };

        if started {
            if let Some(index) = cue_index {
                ivars.playing_cue.set(Some(index));

                if ivars.cues.borrow()[index].auto_continue == cue::AutoContinue::Continue {
                    self.start_next_cue(index);
                }
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
        let active = ivars.active_cue.get();
        if let Some(cue) = ivars.cues.borrow_mut().get_mut(active) {
            if index < cue.effects.len() {
                cue.effects[index] = on;
            }
        }
        if on {
            self.request_node(index);
        }
        if ivars.playing_cue.get() == Some(active) {
            ivars.chain.borrow_mut()[index] = on;
            if let Some(state) = ivars.audio.borrow().as_ref() {
                state.set_last_fx(index, on);
            }
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

    fn start_playback(&self, path: &Path, frame: i64, effects: &[bool], key: usize) {
        if let Some(timer) = self.ivars().fade.borrow_mut().take() {
            timer.invalidate();
        }
        if let Some(state) = self.ivars().audio.borrow().as_ref() {
            state.sh.set_fade(1.0);
        }
        let mut chain = effects.to_vec();
        chain.resize(self.ivars().installed.len(), false);
        *self.ivars().chain.borrow_mut() = chain;
        self.ivars()
            .pending_playback
            .borrow_mut()
            .push_back((path.to_path_buf(), frame, key));
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

        while let Some(key) = self
            .ivars()
            .audio
            .borrow()
            .as_ref()
            .and_then(|state| state.take_finished())
        {
            self.cue_finished(key);
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
        let enabled = ivars.chain.borrow();
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
            effects: vec![false; ivars.installed.len()],
            fade_target: None,
            stop_after_fade: true,
            auto_continue: cue::AutoContinue::None,
            gain: 1.0,
            gain_l: 1.0,
            gain_r: 1.0,
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
        ivars.cue_duration.setStringValue(&NSString::new());
        ivars.fade_target.setStringValue(&NSString::new());

        ivars.field.setEnabled(true);
        ivars.field.setEditable(true);
        ivars.cue_duration.setEnabled(false);
        ivars.cue_duration.setEditable(false);
        ivars.fade_target.setEnabled(false);
        ivars.fade_target.setEditable(false);

        *ivars.enabled.borrow_mut() = vec![false; ivars.installed.len()];
        ivars.available_table.reloadData();

        self.load_cue_levels(index);
        self.set_error(String::new());
    }

    fn add_fade_cue(&self) {
        let ivars = self.ivars();
        let mut cues = ivars.cues.borrow_mut();

        cues.push(cue::Cue {
            id: String::new(),
            name: String::new(),
            path: PathBuf::new(),
            duration: 0.0,
            effects: vec![false; ivars.installed.len()],
            fade_target: Some(String::new()),
            stop_after_fade: true,
            auto_continue: cue::AutoContinue::None,
            gain: 1.0,
            gain_l: 1.0,
            gain_r: 1.0,
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
        ivars.cue_duration.setStringValue(&NSString::new());
        ivars.fade_target.setStringValue(&NSString::new());

        ivars.field.setEnabled(false);
        ivars.field.setEditable(false);
        ivars.cue_duration.setEnabled(true);
        ivars.cue_duration.setEditable(true);
        ivars.fade_target.setEnabled(true);
        ivars.fade_target.setEditable(true);

        *ivars.enabled.borrow_mut() = vec![false; ivars.installed.len()];
        ivars.available_table.reloadData();

        self.load_cue_levels(index);
        self.set_error(String::new());
    }

    fn remove_cue(&self) {
        let ivars = self.ivars();
        self.update_selected_cue();

        let mut cues = ivars.cues.borrow_mut();
        if cues.len() <= 1 {
            self.set_error("You must always have at least one cue".to_string());
            return;
        }

        let row = ivars.active_cue.get();
        if row >= cues.len() {
            return;
        }

        cues.remove(row);
        if let Some(p) = ivars.playing_cue.get() {
            if p == row {
                ivars.playing_cue.set(None);
            } else if p > row {
                ivars.playing_cue.set(Some(p - 1));
            }
        }
        let next_active = row.min(cues.len() - 1);
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
        let row = ivars.cue_table.selectedRow();

        if row < 0 {
            self.set_error("Select a cue first".to_string());
            return;
        }

        let row = row as usize;
        let id = ivars.cue_id.stringValue().to_string().trim().to_string();
        let name = ivars.cue_name.stringValue().to_string().trim().to_string();

        if id.is_empty() {
            self.set_error("Cue ID is required".to_string());
            return;
        }

        let (is_fade, effects, gain, gain_l, gain_r) = {
            let cues = ivars.cues.borrow();

            let Some(cue) = cues.get(row) else {
                return;
            };

            (
                cue.fade_target.is_some(),
                cue.effects.clone(),
                cue.gain,
                cue.gain_l,
                cue.gain_r,
            )
        };

        let (path, duration, fade_target) = if is_fade {
            let target = ivars
                .fade_target
                .stringValue()
                .to_string()
                .trim()
                .to_string();

            if target.is_empty() {
                self.set_error("Fade target is required".to_string());
                return;
            }

            if target == id {
                self.set_error("A fade cue cannot target itself".to_string());
                return;
            }

            let duration_text = ivars.cue_duration.stringValue().to_string();

            let duration = match duration_text.trim().parse::<f64>() {
                Ok(value) if value.is_finite() && value >= 0.0 => value,
                _ => {
                    self.set_error("Enter a valid fade duration in seconds".to_string());
                    return;
                }
            };

            let cues = ivars.cues.borrow();

            if !cues
                .iter()
                .enumerate()
                .any(|(i, cue)| i != row && cue.id == target)
            {
                self.set_error(format!("Fade target {target} does not exist"));
                return;
            }

            (PathBuf::new(), duration, Some(target))
        } else {
            let path = PathBuf::from(ivars.field.stringValue().to_string().trim());

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

            (path, duration, None)
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

        let stop_after_fade = ivars.stop_after_fade.indexOfSelectedItem() == 0;
        let auto_continue = match ivars.auto_continue.indexOfSelectedItem() {
            1 => cue::AutoContinue::Continue,
            2 => cue::AutoContinue::Follow,
            _ => cue::AutoContinue::None,
        };

        cues[row] = cue::Cue {
            id,
            name,
            path,
            duration,
            effects,
            fade_target,
            stop_after_fade,
            auto_continue,
            gain,
            gain_l,
            gain_r,
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
        let auto_continue = match ivars.auto_continue.indexOfSelectedItem() {
            1 => cue::AutoContinue::Continue,
            2 => cue::AutoContinue::Follow,
            _ => cue::AutoContinue::None,
        };
        let is_fade = ivars
            .cues
            .borrow()
            .get(row)
            .is_some_and(|cue| cue.fade_target.is_some());
        let mut cues = ivars.cues.borrow_mut();
        if let Some(cue) = cues.get_mut(row) {
            cue.id = id;
            cue.name = name;
            cue.auto_continue = auto_continue;
            if is_fade {
                cue.fade_target = Some(
                    ivars
                        .fade_target
                        .stringValue()
                        .to_string()
                        .trim()
                        .to_string(),
                );
                cue.duration = ivars
                    .cue_duration
                    .stringValue()
                    .to_string()
                    .trim()
                    .parse::<f64>()
                    .unwrap_or(0.0);
                cue.stop_after_fade = ivars.stop_after_fade.indexOfSelectedItem() == 0;
            } else {
                let path = PathBuf::from(ivars.field.stringValue().to_string());
                cue.duration = if helpers::is_audio_path(&path) {
                    helpers::audio_duration(&path).unwrap_or(0.0)
                } else {
                    0.0
                };
                cue.path = path;
            }
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

        let is_fade = cue.fade_target.is_some();
        ivars.stop_after_fade.setEnabled(is_fade);

        ivars.active_cue.set(row);
        ivars.cue_id.setStringValue(&NSString::from_str(&cue.id));
        ivars
            .cue_name
            .setStringValue(&NSString::from_str(&cue.name));
        ivars
            .field
            .setStringValue(&NSString::from_str(&cue.path.to_string_lossy()));

        let dur_str = cue.duration.to_string();
        ivars
            .cue_duration
            .setStringValue(&NSString::from_str(if is_fade { &dur_str } else { "" }));

        ivars.fade_target.setStringValue(&NSString::from_str(
            cue.fade_target.as_deref().unwrap_or(""),
        ));

        ivars.field.setEnabled(!is_fade);
        ivars.field.setEditable(!is_fade);

        ivars.cue_duration.setEnabled(is_fade);
        ivars.cue_duration.setEditable(is_fade);

        ivars.fade_target.setEnabled(is_fade);
        ivars.fade_target.setEditable(is_fade);

        let mut fx = cue.effects.clone();
        fx.resize(ivars.installed.len(), false);
        *ivars.enabled.borrow_mut() = fx;

        let sel = ivars.available_table.selectedRow();
        ivars.available_table.reloadData();

        if sel >= 0 {
            ivars.available_table.selectRowIndexes_byExtendingSelection(
                &NSIndexSet::indexSetWithIndex(sel as usize),
                false,
            );
        }

        ivars
            .stop_after_fade
            .selectItemAtIndex(if cue.stop_after_fade { 0 } else { 1 });

        let auto_index = match cue.auto_continue {
            cue::AutoContinue::None => 0,
            cue::AutoContinue::Continue => 1,
            cue::AutoContinue::Follow => 2,
        };

        ivars.auto_continue.selectItemAtIndex(auto_index);

        self.load_cue_levels(row);

        self.set_error(String::new());
    }

    fn start_next_cue(&self, index: usize) {
        let ivars = self.ivars();
        let next = index + 1;

        let Some(cue) = ivars.cues.borrow().get(next).cloned() else {
            return;
        };

        if !helpers::is_audio_path(&cue.path) {
            self.set_error(format!("Cue {} has no valid audio file", next + 1));
            return;
        }

        ivars.active_cue.set(next);

        let selected = NSIndexSet::indexSetWithIndex(next);
        ivars
            .cue_table
            .selectRowIndexes_byExtendingSelection(&selected, false);
        ivars.cue_table.scrollRowToVisible(next as NSInteger);

        self.load_selected_cue();

        let key = cue_key(&cue);
        ivars.playing_cue.set(Some(next));
        self.start_playback(&cue.path, 0, &cue.effects, key);
    }

    fn cue_finished(&self, key: usize) {
        let ivars = self.ivars();

        let index = {
            let cues = ivars.cues.borrow();
            cues.iter().position(|cue| cue_key(cue) == key)
        };

        let Some(index) = index else {
            return;
        };

        if ivars.playing_cue.get() == Some(index) {
            ivars.playing_cue.set(None);
        }

        self.start_next_cue(index);
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
