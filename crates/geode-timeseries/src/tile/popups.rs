//! The tile's popups: the series list, the add picker, the expression
//! field, the custom dates editor and the three menus — opened, keyed,
//! committed and cancelled here, painted by `crate::popup`.

use super::*;

impl TimeseriesTile {
    /// Every popup verb: the series list, the add picker, the
    /// expression field, the dates editor and the menus are all opened,
    /// committed and cancelled from here.
    ///
    /// `e`'s refusal on a SOURCE slot stays this method's: there is no
    /// expression to open, and a trader who pressed it deserves the
    /// reason rather than a dead key.
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
            // `.` and the `⋯` button: one toggle for both halves, the
            // market-data menu's own shape.
            "menu" => {
                self.toggle_menu(window, cx);
                true
            }
            // Every menu's own keys: `j`/`k` step over pickable rows,
            // clamped; `enter` picks the highlighted one; `escape`
            // closes. Reusing the list verbs keeps the fragment small —
            // the two popups never share a frame.
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
            // A second `L` closes it — one key for both halves, the way
            // the market-data menu's own `menu` verb toggles.
            "list" => {
                if self.popup.is_some() {
                    self.close_popup_with_window(window, cx);
                } else {
                    self.open_series_popup(cx);
                }
                true
            }
            // The list's cursor IS the chips' cursor, so `j`/`k` are
            // `tab`/`shift+tab` under another name — and wrap the same
            // way. Guarded on the list being open: the fragment binds
            // them only there, but the palette can reach any action.
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
            // `a` and `x` open their own field, closing whatever was up
            // first: a trader who pressed one with the list open meant
            // the new field, and two popups at a time is the thing this
            // enum exists to forbid.
            "add" => {
                self.open_picker(window, cx);
                true
            }
            "expr" => {
                self.open_expr(None, window, cx);
                true
            }
            // `r` and `f` toggle their own menu, `.`'s shape: a second
            // press closes it, and over any other popup the press closes
            // that and opens the menu (over the dates editor
            // `dispatch`'s insert gate has closed it already, so `r`
            // there lands on the range menu).
            "range" => {
                self.toggle_menu_kind(MenuKind::Range, window, cx);
                true
            }
            "freq" => {
                self.toggle_menu_kind(MenuKind::Frequency, window, cx);
                true
            }
            // `c` in the range menu, its `Custom dates…` row, and the
            // palette's action: the dates editor, whatever was up.
            "range_custom" => {
                self.open_range_editor(window, cx);
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
            "pick_colour" => self.open_colour_picker(window, cx),
            // The colour picker commits through its own keys (a hex
            // field's `enter`, a swatch) inside the component; an
            // `enter` that reaches the tile is inert.
            "commit" => match &self.popup {
                Some(Popup::Picker(_)) => self.commit_picker(window, cx),
                Some(Popup::Expr(_)) => self.commit_expr(window, cx),
                Some(Popup::Range(_)) => self.commit_range(window, cx),
                _ => false,
            },
            // `escape` in the dates editor goes BACK to the range menu,
            // highlight on `Custom dates…`, rather than closing: the
            // editor is one of the menu's rows opened up.
            "cancel" if matches!(self.popup, Some(Popup::Range(_))) => {
                self.back_to_range_menu(window, cx);
                true
            }
            "cancel" if self.popup.as_ref().is_some_and(Popup::is_insert) => {
                self.close_popup_with_window(window, cx);
                true
            }
            // The picker's own rule is CLAMPED stepping (header spec §7,
            // the underlying picker's): a bare step at either end stays
            // put rather than wrapping round to the far end of a list
            // the trader is reading top-down.
            // The dates editor's own step: `up`/`down` on the active
            // segment, the same `FieldKey::Step` its listener routes
            // them to — the keymap path and the listener path must not
            // be able to disagree about what an arrow means.
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

    /// Open the series list (spec §9.5). The rows are prepared by the
    /// ONE door that prepares every other piece of chrome, so an empty
    /// popup can never be painted: `rebuild_chrome` fills it in the same
    /// update it is opened in.
    pub(super) fn open_series_popup(&mut self, cx: &mut Context<Self>) {
        self.popup = Some(Popup::Series(SeriesPopup::default()));
        self.rebuild_chrome(cx);
        cx.notify();
    }

    // ---- the add picker (spec §9.6) ----------------------------------

    /// `a`: the typeahead over every catalogued `identity@source`, its
    /// field holding the keyboard (which is what `mode == insert` and
    /// `holds_focus` both report off).
    ///
    /// The catalogue is re-requested on the way in when there is none to
    /// rank (the market-data picker's rule, and CLAUDE.md's trap:
    /// `request_catalog()` queues but never notifies, so the caller must
    /// — in the same update); one that lands WHILE this is open is
    /// folded in by the `Diagnostics` observer in `new`.
    pub(super) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        self.request_catalog(cx);
        let options = self.catalog_options(cx);
        let loaded = self.loaded_marks(&options);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("identity@source"));
        cx.subscribe_in(&input, window, |this, input, event, _window, cx| {
            // The LIVE path, for a trader actually typing.
            // `InputState::set_value` emits no `Change` at all, which is
            // why `commit_picker` re-feeds the field's own text as well
            // rather than trusting this subscription alone.
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

    /// Ask for a catalogue when there is nothing to rank. Queued AND
    /// notified in one update: `Diagnostics::request_catalog` never
    /// notifies on its own (CLAUDE.md), so without this the bridge would
    /// not drain the request until something else woke the entity.
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

    /// Which of `options` this tile already holds — the `•` mark. A
    /// marked row is still pickable (spec §9.6).
    pub(super) fn loaded_marks(&self, options: &[String]) -> Vec<bool> {
        options
            .iter()
            .map(|o| {
                let (identity, source) = split_option(o);
                self.model.holds_pair(source, identity)
            })
            .collect()
    }

    /// `enter` with the picker open, and the mouse form of the `add`
    /// row: re-feed the field's LIVE text first (`set_value` emits no
    /// `Change`, so the last ranking may never have been run), then
    /// resolve what the stage says the commit means.
    ///
    /// The popup closes BEFORE the add, the way the market-data
    /// picker's `picker_pick` does: adding needs no keyboard, and the
    /// field is done being useful the moment a pair is chosen.
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
                    // Belt and braces: `refresh_add_row` trims, so a
                    // query of nothing but spaces offers no add row to
                    // reach this arm through in the first place.
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
                    // Nothing ranked and nothing typed: inert, and the
                    // picker stays open (the market-data rule).
                    None => Commit::Nothing,
                },
            },
            PickerStage::Sources { identity } => match p.list.pick() {
                Some(i) => Commit::Add(identity.clone(), p.option(i).to_string()),
                None => Commit::Nothing,
            },
        };
        match decision {
            // Nothing to commit: the picker stays open (retyping is one
            // keystroke away) and the verb answers UNHANDLED, so the
            // notice `dispatch` took on the way in goes back on screen
            // rather than being cleared by an `enter` that did nothing.
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

    // ---- the expression field (spec §9.7) ----------------------------

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

    /// `enter` with the expression field open (spec §9.7): parse against
    /// this tile's own slots (§7). An error paints INLINE under the
    /// field and the field stays open and focused — a parse error is
    /// about the text still on screen, and closing would throw it away.
    /// A success adds or replaces, then closes.
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
                // UNHANDLED, like the picker's inert `enter`: nothing
                // was written, the field is still open on the text that
                // caused it, and `dispatch`'s tail is what puts a
                // standing notice back — answering `true` here dropped
                // one for a keystroke that changed nothing.
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

    // ---- the custom dates editor (spec §9.8) ------------------------

    /// `c` in the range menu (or its `Custom dates…` row): two segmented
    /// date fields seeded from the range the model holds now, with
    /// `from` active on its day segment — so the first digit typed is
    /// the day's.
    ///
    /// A `Relative` range is RESOLVED, so it opens as the dates it
    /// currently means — and the `to` field shows the INCLUSIVE last
    /// day, which `resolve`'s half-open end is a second past. Hence the
    /// second back before the date is taken: `Range::Absolute`'s own
    /// convention, read in reverse.
    ///
    /// An `Absolute` range instead seeds from its STORED dates, as typed
    /// (a ruling). `resolve` clips its end to the frame's
    /// as-of — right for what is fetched and queried (ruling 4), wrong
    /// for a seed: reopening the editor under an as-of inside the stored
    /// span would show a `to` the trader never typed, and `enter` would
    /// then write it. Seeding from the stored pair makes the round trip
    /// lossless under any as-of and across midnight.
    pub(super) fn open_range_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        }));
        self.notice = None;
        cx.notify();
    }

    /// `escape` in the dates editor, by either door (its listener and
    /// the `mode == insert` layer's `cancel`): the editor closes through
    /// the one closer — blurred first — and the range menu opens with
    /// its highlight on `Custom dates…`, the row the editor came from.
    /// A second `escape` then closes the menu.
    pub(super) fn back_to_range_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popup_with_window(window, cx);
        self.open_menu(MenuKind::Range, cx);
        if let Some(Popup::Menu(m)) = &mut self.popup
            && let Some(custom) = menu::custom_row(&m.rows)
        {
            m.highlighted = custom;
        }
    }

    /// The dates editor's own keys, run from the `on_key_down` on its
    /// focused container — which sits on the focused element and so runs
    /// BEFORE the shell root's listener. Answers whether the key was
    /// consumed; the listener stops propagation on `true`.
    ///
    /// `geode_widgets::datefield::route` is the ONE key table this
    /// consults (CLAUDE.md), and a chord answers `None` there, so
    /// `ctrl+k` still opens the palette over an open editor. `tab` is
    /// the one key this editor adds: `route` has no arm for it, and with
    /// two fields under one keyboard both directions are the same move.
    /// A digit is the active segment's at once — the presets live in the
    /// range menu, not here.
    ///
    /// `enter`, `escape`, `up` and `down` are ALSO bound by the
    /// fragment's `mode == insert` layer, so both doors end in the same
    /// four calls — whichever fires first stops the other.
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
            FieldKey::Cancel => self.back_to_range_menu(window, cx),
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

    /// A click on one of the two fields' segments: the mouse form of
    /// `tab` plus `left`/`right`, taking the keyboard back when a
    /// tile-focus move has left the popup open without it (the
    /// market-data field's M4).
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

    /// `enter` with the dates editor open: finish both fields' pending
    /// digits, then write the two dates.
    ///
    /// Answers whether the keystroke was HANDLED, in the sense
    /// `dispatch`'s tail means: a refusal that keeps the popup open and
    /// paints an inline reason is `false`, so a standing notice survives
    /// it (the expression field's own answer).
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

    /// The door the dates editor writes through on `enter`. A cap
    /// refusal is inline, for the same reason a backwards range is: the
    /// editor holds what caused it.
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

    // ---- the menus ---------------------------------------------------

    /// `.` and the `⋯` button: the action list's toggle.
    pub(crate) fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_menu_kind(MenuKind::Actions, window, cx);
    }

    /// Close menu `kind` if it is up, otherwise close whatever is (a
    /// field blurred first, through the one closer) and open it. The
    /// rows are built HERE, once per open — the model, the default
    /// source, the live chords and the frequency rows' cap refusals are
    /// read then, never in `render`.
    pub(super) fn toggle_menu_kind(
        &mut self,
        kind: MenuKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(&self.popup, Some(Popup::Menu(m)) if m.kind == kind) {
            self.close_popup_with_window(window, cx);
            return;
        }
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        self.open_menu(kind, cx);
    }

    /// Build and install menu `kind` over the current model, the
    /// highlight on the value in force (`menu::start`).
    pub(super) fn open_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let rows = self.menu_rows(kind, cx);
        let highlighted = menu::start(&rows);
        self.popup = Some(Popup::Menu(MenuState {
            kind,
            rows,
            highlighted,
        }));
        self.notice = None;
        cx.notify();
    }

    /// Menu `kind`'s rows over the model as it is now. Called at open and
    /// from `rebuild_chrome` while the menu is up — a `:` line or a
    /// delivery can move what a row promises under an open menu (the
    /// cursor slot, the range, the frequency, the cap).
    ///
    /// Every action row's `hint` is its action's live chord (the
    /// footer's own rule), and so is `Custom dates…`'s when the keymap
    /// binds one; a preset's and a frequency's is its short label.
    pub(super) fn menu_rows(&self, kind: MenuKind, cx: &App) -> Vec<menu::MenuRow> {
        let mut rows = match kind {
            MenuKind::Actions => {
                let default_source = cx
                    .try_global::<SeriesSettings>()
                    .and_then(|s| s.default_source.clone());
                menu::rows(
                    &menu::MenuInputs { model: &self.model },
                    default_source.as_deref(),
                )
            }
            MenuKind::Range => menu::range_rows(self.model.range()),
            MenuKind::Frequency => {
                let (now, as_of) = self.now_and_as_of(cx);
                menu::frequency_rows(self.model.frequency(), |f| {
                    self.model.frequency_refusal(f, now, &as_of)
                })
            }
        };
        let empty = Vec::new();
        let bindings = cx
            .try_global::<geode_shell::tips::Chords>()
            .map(|c| c.0.as_slice())
            .unwrap_or(&empty);
        let chord = |action: &str| {
            geode_shell::tips::chord_for(bindings, action)
                .map(|ks| SharedString::from(geode_shell::palette::render_binding(&ks)))
        };
        for row in &mut rows {
            match row {
                menu::MenuRow::Action {
                    pick: menu::Pick::Action(id),
                    hint,
                    ..
                } => *hint = chord(&id.0).unwrap_or_default(),
                menu::MenuRow::Action {
                    pick: menu::Pick::CustomRange,
                    hint,
                    ..
                } => {
                    if let Some(live) = chord(menu::CUSTOM_RANGE_ACTION) {
                        *hint = live;
                    }
                }
                _ => {}
            }
        }
        rows
    }

    /// A click on the range trigger: opens the range menu through `r`'s
    /// own path, or CLOSES what the trigger owns when it is up — the
    /// range menu, or the dates editor opened from it (a second click
    /// that reopened would throw away dates the trader had started
    /// typing).
    pub(crate) fn range_trigger_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owned = match &self.popup {
            Some(Popup::Range(_)) => true,
            Some(Popup::Menu(m)) => m.kind == MenuKind::Range,
            _ => false,
        };
        if owned {
            self.close_popup_with_window(window, cx);
            return;
        }
        self.dispatch(&ActionId("timeseries::range".into()), None, window, cx);
    }

    /// A press outside the popup that was painted: `Some(kind)` for a
    /// menu, `None` for the dates editor. It closes that popup only if
    /// it is still the one up — the listener belongs to the frame that
    /// painted it, and a trigger's capture-phase press runs first and may
    /// already have swapped another popup in (the frequency trigger over
    /// an open range menu), which this press must leave open.
    pub(crate) fn outside_press(
        &mut self,
        painted: Option<MenuKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let still_up = match (&self.popup, painted) {
            (Some(Popup::Menu(m)), Some(kind)) => m.kind == kind,
            (Some(Popup::Range(_)), None) => true,
            _ => false,
        };
        if still_up {
            self.close_popup_with_window(window, cx);
        }
    }

    /// A click on the frequency trigger: `f`'s own path, which toggles.
    pub(crate) fn freq_trigger_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(&ActionId("timeseries::freq".into()), None, window, cx);
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
    /// row's reason becomes the notice and the menu stays. An enabled
    /// action row closes the menu and re-enters [`Self::dispatch`] on
    /// its own action id, so a row, a key and the palette take one path;
    /// a preset or a frequency is written through the model's own setter
    /// (`:range`'s and `:freq`'s), and `Custom dates…` opens the editor.
    pub(crate) fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Menu(m)) = &self.popup else {
            return;
        };
        let Some(menu::MenuRow::Action { pick, enabled, .. }) = m.rows.get(index) else {
            return;
        };
        if let Err(reason) = enabled {
            self.notice = Some(reason.clone());
            cx.notify();
            return;
        }
        match pick.clone() {
            menu::Pick::Action(id) => {
                self.close_popup_with_window(window, cx);
                self.dispatch(&id, None, window, cx);
            }
            menu::Pick::CustomRange => self.open_range_editor(window, cx),
            menu::Pick::Range(preset) => {
                let (now, as_of) = self.now_and_as_of(cx);
                let written = self.model.set_range(Range::Relative(preset), now, &as_of);
                self.menu_written(written, window, cx);
            }
            menu::Pick::Frequency(f) => {
                let (now, as_of) = self.now_and_as_of(cx);
                let written = self.model.set_frequency(f, now, &as_of);
                self.menu_written(written, window, cx);
            }
        }
    }

    /// A value row's write: on success the menu closes and the change
    /// takes the usual tail; a refusal (the point cap, for a preset the
    /// frequency in force cannot cover) becomes the notice and the menu
    /// stays up on the row that caused it, as a disabled row's does.
    fn menu_written(
        &mut self,
        written: Result<Changed, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match written {
            Ok(changed) => {
                self.close_popup_with_window(window, cx);
                self.apply_changed(changed, cx);
            }
            Err(e) => {
                self.notice = Some(e.into());
                cx.notify();
            }
        }
    }

    /// A right-click on a chip: the slot under the pointer becomes the
    /// cursor and the menu opens on it — the design guide's context
    /// menu for "commands that act on the object under the pointer".
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
        self.open_menu(MenuKind::Actions, cx);
    }

    /// A click on a chip's swatch: the slot becomes the cursor and its
    /// visibility flips — `v`'s own path, so the fetch-on-show and the
    /// density budget rules ride along.
    pub(crate) fn swatch_clicked(&mut self, index: usize, cx: &mut Context<Self>) {
        let moved = self.model.set_cursor(index);
        let flipped = self.model.toggle_visible();
        self.apply_changed(moved | flipped, cx);
    }

    // ---- the colour picker -------------------------------------------

    /// `Colour…`: open gpui-component's picker over the cursor's slot.
    /// Refused like the slot section's other rows when there is none.
    ///
    /// The state is seeded with the slot's colour as painted now, WITHOUT
    /// a change event (`set_value`), and the featured row is the five
    /// palette colours then every `[colours]` name, resolved through the
    /// same `colour_fn` the chips use — so a featured swatch is exactly
    /// the `Hsla` a slot on that colour paints, and picking one maps
    /// back to it.
    pub(super) fn open_colour_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(slot) = self.model.cursor_slot() else {
            self.notice = Some("add a series first".into());
            cx.notify();
            return false;
        };
        let target = slot.number;
        let current = slot.colour.clone();
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        let colours = Arc::clone(&self.colours.borrow());
        let (featured, seed) = {
            let colour_of = colour_fn(Arc::clone(&colours), cx.theme());
            let featured: Vec<(Hsla, Colour)> = (0..Palette::LEN)
                .map(Colour::Palette)
                .chain(colours.names().map(|n| Colour::Named(n.to_string())))
                .map(|c| (colour_of(&c), c))
                .collect();
            (featured, colour_of(&current))
        };
        let swatches = featured.iter().map(|(h, _)| *h).collect();
        let picker = self.colour_picker_state(window, cx);
        picker.update(cx, |state, cx| {
            state.set_value(seed, window, cx);
            state.set_open(true, cx);
        });
        self.popup = Some(Popup::Colour(ColourPick {
            target,
            swatches,
            picker,
        }));
        self.pick_context = Some(PickContext { target, featured });
        self.notice = None;
        cx.notify();
        true
    }

    /// The picker state, made on first use. Its two subscriptions are
    /// the whole bridge: a `Change` carrying a colour writes it to the
    /// TARGET slot, and the state going closed — by any route the
    /// component owns (`escape`, a click outside, a swatch or hex commit,
    /// the trigger) — closes the popup through the one closer.
    fn colour_picker_state(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ColorPickerState> {
        if let Some(picker) = &self.colour_picker {
            return picker.clone();
        }
        let picker = cx.new(|cx| ColorPickerState::new(window, cx));
        cx.subscribe_in(&picker, window, |this, _picker, event, _window, cx| {
            // `None` is a cleared value, which this picker never offers.
            if let ColorPickerEvent::Change(Some(h)) = event {
                this.colour_picked(*h, cx);
            }
        })
        .detach();
        cx.observe_in(&picker, window, |this, picker, window, cx| {
            if !picker.read(cx).is_open() && matches!(this.popup, Some(Popup::Colour(_))) {
                this.close_popup_with_window(window, cx);
            }
        })
        .detach();
        self.colour_picker = Some(picker.clone());
        picker
    }

    /// A colour the picker committed — a featured swatch, the palette
    /// grid, a slider step, the hex field's `enter`. Written through
    /// `:colour`'s own door (`Model::set_colour`, then `apply_changed`),
    /// so the repaint and the session follow the usual route. A slider
    /// drag commits every step, so the chart follows it live.
    ///
    /// Read off [`PickContext`], not the popup, because a hex `enter`'s
    /// `Change` lands after the popover it came from has closed. A
    /// target that has since gone takes nothing: the pick has nowhere
    /// to land.
    ///
    /// A pick within a step of what the target already paints is no
    /// change at all (`within_a_step`): `enter` on the component's
    /// untouched hex field hands back the painted colour truncated, and
    /// that must leave a theme-following colour theme-following.
    pub(super) fn colour_picked(&mut self, h: Hsla, cx: &mut Context<Self>) {
        let Some(pick) = &self.pick_context else {
            return;
        };
        let Some(current) = self
            .model
            .slots()
            .iter()
            .find(|s| s.number == pick.target)
            .map(|s| s.colour.clone())
        else {
            return;
        };
        let painted = colour_fn(Arc::clone(&self.colours.borrow()), cx.theme())(&current);
        if within_a_step(Rgb8::from_hsla(h), Rgb8::from_hsla(painted)) {
            return;
        }
        let colour = colour_from_pick(h, &pick.featured);
        if let Ok(changed) = self.model.set_colour(pick.target, colour) {
            self.apply_changed(changed, cx);
        }
    }

    /// Close the picker when the slot it was opened for is gone.
    pub(super) fn close_orphaned_colour_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Popup::Colour(c)) = &self.popup
            && self.model.slots().iter().all(|s| s.number != c.target)
        {
            self.close_popup_with_window(window, cx);
        }
    }

    // ---- shared by every popup ---------------------------------------

    /// The ONE door a `(identity, source)` pair is added by — the `:add`
    /// line's and the picker's both, so the dataset check, its message
    /// and the `apply_changed` tail cannot drift between them.
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

    /// The ONE closer (the market-data panel's rule): every path that
    /// drops a popup comes through here, because a popup whose own field
    /// holds the keyboard has to be blurred BEFORE it is dropped — an
    /// unblurred dead handle leaves `Window::focused` pointing at
    /// nothing for the rest of the session, and the shell's focus-return
    /// net never fires.
    ///
    /// The series list holds no field, so for it the blur is a no-op;
    /// the picker's and the expression field's are what make the
    /// `window` parameter earn its keep. The blur is conditional on the
    /// popup's OWN field holding focus: a field orphaned by a tile-focus
    /// move, closed from a `:` line, must not blur the command line.
    pub(crate) fn close_popup_with_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let own_field_focused = self
            .popup
            .as_ref()
            .is_some_and(|p| p.holds_focus(window, cx));
        if own_field_focused {
            window.blur(cx);
        }
        // The picker's open state is the component's; a close from this
        // side (a verb, `:remove`) tells it, so the next `Colour…` starts
        // from a closed popover. Its observer then finds no popup.
        if let Some(Popup::Colour(c)) = &self.popup {
            c.picker.update(cx, |state, cx| state.set_open(false, cx));
        }
        self.popup = None;
        cx.notify();
    }

    /// The mouse's form of `tab` (spec §9.3): a chip click moves the
    /// cursor onto its slot — and so does a click on the series list's
    /// row, which is the same slot under another painting. Whatever
    /// popup is open STAYS open: the list's own highlight is this
    /// cursor, so a row click that closed it would take the thing it
    /// just moved off the screen.
    pub(crate) fn chip_clicked(&mut self, index: usize, cx: &mut Context<Self>) {
        let changed = self.model.set_cursor(index);
        self.apply_changed(changed, cx);
    }
}
