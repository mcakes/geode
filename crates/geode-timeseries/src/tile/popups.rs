//! The tile's popups: the series list, the add picker, the expression
//! field and the range dialog — opened, keyed, committed and cancelled
//! here, painted by `crate::popup`.

use super::*;

impl TimeseriesTile {
    /// Every popup verb: the series list, the add picker, the
    /// expression field and the range dialog are all opened, committed
    /// and cancelled from here.
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
            // The menu's own keys: `j`/`k` step over action rows,
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
            // `r` opens the range popup, closing whatever was up first —
            // `a` and `x`'s own rule. Note what that means for a SECOND
            // `r`: the popup is an insert popup, so `dispatch`'s
            // stage-aware gate has already closed it by the time this
            // arm runs, and the arm therefore REOPENS it on a fresh
            // seed rather than toggling it shut. `escape` is the close
            // (spec §9.8 gives `r` no toggle), and reopening on the
            // range now in the model is a harmless answer to a key the
            // trader pressed meaning "the range".
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
            // The picker's own rule is CLAMPED stepping (header spec §7,
            // the underlying picker's): a bare step at either end stays
            // put rather than wrapping round to the far end of a list
            // the trader is reading top-down.
            // The range popup's own step: `up`/`down` on the active
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

    // ---- the range popup (spec §9.8) ---------------------------------

    /// `r`: two segmented date fields seeded from the range the model
    /// holds now, with `from` active on its day segment.
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
    /// for a seed: reopening `r` under an as-of inside the stored span
    /// would show a `to` the trader never typed, and `enter` would then
    /// write it. Seeding from the stored pair makes the round trip
    /// lossless under any as-of and across midnight.
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

    /// The range popup's own keys, run from the `on_key_down` on its
    /// focused container — which sits on the focused element and so runs
    /// BEFORE the shell root's listener. Answers whether the key was
    /// consumed; the listener stops propagation on `true`.
    ///
    /// `geode_widgets::datefield::route` is the ONE key table this
    /// consults (CLAUDE.md), and a chord answers `None` there, so
    /// `ctrl+k` still opens the palette over an open popup. `tab` is the
    /// one key this popup adds: `route` has no arm for it, and with two
    /// fields under one keyboard both directions are the same move.
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
            FieldKey::Cancel => self.close_popup_with_window(window, cx),
            // The digit shortcut (§9.8) — see `RangePopup::
            // digit_is_preset` for when a digit is a preset and when it
            // belongs to the date.
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

    /// `enter` with the range popup open: finish both fields' pending
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

    /// The ONE door the range popup writes through — the digit, the
    /// chip click and `enter` all take it, so the cap refusal, the
    /// close and the `apply_changed` tail cannot drift between them.
    /// A cap refusal is inline too, for the same reason a backwards
    /// range is: the popup holds what caused it.
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

    // ---- the action menu (mouse pass, 2026-09-24) --------------------

    /// `.` and the `⋯` button: close the menu if it is up, otherwise
    /// close whatever is (a field blurred first, through the one
    /// closer) and open it. The rows are built HERE, once per open —
    /// the model, the default source and the live chords are read
    /// then, never in `render`.
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
