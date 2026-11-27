use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AutoContinue {
    None,
    Continue,
    Follow,
}

#[derive(Clone)]
pub struct Cue {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub duration: f64,
    pub effects: Vec<bool>,
    pub fade_target: Option<String>,
    pub stop_after_fade: bool,
    pub auto_continue: AutoContinue,
    pub gain: f32,
    pub gain_l: f32,
    pub gain_r: f32,
}

pub struct CueIvars {
    pub field: Retained<NSTextField>,
    pub cue_id: Retained<NSTextField>,
    pub cue_name: Retained<NSTextField>,
    pub error_label: Retained<NSTextField>,
    pub table: Retained<NSTableView>,
    pub cues: RefCell<Vec<Cue>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = CueIvars]
    #[name = "CueController"]
    pub struct CueController;

    unsafe impl NSObjectProtocol for CueController {}

    unsafe impl NSTableViewDataSource for CueController {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows_in_table_view(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().cues.borrow().len() as NSInteger
        }

        #[unsafe(method(tableView:objectValueForTableColumn:row:))]
        fn table_view_object_value(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> *mut NSObject {
            let cues = self.ivars().cues.borrow();
            let Some(cue) = cues.get(row as usize) else {
                return std::ptr::null_mut();
            };
            let text = match column.map(|c| c.identifier().to_string()).as_deref() {
                Some("id") => cue.id.clone(),
                Some("name") => cue.name.clone(),
                Some("duration") => format_duration(cue.duration),
                _ => return std::ptr::null_mut(),
            };
            Retained::autorelease_ptr(NSString::from_str(&text)) as *mut NSObject
        }
    }

    impl CueController {
        #[unsafe(method(onAddCue:))]
        fn on_add_cue(&self, _sender: Option<&AnyObject>) {
            self.add_cue();
        }

        #[unsafe(method(onRemoveCue:))]
        fn on_remove_cue(&self, _sender: Option<&AnyObject>) {
            self.remove_cue();
        }
    }
);

impl CueController {
    pub fn new(
        mtm: MainThreadMarker,
        field: Retained<NSTextField>,
        cue_id: Retained<NSTextField>,
        cue_name: Retained<NSTextField>,
        error_label: Retained<NSTextField>,
        table: Retained<NSTableView>,
    ) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(CueIvars {
            field,
            cue_id,
            cue_name,
            error_label,
            table,
            cues: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            this.ivars()
                .table
                .setDataSource(Some(ProtocolObject::from_ref(&*this)));
        }
        this
    }

    pub fn make_buttons(&self, mtm: MainThreadMarker) -> (Retained<NSButton>, Retained<NSButton>) {
        unsafe {
            (
                NSButton::buttonWithTitle_target_action(
                    ns_string!("Add Cue"),
                    Some(self),
                    Some(sel!(onAddCue:)),
                    mtm,
                ),
                NSButton::buttonWithTitle_target_action(
                    ns_string!("Remove Cue"),
                    Some(self),
                    Some(sel!(onRemoveCue:)),
                    mtm,
                ),
            )
        }
    }

    fn set_error(&self, message: String) {
        let label = &self.ivars().error_label;
        label.setStringValue(&NSString::from_str(&message));
        label.setAlphaValue(if message.is_empty() { 0.0 } else { 1.0 });
    }

    fn add_cue(&self) {
        let ivars = self.ivars();
        let id = ivars.cue_id.stringValue().to_string().trim().to_string();
        let name = ivars.cue_name.stringValue().to_string().trim().to_string();
        let path = PathBuf::from(ivars.field.stringValue().to_string().trim());

        if id.is_empty() {
            self.set_error("Cue ID is required".to_string());
            return;
        }
        if ivars.cues.borrow().iter().any(|c| c.id == id) {
            self.set_error(format!("Cue ID {id} already exists"));
            return;
        }
        if !helpers::is_audio_path(&path) {
            self.set_error(format!("Not a supported audio file: {}", path.display()));
            return;
        }
        let duration = match audio_duration(&path) {
            Ok(d) => d,
            Err(e) => {
                self.set_error(e.to_string());
                return;
            }
        };

        ivars.cues.borrow_mut().push(Cue {
            id,
            name,
            path,
            duration,
            effects: Vec::new(),
            fade_target: None,
            stop_after_fade: true,
            auto_continue: AutoContinue::None,
            gain: 1.0,
            gain_l: 1.0,
            gain_r: 1.0,
        });
        ivars.table.reloadData();
        ivars.cue_id.setStringValue(&NSString::new());
        ivars.cue_name.setStringValue(&NSString::new());
        self.set_error(String::new());
    }

    fn remove_cue(&self) {
        let ivars = self.ivars();
        let row = ivars.table.selectedRow();
        if row < 0 {
            return;
        }
        {
            let mut cues = ivars.cues.borrow_mut();
            if (row as usize) < cues.len() {
                cues.remove(row as usize);
            }
        }
        ivars.table.reloadData();
    }
}

pub fn audio_duration(path: &Path) -> anyhow::Result<f64> {
    let file = helpers::open_audio_file(path)?;
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
    table.setAllowsEmptySelection(false);
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
