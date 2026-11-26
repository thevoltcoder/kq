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
    window.setDelegate(Some(ProtocolObject::from_ref(&*handler)));
    window.setTitle(ns_string!("kq"));
    window.setContentMinSize(NSSize::new(560.0, 420.0));

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
            Retained::into_super(Retained::into_super(browse_btn.clone())),
        ],
        NSStackViewDistribution::Fill,
        mtm,
    );
    browse_btn.setContentHuggingPriority_forOrientation(
        1000.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    browse_btn.setContentCompressionResistancePriority_forOrientation(
        1000.0,
        NSLayoutConstraintOrientation::Horizontal,
    );

    field.setContentCompressionResistancePriority_forOrientation(
        250.0,
        NSLayoutConstraintOrientation::Horizontal,
    );

    let button_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(play_btn)),
            Retained::into_super(Retained::into_super(stop_btn)),
        ],
        NSStackViewDistribution::FillEqually,
        mtm,
    );

    let toggle_row = make_stack(
        &[
            Retained::into_super(Retained::into_super(enable_btn)),
            Retained::into_super(Retained::into_super(disable_btn)),
            Retained::into_super(Retained::into_super(edit_btn)),
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

    file_row.setTranslatesAutoresizingMaskIntoConstraints(false);

    let cue_id = NSTextField::textFieldWithString(&NSString::new(), mtm);
    cue_id.setEditable(true);
    cue_id.setSelectable(true);
    cue_id.setPlaceholderString(Some(ns_string!("Cue ID")));
    cue_id.setTranslatesAutoresizingMaskIntoConstraints(false);

    let cue_name = NSTextField::textFieldWithString(&NSString::new(), mtm);
    cue_name.setEditable(true);
    cue_name.setSelectable(true);
    cue_name.setPlaceholderString(Some(ns_string!("Cue Name")));
    cue_name.setTranslatesAutoresizingMaskIntoConstraints(false);

    description_content.addSubview(&file_row);
    description_content.addSubview(&cue_id);
    description_content.addSubview(&cue_name);

    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        file_row
            .leadingAnchor()
            .constraintEqualToAnchor(&description_content.leadingAnchor()),
        file_row
            .widthAnchor()
            .constraintEqualToAnchor(&description_content.widthAnchor()),
        file_row
            .trailingAnchor()
            .constraintEqualToAnchor(&description_content.trailingAnchor()),
        file_row
            .topAnchor()
            .constraintEqualToAnchor(&description_content.topAnchor()),
        cue_id
            .leadingAnchor()
            .constraintEqualToAnchor(&description_content.leadingAnchor()),
        cue_id
            .topAnchor()
            .constraintEqualToAnchor_constant(&file_row.bottomAnchor(), 12.0),
        cue_name
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&cue_id.trailingAnchor(), 8.0),
        cue_name
            .trailingAnchor()
            .constraintEqualToAnchor(&description_content.trailingAnchor()),
        cue_name
            .topAnchor()
            .constraintEqualToAnchor(&cue_id.topAnchor()),
        cue_id
            .widthAnchor()
            .constraintEqualToAnchor_multiplier(&cue_name.widthAnchor(), 0.2),
        browse_btn.widthAnchor().constraintEqualToConstant(90.0),
    ]));

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

    let background: Retained<BackgroundView> =
        unsafe { msg_send![mtm.alloc::<BackgroundView>(), init] };

    window.setContentView(Some(&background));

    let content: &NSView = &background;

    for v in [
        button_row.as_ref() as &NSView,
        tabs.as_ref() as &NSView,
        error_label.as_ref() as &NSView,
    ] {
        v.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(v);
    }

    button_row
        .setContentHuggingPriority_forOrientation(1000.0, NSLayoutConstraintOrientation::Vertical);
    button_row.setContentCompressionResistancePriority_forOrientation(
        1000.0,
        NSLayoutConstraintOrientation::Vertical,
    );

    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        button_row
            .topAnchor()
            .constraintEqualToAnchor_constant(&content.topAnchor(), 20.0),
        button_row
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&content.leadingAnchor(), 20.0),
        button_row
            .trailingAnchor()
            .constraintEqualToAnchor_constant(&content.trailingAnchor(), -20.0),
        error_label
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&content.leadingAnchor(), 20.0),
        error_label
            .trailingAnchor()
            .constraintEqualToAnchor_constant(&content.trailingAnchor(), -20.0),
        error_label
            .bottomAnchor()
            .constraintEqualToAnchor_constant(&content.bottomAnchor(), -20.0),
        error_label.heightAnchor().constraintEqualToConstant(36.0),
        tabs.leadingAnchor()
            .constraintEqualToAnchor_constant(&content.leadingAnchor(), 20.0),
        tabs.trailingAnchor()
            .constraintEqualToAnchor_constant(&content.trailingAnchor(), -20.0),
        tabs.bottomAnchor()
            .constraintEqualToAnchor_constant(&error_label.topAnchor(), -14.0),
        tabs.heightAnchor().constraintEqualToConstant(240.0),
        tabs.topAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(&button_row.bottomAnchor(), 14.0),
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
