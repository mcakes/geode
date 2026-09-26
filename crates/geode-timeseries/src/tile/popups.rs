//! Lifecycle and input routing for the series list, add picker, expression
//! field, range editor, and action menu. `crate::popup` owns state and popup
//! painting; the expression field is rendered inline by `crate::header`.

use super::*;

impl TimeseriesTile {
    /// Route popup actions. Editing a source slot refuses with a notice because
    /// only expression slots have expression text to edit.
    pub(super) fn popup_verb(
        &mut self,
        verb: &str,
        n: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let series_open = matches!(self.popup, Some(Popup::Series(_)));
        let menu_open = matches!(self.popup, Some(Popup::Menu(_)));
        match verb {
            // Keyboard and header button share the action-menu toggle.
            "menu" => {
                self.toggle_menu(window, cx);
                true
            }
            // Menu navigation clamps over action rows, including disabled rows, and
            // skips headings/separators. Enter picks; Escape closes. Series navigation
            // uses these action names in its own mutually exclusive context.
            "list_down" | "list_up" if menu_open => {
                let delta = if verb == "list_down" {
                    n as isize
                } else {
                    -(n as isize)
                };
                if let Some(Popup::Menu(m)) = &mut self.popup {
                    m.highlighted = menu::step(&m.rows, m.highlighted, delta);
                }
                cx.notify();
                true
            }
            "list_close" if menu_open => {
                self.close_popup_with_window(window, cx);
                true
            }
            "menu_pick" if menu_open => {
                let Some(Popup::Menu(m)) = &self.popup else {
                    return false;
                };
                let index = m.highlighted;
                self.menu_pick(index, window, cx);
                true
            }
            // Toggle the fieldless series list.
            "list" => {
                if self.popup.is_some() {
                    self.close_popup_with_window(window, cx);
                } else {
                    self.open_series_popup(cx);
                }
                true
            }
            // List and header share the model cursor, including wrapping navigation.
            // Guard these actions because palette dispatch can bypass keymap context.
            "list_down" if series_open => {
                let changed = self.model.cursor_next(n);
                self.apply_changed(changed, cx);
                true
            }
            "list_up" if series_open => {
                let changed = self.model.cursor_prev(n);
                self.apply_changed(changed, cx);
                true
            }
            "list_close" if series_open => {
                self.close_popup_with_window(window, cx);
                true
            }
            // Opening an editor replaces the current popup.
            "add" => {
                self.open_picker(window, cx);
                true
            }
            "expr" => {
                self.open_expr(None, window, cx);
                true
            }
            // Range opens a fresh draft. Dispatch closes an existing insert popup
            // first, so repeating this action reopens rather than toggling it shut.
            "range" => {
                self.open_range(window, cx);
                true
            }
            "edit" => {
                if let Some(number) = header::cursor_is_source(&self.model) {
                    self.notice = Some(format!("s{number} is not an expression").into());
                    cx.notify();
                    return false;
                }
                // `e` is `x` prefilled: the cursor's own expression, with
                // its number carried so a commit REPLACES rather than
                // adds (and so `resolve` excludes it from what the text
                // may reference).
                let Some(slot) = self.model.cursor_slot() else {
                    return false;
                };
                let seed = (
                    slot.number,
                    slot.text.clone().unwrap_or_default().to_string(),
                );
                self.open_expr(Some(seed), window, cx);
                true
            }
            "commit" => match &self.popup {
                Some(Popup::Picker(_)) => self.commit_picker(window, cx),
                Some(Popup::Expr(_)) => self.commit_expr(window, cx),
                Some(Popup::Range(_)) => self.commit_range(window, cx),
                _ => false,
            },
            "cancel" if self.popup.as_ref().is_some_and(Popup::is_insert) => {
                self.close_popup_with_window(window, cx);
                true
            }
            // Range arrows step the active date segment through the same operation
            // as its focused listener. Picker arrows instead clamp list navigation.
            "insert_up" | "insert_down" if matches!(self.popup, Some(Popup::Range(_))) => {
                let delta = if verb == "insert_up" {
                    n as i64
                } else {
                    -(n as i64)
                };
                self.apply_range_key(FieldKey::Step(delta), cx);
                true
            }
            "insert_up" | "insert_down" => match &mut self.popup {
                Some(Popup::Picker(p)) => {
                    let delta = if verb == "insert_up" {
                        -(n as i64)
                    } else {
                        n as i64
                    };
                    p.list.nav_clamped(NavCommand::Move(delta));
                    cx.notify();
                    true
                }
                // The expression field has no list to step; the keys are
                // CONSUMED rather than passed on, so an arrow cannot pan
                // the chart behind an open field.
                Some(Popup::Expr(_)) => true,
                _ => false,
            },
            _ => false,
        }
    }

    /// Open the fieldless series list and prepare its rows immediately.
    /// An empty model produces the list's empty state.
    pub(super) fn open_series_popup(&mut self, cx: &mut Context<Self>) {
        self.popup = Some(Popup::Series(SeriesPopup::default()));
        self.rebuild_chrome(cx);
        cx.notify();
    }

    // Add picker.

    /// Open focused identity typeahead over catalogued `identity@source` pairs.
    /// Request a catalogue when none has identities; the Diagnostics observer
    /// incorporates changed options while the identity stage remains open.
    pub(super) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        self.request_catalog(cx);
        let options = self.catalog_options(cx);
        let loaded = self.loaded_marks(&options);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("identity@source"));
        cx.subscribe_in(&input, window, |this, input, event, _window, cx| {
            // User edits update ranking here. Commit also reads live Input text
            // because programmatic `set_value` emits no Change event.
            if let InputEvent::Change = event {
                let query = input.read(cx).value().to_string();
                if let Some(Popup::Picker(p)) = &mut this.popup {
                    p.list.set_query(&query);
                    p.refresh_add_row();
                }
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.popup = Some(Popup::Picker(PickerState::new(input, options, loaded)));
        self.notice = None;
        cx.notify();
    }

    /// Request and notify together when the catalogue has no identities.
    /// `Diagnostics::request_catalog` alone does not wake its observer to drain
    /// the request.
    pub(super) fn request_catalog(&self, cx: &mut Context<Self>) {
        let have = self
            .diagnostics
            .read(cx)
            .catalog
            .as_ref()
            .is_some_and(|c| !c.identities.is_empty());
        if have {
            return;
        }
        self.diagnostics.update(cx, |d, cx| {
            d.request_catalog();
            cx.notify();
        });
    }

    /// Every `identity@source` the picker ranks: the catalogue's own
    /// rows, restricted to sources this build actually has configured
    /// (a catalogue outlives a `sources` edit), sorted so the list is
    /// stable across deliveries.
    pub(super) fn catalog_options(&self, cx: &App) -> Vec<String> {
        let Some(settings) = cx.try_global::<SeriesSettings>() else {
            return Vec::new();
        };
        let diagnostics = self.diagnostics.read(cx);
        let Some(catalog) = diagnostics.catalog.as_ref() else {
            return Vec::new();
        };
        let mut options: Vec<String> = catalog
            .identities
            .iter()
            .filter(|(source, _)| settings.dataset_of(source).is_some())
            .flat_map(|(source, ids)| ids.iter().map(move |id| format!("{id}@{source}")))
            .collect();
        options.sort();
        options
    }

    /// Mark pairs already held by this tile without disabling their rows.
    pub(super) fn loaded_marks(&self, options: &[String]) -> Vec<bool> {
        options
            .iter()
            .map(|o| {
                let (identity, source) = split_option(o);
                self.model.holds_pair(source, identity)
            })
            .collect()
    }

    /// Resolve a picker commit from its live Input text and current stage.
    /// Close before adding a selected pair. A source-selection transition keeps
    /// the editor open; an empty decision leaves it unchanged.
    pub(crate) fn commit_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(Popup::Picker(p)) = &mut self.popup else {
            return false;
        };
        let text = p.input.read(cx).value().to_string();
        if p.list.set_query(&text) {
            p.refresh_add_row();
        }
        let decision = match &p.stage {
            PickerStage::Identities => match &p.add_row {
                // Nothing matched: either the text already names a pair
                // outright, or the source stage asks which source it
                // belongs to.
                Some(_) => match text.trim() {
                    // The add-row builder also excludes whitespace-only identities.
                    "" => Commit::Nothing,
                    typed => match parse_pair(typed) {
                        Some((identity, source))
                            if cx
                                .try_global::<SeriesSettings>()
                                .is_some_and(|s| s.dataset_of(source).is_some()) =>
                        {
                            Commit::Add(identity.to_string(), source.to_string())
                        }
                        // Anything else is an identity awaiting a source
                        // — including a text carrying an `@` whose right
                        // half names no configured source, which a REST
                        // path may legitimately do.
                        _ => Commit::Stage(typed.to_string()),
                    },
                },
                None => match p.list.pick() {
                    // An OPTION was built here as `{identity}@{source}`,
                    // so its source is the last `@` piece — an identity
                    // that carries an `@` of its own (a REST path from a
                    // catalogue) still splits correctly.
                    Some(i) => {
                        let (identity, source) = split_option(p.option(i));
                        Commit::Add(identity.to_string(), source.to_string())
                    }
                    // No ranked choice or typed identity: leave the picker open.
                    None => Commit::Nothing,
                },
            },
            PickerStage::Sources { identity } => match p.list.pick() {
                Some(i) => Commit::Add(identity.clone(), p.option(i).to_string()),
                None => Commit::Nothing,
            },
        };
        match decision {
            // An inert commit is unhandled so dispatch restores a standing notice.
            Commit::Nothing => return false,
            Commit::Add(identity, source) => {
                self.close_popup_with_window(window, cx);
                if let Err(e) = self.add_pair(&identity, &source, cx) {
                    self.notice = Some(e.into());
                    cx.notify();
                }
            }
            Commit::Stage(identity) => {
                let settings = cx
                    .try_global::<SeriesSettings>()
                    .cloned()
                    .unwrap_or_default();
                let sources: Vec<String> = settings.names();
                let default_source = settings.default_source.clone();
                // A stage with nothing to choose from would be a dead
                // end: say why and close, the
                // way `:add` refuses an unconfigured source.
                if sources.is_empty() {
                    self.close_popup_with_window(window, cx);
                    self.notice = Some("no fetch source is configured".into());
                    cx.notify();
                    return true;
                }
                if let Some(Popup::Picker(p)) = &mut self.popup {
                    p.enter_sources(identity, sources, default_source.as_deref());
                    // The field is the STAGE's filter now, not the
                    // identity it carries — and it keeps the keyboard.
                    let input = p.input.clone();
                    input.update(cx, |s, cx| s.set_value("", window, cx));
                }
                cx.notify();
            }
        }
        true
    }

    /// A click on painted row `row` (WINDOW-relative, matching
    /// [`geode_shell::choice::ChoiceList::highlighted`]): light it, then
    /// take exactly the path `enter` takes.
    pub(crate) fn picker_pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Picker(p)) = &mut self.popup else {
            return;
        };
        if !p.list.set_highlighted(row) {
            return;
        }
        self.commit_picker(window, cx);
    }

    // Expression field.

    /// `x` (empty) or `e` (prefilled with the cursor's expression and
    /// its slot number). The field is tile-owned and focused, like the
    /// picker's.
    pub(super) fn open_expr(
        &mut self,
        seed: Option<(u8, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("s1 / s2"));
        if let Some((_, text)) = &seed {
            let text = text.clone();
            input.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        cx.subscribe_in(&input, window, |this, _input, event, _window, cx| {
            // A typed character answers the error under the field: it
            // describes text that is no longer what is there.
            if let InputEvent::Change = event
                && let Some(Popup::Expr(f)) = &mut this.popup
                && f.error.take().is_some()
            {
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.popup = Some(Popup::Expr(ExprField {
            input,
            editing: seed.map(|(number, _)| number),
            error: None,
        }));
        self.notice = None;
        cx.notify();
    }

    /// Resolve the draft against this tile's slots. A resolution error stays
    /// inline with the draft open. A resolved expression is added or replaces
    /// its original slot, then the editor closes, including on a model refusal.
    pub(super) fn commit_expr(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(Popup::Expr(f)) = &self.popup else {
            return false;
        };
        let text = f.input.read(cx).value().to_string();
        let editing = f.editing;
        let default_source = cx
            .try_global::<SeriesSettings>()
            .and_then(|s| s.default_source.clone());
        match resolve(
            &text,
            self.model.slots(),
            default_source.as_deref(),
            editing,
        ) {
            Err(e) => {
                if let Some(Popup::Expr(f)) = &mut self.popup {
                    f.error = Some(e.into());
                }
                cx.notify();
                // Keep the draft and return unhandled so dispatch restores any standing
                // notice alongside the inline resolution error.
                return false;
            }
            Ok(expr) => {
                let written = match editing {
                    Some(number) => self.model.replace_expr(number, &text, expr),
                    None => self.model.add_expr(&text, expr).map(|(_, changed)| changed),
                };
                self.close_popup_with_window(window, cx);
                match written {
                    Ok(changed) => self.apply_changed(changed, cx),
                    // A refusal the parser could not see (a slot budget,
                    // a vanished number) is the tile's own notice, not
                    // an inline error under a field that is now gone.
                    Err(e) => {
                        self.notice = Some(e.into());
                        cx.notify();
                    }
                }
            }
        }
        true
    }

    // Range editor.

    /// Open From and To date fields with From's Day segment active.
    /// Absolute ranges use their stored inclusive dates; clipping for a query
    /// must not silently change a saved range when reopened and committed.
    /// Relative ranges resolve against now/as-of, then seed UTC dates from the
    /// start and from one second before the exclusive end (clamped to start).
    /// Committing those fields converts the draft to an absolute date range.
    pub(super) fn open_range(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        let (first, last) = match self.model.range() {
            Range::Absolute { from, to } => (*from, *to),
            relative => {
                let (now, as_of) = self.now_and_as_of(cx);
                let (start, end) = relative.resolve(now, &as_of);
                let last = end
                    .checked_sub_signed(chrono::Duration::seconds(1))
                    .unwrap_or(end)
                    .max(start);
                (start.date_naive(), last.date_naive())
            }
        };
        let open = |date: chrono::NaiveDate| {
            DateTimeField::open(
                date.and_hms_opt(0, 0, 0).expect("midnight exists"),
                Precision::Date,
                Segment::Day,
            )
        };
        let from = open(first);
        let to = open(last);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let id = self.id.0;
        self.popup = Some(Popup::Range(RangePopup {
            from_paint: DateFieldPaint::of(&from, id, Which::From),
            to_paint: DateFieldPaint::of(&to, id, Which::To),
            from,
            to,
            active: Which::From,
            focus,
            error: None,
            edited: false,
            frequency: self.model.frequency(),
        }));
        self.notice = None;
        cx.notify();
    }

    /// Handle keys at the range popup's focused container, before the shell
    /// listener. A handled key stops propagation to avoid dispatching it twice.
    /// Use the shared datefield router; chords fall through for shell actions.
    /// Tab and Shift+Tab switch fields. Insert-mode keymap actions provide the
    /// same commit, cancel, and step operations as this listener.
    pub(crate) fn range_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.popup, Some(Popup::Range(_))) {
            return false;
        }
        let modifiers = event.keystroke.modifiers;
        let chord = modifiers.control || modifiers.alt || modifiers.platform;
        if !chord && event.keystroke.key.as_str() == "tab" {
            if let Some(Popup::Range(r)) = &mut self.popup {
                r.switch();
                // A standing refusal names one of the two dates; the
                // keyboard has just moved onto the other. Answered here
                // for the same reason `apply_range_key` answers it —
                // every field key clears it.
                r.error = None;
            }
            cx.notify();
            return true;
        }
        let Some(key) = route(event.keystroke.key.as_str(), modifiers.shift, chord) else {
            return false;
        };
        match key {
            FieldKey::Commit => {
                self.commit_range(window, cx);
            }
            FieldKey::Cancel => self.close_popup_with_window(window, cx),
            // Preset digits are active only while `digit_is_preset` permits them;
            // otherwise digits edit the selected date segment.
            FieldKey::Digit(d)
                if matches!(&self.popup, Some(Popup::Range(r)) if r.digit_is_preset())
                    && Preset::digit(d).is_some() =>
            {
                let preset = Preset::digit(d).expect("just checked");
                self.write_range(Range::Relative(preset), window, cx);
            }
            other => self.apply_range_key(other, cx),
        }
        true
    }

    /// One key onto the active field. A keystroke that moves the field
    /// answers a refusal about a date that is no longer on screen — the
    /// expression field's own rule.
    pub(super) fn apply_range_key(&mut self, key: FieldKey, cx: &mut Context<Self>) {
        let id = self.id.0;
        if let Some(Popup::Range(r)) = &mut self.popup {
            r.apply(key, id);
            r.error = None;
        }
        cx.notify();
    }

    /// Select a date segment and recover the popup's focus after tile focus
    /// has moved elsewhere. Clear the prior inline refusal.
    pub(crate) fn range_segment_clicked(
        &mut self,
        which: Which,
        segment: Segment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.id.0;
        if let Some(Popup::Range(r)) = &mut self.popup {
            r.select(which, segment, id);
            r.error = None;
            if !r.focus.is_focused(window) {
                r.focus.focus(window, cx);
            }
            cx.notify();
        }
    }

    /// A click on a preset chip — the mouse form of the digit, and the
    /// same door.
    pub(crate) fn range_preset_clicked(
        &mut self,
        preset: Preset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.write_range(Range::Relative(preset), window, cx);
    }

    /// Complete pending digits in both fields and commit their dates.
    /// A refusal stays inline and returns false, allowing dispatch to retain a
    /// standing tile notice.
    pub(super) fn commit_range(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let id = self.id.0;
        let Some(Popup::Range(r)) = &mut self.popup else {
            return false;
        };
        // A digit still being typed is part of the answer (the widget's
        // commit rule: `1` in the day means the 1st), so both fields are
        // completed before either date is read — and one that cannot
        // complete names its own segment rather than committing a date
        // the trader never finished typing.
        let mut refusal = None;
        for which in [Which::From, Which::To] {
            let field = match which {
                Which::From => &mut r.from,
                Which::To => &mut r.to,
            };
            if let Err(segment) = field.complete_pending() {
                refusal = Some(format!(
                    "finish the '{}' {} or backspace",
                    which.word(),
                    segment.name()
                ));
                break;
            }
        }
        // The completion moved a value; the painted segments follow it.
        r.from_paint = DateFieldPaint::of(&r.from, id, Which::From);
        r.to_paint = DateFieldPaint::of(&r.to, id, Which::To);
        let range = match refusal {
            Some(e) => Err(e),
            // Refused INLINE rather than as a notice: the popup stays
            // open on the two dates that caused it, which is the only
            // place the trader can fix them.
            None if r.to.date() < r.from.date() => Err("'to' is before 'from'".to_string()),
            None => Ok(Range::Absolute {
                from: r.from.date(),
                to: r.to.date(),
            }),
        };
        match range {
            Ok(range) => self.write_range(range, window, cx),
            Err(e) => {
                if let Some(Popup::Range(r)) = &mut self.popup {
                    r.error = Some(e.into());
                }
                cx.notify();
                false
            }
        }
    }

    /// Validate and apply ranges from Enter, preset digits, and preset clicks.
    /// Success closes and applies model flags; a refusal stays inline with the
    /// editor open.
    pub(super) fn write_range(
        &mut self,
        range: Range,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (now, as_of) = self.now_and_as_of(cx);
        match self.model.set_range(range, now, &as_of) {
            Ok(changed) => {
                self.close_popup_with_window(window, cx);
                self.apply_changed(changed, cx);
                true
            }
            Err(e) => {
                if let Some(Popup::Range(r)) = &mut self.popup {
                    r.error = Some(e.into());
                }
                cx.notify();
                false
            }
        }
    }

    // Action-menu lifecycle and pointer controls.

    /// Toggle the menu, closing any other popup through the focus-aware closer.
    /// Opening prepares rows from the model and binding hints; subsequent chrome
    /// rebuilds refresh them while the menu remains open.
    pub(crate) fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.popup, Some(Popup::Menu(_))) {
            self.close_popup_with_window(window, cx);
            return;
        }
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        self.open_menu(cx);
    }

    /// Build and install the menu over the current model.
    pub(super) fn open_menu(&mut self, cx: &mut Context<Self>) {
        let rows = self.menu_rows(cx);
        let highlighted = menu::first_enabled(&rows);
        self.popup = Some(Popup::Menu(MenuState { rows, highlighted }));
        self.notice = None;
        cx.notify();
    }

    /// The menu's rows over the model as it is now, every `hint` its
    /// action's live chord (the footer's own rule). Called at open and
    /// from `rebuild_chrome` while the menu is up — a `:` line or a
    /// delivery can move the cursor slot under an open menu, and a row
    /// must keep its promise (the heading, Hide/Show, the enablement).
    pub(super) fn menu_rows(&self, cx: &App) -> Vec<menu::MenuRow> {
        let default_source = cx
            .try_global::<SeriesSettings>()
            .and_then(|s| s.default_source.clone());
        let mut rows = menu::rows(
            &menu::MenuInputs { model: &self.model },
            default_source.as_deref(),
        );
        let empty = Vec::new();
        let bindings = cx
            .try_global::<geode_shell::tips::Chords>()
            .map(|c| c.0.as_slice())
            .unwrap_or(&empty);
        for row in &mut rows {
            if let menu::MenuRow::Action { id, hint, .. } = row {
                *hint = geode_shell::tips::chord_for(bindings, &id.0)
                    .map(|ks| geode_shell::palette::render_binding(&ks).into())
                    .unwrap_or_default();
            }
        }
        rows
    }

    /// A click on the `range · freq` readout: opens the range popup
    /// through `r`'s own path, or CLOSES it when it is already up — a
    /// second click that reopened would seed fresh fields over dates
    /// the trader had started typing.
    pub(crate) fn readout_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.popup, Some(Popup::Range(_))) {
            self.close_popup_with_window(window, cx);
            return;
        }
        self.dispatch(&ActionId("timeseries::range".into()), None, window, cx);
    }

    /// A pointer resting on menu row `index`: the mouse form of `j`/`k`.
    /// Change-only, because gpui fires this on every pointer move over
    /// the row.
    pub(crate) fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Popup::Menu(m)) = &mut self.popup else {
            return;
        };
        if m.highlighted == index || index >= m.rows.len() {
            return;
        }
        m.highlighted = index;
        cx.notify();
    }

    /// `enter` on the highlighted row, or a click on any row: a disabled
    /// row's reason becomes the notice and the menu stays; an enabled
    /// one closes the menu and re-enters [`Self::dispatch`] on its own
    /// action id, so a row, a key and the palette take one path.
    pub(crate) fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Menu(m)) = &self.popup else {
            return;
        };
        let Some(menu::MenuRow::Action { id, enabled, .. }) = m.rows.get(index) else {
            return;
        };
        match enabled {
            Err(reason) => {
                self.notice = Some((*reason).into());
                cx.notify();
            }
            Ok(()) => {
                let id = id.clone();
                self.close_popup_with_window(window, cx);
                self.dispatch(&id, None, window, cx);
            }
        }
    }

    /// Select the clicked slot and replace any open popup with its action menu.
    pub(crate) fn chip_context_menu(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.model.set_cursor(index);
        self.apply_changed(changed, cx);
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        self.open_menu(cx);
    }

    /// A click on a chip's swatch: the slot becomes the cursor and its
    /// visibility flips — `v`'s own path, so the fetch-on-show and the
    /// density budget rules ride along.
    pub(crate) fn swatch_clicked(&mut self, index: usize, cx: &mut Context<Self>) {
        let moved = self.model.set_cursor(index);
        let flipped = self.model.toggle_visible();
        self.apply_changed(moved | flipped, cx);
    }

    /// A click on one of the range popup's frequency chips: the
    /// frequency is written at once and the popup STAYS open (it is a
    /// setting the popup shows ticked, not a commit of the popup), with
    /// the cap refusal inline like a backwards range.
    pub(crate) fn range_freq_clicked(&mut self, f: Frequency, cx: &mut Context<Self>) {
        let (now, as_of) = self.now_and_as_of(cx);
        match self.model.set_frequency(f, now, &as_of) {
            Ok(changed) => {
                if let Some(Popup::Range(r)) = &mut self.popup {
                    r.error = None;
                    r.frequency = f;
                }
                self.apply_changed(changed, cx);
            }
            Err(e) => {
                if let Some(Popup::Range(r)) = &mut self.popup {
                    r.error = Some(e.into());
                }
                cx.notify();
            }
        }
    }

    // ---- shared by every popup ---------------------------------------

    /// Add a source pair from the picker or `:add`, sharing source/dataset
    /// validation and the model-change dispatch path.
    pub(super) fn add_pair(
        &mut self,
        identity: &str,
        source: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let settings = cx
            .try_global::<SeriesSettings>()
            .cloned()
            .unwrap_or_default();
        let dataset = settings
            .dataset_of(source)
            .ok_or_else(|| {
                format!(
                    "'{source}' is not a fetch source (have: {})",
                    settings.names().join(", ")
                )
            })?
            .to_string();
        let changed = self.model.add_source(identity, source, &dataset)?.1;
        self.apply_changed(changed, cx);
        Ok(())
    }

    /// Blur the popup's own focused field before dropping it so the shell can
    /// restore focus. Do not blur a different field that acquired focus while
    /// the popup stayed open. The fieldless series list needs no blur.
    pub(crate) fn close_popup_with_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let own_field_focused = self
            .popup
            .as_ref()
            .is_some_and(|p| p.holds_focus(window, cx));
        if own_field_focused {
            window.blur(cx);
        }
        self.popup = None;
        cx.notify();
    }

    /// Move the shared header/list cursor to a clicked slot without closing
    /// the current popup.
    pub(crate) fn chip_clicked(&mut self, index: usize, cx: &mut Context<Self>) {
        let changed = self.model.set_cursor(index);
        self.apply_changed(changed, cx);
    }
}
