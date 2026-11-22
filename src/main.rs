use kq::{handling::*, helpers::*, *};

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
