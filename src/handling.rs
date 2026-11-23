use super::*;

#[repr(C)]
pub struct RawAudioUnit {
    _private: [u8; 0],
}

unsafe impl RefEncode for RawAudioUnit {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct(
        "ComponentInstanceRecord",
        &[Encoding::Array(1, &Encoding::LongLong)],
    ));
}

pub struct Installed {
    pub name: String,
    pub desc: AudioComponentDescription,
}

pub struct ChainItem {
    pub index: usize,
    pub node: Retained<AVAudioUnitEffect>,
}

pub struct Editor {
    #[allow(dead_code)]
    pub window: Retained<NSWindow>,
    #[allow(dead_code)]
    pub factory: Option<Retained<AnyObject>>,
}

pub struct AudioState {
    pub engine: Retained<AVAudioEngine>,
    pub player: Retained<AVAudioPlayerNode>,
    pub nodes: Vec<Retained<AVAudioUnitEffect>>,
    pub fade_mixer: Retained<AVAudioMixerNode>,
    pub matrix: Option<Retained<AVAudioUnit>>,
}

pub struct AppIvars {
    pub field: Retained<NSTextField>,
    pub error_label: Retained<NSTextField>,
    pub search: Retained<NSSearchField>,
    pub available_table: Retained<NSTableView>,
    pub chain_table: Retained<NSTableView>,
    pub installed: Vec<Installed>,
    pub filtered: RefCell<Vec<usize>>,
    pub chain: RefCell<Vec<ChainItem>>,
    pub editors: RefCell<Vec<Editor>>,
    pub audio: RefCell<Option<AudioState>>,
    pub fade: RefCell<Option<Retained<NSTimer>>>,
    pub gain: Cell<f32>,
    pub gain_l: Cell<f32>,
    pub gain_r: Cell<f32>,
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
            if !helpers::is_audio_path(&path) {
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

            match helpers::start_audio(&path, nodes) {
                Ok(state) => {
                    *self.ivars().audio.borrow_mut() = Some(state);
                    self.apply_gain();
                    self.apply_channel_gains();
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
            self.rewire();
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
    pub fn new(
        mtm: MainThreadMarker,
        field: Retained<NSTextField>,
        error_label: Retained<NSTextField>,
        search: Retained<NSSearchField>,
        available_table: Retained<NSTableView>,
        chain_table: Retained<NSTableView>,
    ) -> Retained<Self> {
        let installed = helpers::list_installed_effects();
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
            gain: Cell::new(1.0),
            gain_l: Cell::new(1.0),
            gain_r: Cell::new(1.0),
        });

        unsafe { msg_send![super(this), init] }
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

    pub fn set_gain_l(&self, db: f64) {
        self.ivars().gain_l.set(Self::db_to_lin(db));
        self.apply_channel_gains();
    }

    pub fn set_gain_r(&self, db: f64) {
        self.ivars().gain_r.set(Self::db_to_lin(db));
        self.apply_channel_gains();
    }

    fn apply_channel_gains(&self) {
        let l = self.ivars().gain_l.get();
        let r = self.ivars().gain_r.get();
        if let Some(state) = self.ivars().audio.borrow().as_ref() {
            if let Some(matrix) = state.matrix.as_ref() {
                helpers::set_matrix_gain(matrix, 0, 0, l);
                helpers::set_matrix_gain(matrix, 1, 1, r);
            }
        }
    }

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

    pub fn add_selected(&self) {
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
            self.rewire();
        }
    }

    pub fn edit_selected(&self) {
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

    pub fn open_editor(&self, node: &AVAudioUnitEffect, title: String) {
        let mtm = self.mtm();

        match helpers::cocoa_view(helpers::raw_audio_unit(node)) {
            Some((view, factory)) => {
                let window = helpers::editor_window(mtm, &view, &title);
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
                        helpers::raw_audio_unit(node) as AudioUnit,
                    )
                };

                let window = helpers::editor_window(mtm, &view, &title);

                self.ivars().editors.borrow_mut().push(Editor {
                    window,
                    factory: None,
                });

                self.set_error(String::new());
            }
        }
    }

    pub fn finish_stop(&self) {
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
                if let Some(matrix) = &state.matrix {
                    state.engine.detachNode(matrix);
                }
            }
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

    fn rewire(&self) {
        let nodes: Vec<Retained<AVAudioUnitEffect>> = self
            .ivars()
            .chain
            .borrow()
            .iter()
            .map(|c| c.node.clone())
            .collect();
        if let Some(state) = self.ivars().audio.borrow_mut().as_mut() {
            helpers::rewire(state, nodes);
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
