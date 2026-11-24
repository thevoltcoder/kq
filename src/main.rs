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
    field.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
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

    let error_label = make_error_label(mtm);
    let handler = AppHandler::new(
        mtm,
        field.clone(),
        error_label.clone(),
        search.clone(),
        available_table.clone(),
    );

    app.setDelegate(Some(ProtocolObject::from_ref(&*handler)));
    unsafe {
        available_table.setDataSource(Some(ProtocolObject::from_ref(&*handler)));
        available_table.setTarget(Some(&handler));
        available_table.setAction(Some(sel!(onSelect:)));
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
    let edit_btn = make_button(ns_string!("Edit"), &handler, sel!(onEdit:), mtm);
    let enable_btn = make_button(ns_string!("Enable"), &handler, sel!(onEnable:), mtm);
    let disable_btn = make_button(ns_string!("Disable"), &handler, sel!(onDisable:), mtm);

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

    let button_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(play_btn)),
            Retained::into_super(Retained::into_super(stop_btn)),
            Retained::into_super(Retained::into_super(edit_btn)),
        ],
        NSStackViewDistribution::FillEqually,
        mtm,
    );

    let toggle_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(enable_btn)),
            Retained::into_super(Retained::into_super(disable_btn)),
        ],
        NSStackViewDistribution::FillEqually,
        mtm,
    );

    let left_col = make_stack(
        &[
            Retained::into_super(Retained::into_super(Retained::into_super(search.clone()))),
            Retained::into_super(available_scroll.clone()),
            Retained::into_super(toggle_row),
        ],
        NSStackViewDistribution::Fill,
        mtm,
    );
    left_col.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    left_col.setAlignment(NSLayoutAttribute::Width);

    let levels_content = NSView::new(mtm);
    levels_content.setTranslatesAutoresizingMaskIntoConstraints(false);

    let gain_handler = handler.clone();
    let (gain_box, _gain_slider, _gain_field, _fs_gain) = helpers::make_faderslider(
        mtm,
        "Gain",
        Some(Box::new(move |db| gain_handler.set_gain(db))),
    );

    let hl = handler.clone();
    let (l_box, _l_slider, _l_field, _fs_l) =
        helpers::make_faderslider(mtm, "L", Some(Box::new(move |db| hl.set_gain_l(db))));

    let hr = handler.clone();
    let (r_box, _r_slider, _r_field, _fs_r) =
        helpers::make_faderslider(mtm, "R", Some(Box::new(move |db| hr.set_gain_r(db))));

    let faders_row = make_stack(
        &[gain_box, l_box, r_box],
        NSStackViewDistribution::Fill,
        mtm,
    );
    faders_row.setSpacing(16.0);
    faders_row.setTranslatesAutoresizingMaskIntoConstraints(false);
    levels_content.addSubview(&faders_row);

    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        faders_row
            .leadingAnchor()
            .constraintEqualToAnchor(&levels_content.leadingAnchor()),
        faders_row
            .topAnchor()
            .constraintEqualToAnchor(&levels_content.topAnchor()),
    ]));

    let description_content = NSView::new(mtm);
    description_content.setTranslatesAutoresizingMaskIntoConstraints(false);

    let levels_tab_content = padded_tab_content(&levels_content, 16.0, mtm);
    let description_tab_content = padded_tab_content(&description_content, 16.0, mtm);
    let filters_tab_content = padded_tab_content(&left_col, 16.0, mtm);

    let description_tab = NSTabViewItem::new();
    description_tab.setLabel(ns_string!("Info"));
    description_tab.setView(Some(&description_tab_content));

    let levels_tab = NSTabViewItem::new();
    levels_tab.setLabel(ns_string!("Levels"));
    levels_tab.setView(Some(&levels_tab_content));

    let filters_tab = NSTabViewItem::new();
    filters_tab.setLabel(ns_string!("Effects"));
    filters_tab.setView(Some(&filters_tab_content));

    let tabs = NSTabView::new(mtm);
    tabs.addTabViewItem(&description_tab);
    tabs.addTabViewItem(&levels_tab);
    tabs.addTabViewItem(&filters_tab);
    tabs.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);

    error_label
        .setContentHuggingPriority_forOrientation(750.0, NSLayoutConstraintOrientation::Vertical);

    let root = make_stack(
        &[
            Retained::into_super(file_row.clone()),
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
        root.leadingAnchor()
            .constraintEqualToAnchor_constant(&content.leadingAnchor(), 20.0),
        root.trailingAnchor()
            .constraintEqualToAnchor_constant(&content.trailingAnchor(), -20.0),
        root.topAnchor()
            .constraintEqualToAnchor_constant(&content.topAnchor(), 20.0),
        root.bottomAnchor()
            .constraintEqualToAnchor_constant(&content.bottomAnchor(), -20.0),
        file_row
            .widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        button_row
            .widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        tabs.widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        error_label
            .widthAnchor()
            .constraintEqualToAnchor(&root.widthAnchor()),
        error_label.heightAnchor().constraintEqualToConstant(36.0),
    ]));

    content.layoutSubtreeIfNeeded();
    tabs.layoutSubtreeIfNeeded();

    window.center();
    window.makeKeyAndOrderFront(None);

    content.layoutSubtreeIfNeeded();
    tabs.layoutSubtreeIfNeeded();

    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
}
