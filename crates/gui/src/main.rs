//! mirrorscope: GUI entry point. `mirrorscope --cli ...` runs headless instead.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use mirrorscope_core::backups::{diff_text, list_backups, BackupInfo};
use mirrorscope_core::config::{default_config_path, valid_preset_name, Config, Settings, Theme};
use mirrorscope_core::fetch::{fetch_status, http_client};
use mirrorscope_core::filter::{filter_mirrors, FilterConfig};
use mirrorscope_core::mirrorlist::{generate, HeaderInfo};
use mirrorscope_core::select::select_indices;
use mirrorscope_core::sort::sort_mirrors;
use mirrorscope_core::speedtest::{
    format_rate, run_speedtest, SpeedEvent, SpeedOptions, SpeedOutcome,
};
use mirrorscope_core::{
    privileged, units, Mirror, Protocol, SortKey, SortOrder, PACMAN_DIR, STATUS_URL,
    SYSTEM_MIRRORLIST,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio_util::sync::CancellationToken;

slint::include_modules!();

#[derive(Clone, Debug)]
enum Speed {
    Testing,
    Rate(f64),
    Failed(String),
}

struct App {
    rt: tokio::runtime::Runtime,
    config: Config,
    config_path: Option<PathBuf>,
    settings: Settings,
    all: Vec<Mirror>,
    /// Filtered and sorted candidates.
    list: Vec<Mirror>,
    speeds: HashMap<String, Speed>,
    overrides: HashMap<String, bool>,
    country_counts: Vec<(String, String, usize)>,
    backups: Vec<BackupInfo>,
    cancel: Option<CancellationToken>,
    test_done: usize,
    test_total: usize,
    units_preset: Option<String>,
    rows_model: Rc<VecModel<Row>>,
    countries_model: Rc<VecModel<CountryRow>>,
    backups_model: Rc<VecModel<BackupRow>>,
    presets_model: Rc<VecModel<SharedString>>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.borrow_mut().as_mut().map(f))
}

/// Run `f` on the UI thread with the window and app state.
fn on_ui(weak: &slint::Weak<MainWindow>, f: impl FnOnce(&MainWindow, &mut App) + Send + 'static) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            with_app(|app| f(&ui, app));
        }
    });
}

fn set_msg(ui: &MainWindow, msg: impl Into<SharedString>, error: bool) {
    ui.set_message(msg.into());
    ui.set_message_error(error);
}

fn rates_of(app: &App) -> HashMap<String, f64> {
    app.speeds
        .iter()
        .filter_map(|(k, v)| match v {
            Speed::Rate(r) => Some((k.clone(), *r)),
            _ => None,
        })
        .collect()
}

fn human_age(t: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    match t {
        None => "never".into(),
        Some(t) => {
            let mins = (now - t).num_minutes();
            if mins < 90 {
                format!("{mins} min ago")
            } else if mins < 48 * 60 {
                format!("{:.1} h ago", mins as f64 / 60.0)
            } else {
                format!("{} d ago", mins / (24 * 60))
            }
        }
    }
}

fn fmt_delay(d: Option<i64>) -> String {
    match d {
        None => "-".into(),
        Some(s) if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        Some(s) => format!("{:.1} h", s as f64 / 3600.0),
    }
}

fn speed_cells(s: Option<&Speed>) -> (String, i32) {
    match s {
        None => (String::new(), 0),
        Some(Speed::Testing) => (String::new(), 1),
        Some(Speed::Rate(r)) => (format_rate(*r), 2),
        Some(Speed::Failed(e)) => (e.clone(), 3),
    }
}

fn make_row(m: &Mirror, checked: bool, speed: Option<&Speed>, now: DateTime<Utc>) -> Row {
    let (speed_text, state) = speed_cells(speed);
    Row {
        checked,
        url: m.url.clone().into(),
        country: m.country.clone().into(),
        protocol: m.protocol.as_str().into(),
        last_sync: human_age(m.last_sync, now).into(),
        delay: fmt_delay(m.delay).into(),
        score: m
            .score
            .map(|s| format!("{s:.2}"))
            .unwrap_or_else(|| "-".into())
            .into(),
        speed: speed_text.into(),
        completion: m
            .completion_pct
            .map(|c| format!("{:.0}%", c * 100.0))
            .unwrap_or_else(|| "-".into())
            .into(),
        state,
    }
}

/// Column index in the table -> sort key.
fn col_key(col: i32) -> Option<SortKey> {
    Some(match col {
        0 => SortKey::Url,
        1 => SortKey::Country,
        3 => SortKey::Age,
        4 => SortKey::Delay,
        5 => SortKey::Score,
        6 => SortKey::Rate,
        7 => SortKey::Completion,
        _ => return None,
    })
}

fn key_col(key: SortKey) -> i32 {
    match key {
        SortKey::Url => 0,
        SortKey::Country => 1,
        SortKey::Age => 3,
        SortKey::Delay => 4,
        SortKey::Score => 5,
        SortKey::Rate => 6,
        SortKey::Completion => 7,
    }
}

/// Read the filter widgets into `app.settings`.
fn pull(ui: &MainWindow, app: &mut App) {
    let s = &mut app.settings;
    let mut protos = Vec::new();
    if ui.get_p_https() {
        protos.push(Protocol::Https);
    }
    if ui.get_p_http() {
        protos.push(Protocol::Http);
    }
    if ui.get_p_rsync() {
        protos.push(Protocol::Rsync);
    }
    s.filter.protocols = protos;
    s.filter.max_age_hours = match ui.get_age_hours() {
        0 => None,
        h => Some(h as f64),
    };
    s.filter.min_completion_pct = ui.get_completion() as f64;
    s.filter.require_ipv4 = ui.get_ipv4();
    s.filter.require_ipv6 = ui.get_ipv6();
    s.filter.include_regex = ui.get_include_re().to_string();
    s.filter.exclude_regex = ui.get_exclude_re().to_string();
    s.number = ui.get_number().max(1) as usize;
    s.concurrency = ui.get_concurrency().max(1) as usize;
    s.timeout_secs = ui.get_timeout_secs().max(1) as u64;
}

/// Write `app.settings` into the filter widgets.
fn push(ui: &MainWindow, s: &Settings) {
    ui.set_p_https(s.filter.protocols.contains(&Protocol::Https));
    ui.set_p_http(s.filter.protocols.contains(&Protocol::Http));
    ui.set_p_rsync(s.filter.protocols.contains(&Protocol::Rsync));
    ui.set_age_hours(s.filter.max_age_hours.map(|h| h as i32).unwrap_or(0));
    ui.set_completion(s.filter.min_completion_pct as i32);
    ui.set_ipv4(s.filter.require_ipv4);
    ui.set_ipv6(s.filter.require_ipv6);
    ui.set_include_re(s.filter.include_regex.clone().into());
    ui.set_exclude_re(s.filter.exclude_regex.clone().into());
    ui.set_number(s.number as i32);
    ui.set_concurrency(s.concurrency as i32);
    ui.set_timeout_secs(s.timeout_secs as i32);
}

fn country_selected(f: &FilterConfig, name: &str, code: &str) -> bool {
    f.countries
        .iter()
        .any(|c| c.eq_ignore_ascii_case(name) || c.eq_ignore_ascii_case(code))
}

/// Recompute filtered list, sort, countries, rows, preview and status.
fn refresh(ui: &MainWindow, app: &mut App) {
    let now = Utc::now();
    // country counts: everything except the country filter itself
    let mut f_nocountry = app.settings.filter.clone();
    f_nocountry.countries.clear();
    match filter_mirrors(&app.all, &f_nocountry, now) {
        Ok(v) => {
            let mut counts: HashMap<(String, String), usize> = HashMap::new();
            for m in v.iter().filter(|m| !m.country.is_empty()) {
                *counts
                    .entry((m.country.clone(), m.country_code.clone()))
                    .or_default() += 1;
            }
            let mut c: Vec<_> = counts
                .into_iter()
                .map(|((n, code), c)| (n, code, c))
                .collect();
            c.sort();
            app.country_counts = c;
            ui.set_message_error(false);
        }
        Err(e) => {
            set_msg(ui, format!("Filter error: {e}"), true);
            return;
        }
    }
    match filter_mirrors(&app.all, &app.settings.filter, now) {
        Ok(v) => app.list = v,
        Err(e) => {
            set_msg(ui, format!("Filter error: {e}"), true);
            return;
        }
    }
    let rates = rates_of(app);
    sort_mirrors(
        &mut app.list,
        app.settings.sort,
        app.settings.order,
        &rates,
        now,
    );

    // countries sidebar
    let search = ui.get_country_search().to_lowercase();
    let crows: Vec<CountryRow> = app
        .country_counts
        .iter()
        .filter(|(n, c, _)| {
            search.is_empty()
                || n.to_lowercase().contains(&search)
                || c.to_lowercase().contains(&search)
        })
        .map(|(n, c, k)| CountryRow {
            name: n.clone().into(),
            count: *k as i32,
            checked: country_selected(&app.settings.filter, n, c),
        })
        .collect();
    app.countries_model.set_vec(crows);

    // rows
    let sel: std::collections::HashSet<usize> =
        select_indices(&app.list, app.settings.number, &app.overrides)
            .into_iter()
            .collect();
    let rows: Vec<Row> = app
        .list
        .iter()
        .enumerate()
        .map(|(i, m)| make_row(m, sel.contains(&i), app.speeds.get(&m.url), now))
        .collect();
    app.rows_model.set_vec(rows);
    ui.set_sort_col(key_col(app.settings.sort));
    ui.set_sort_desc(app.settings.order == SortOrder::Descending);
    update_preview(ui, app);
}

fn selected(app: &App) -> Vec<&Mirror> {
    select_indices(&app.list, app.settings.number, &app.overrides)
        .into_iter()
        .map(|i| &app.list[i])
        .collect()
}

fn preview_text(app: &App) -> String {
    let sort = format!("{:?} ({:?})", app.settings.sort, app.settings.order).to_lowercase();
    let filters = format!(
        "{} top={}",
        app.settings.filter.describe(),
        app.settings.number
    );
    generate(
        &selected(app),
        &HeaderInfo {
            when: Utc::now(),
            filters: &filters,
            sort: &sort,
            source: STATUS_URL,
        },
    )
}

fn update_preview(ui: &MainWindow, app: &App) {
    ui.set_preview(preview_text(app).into());
    update_status(ui, app);
}

fn update_status(ui: &MainWindow, app: &App) {
    let tested = app
        .speeds
        .values()
        .filter(|s| matches!(s, Speed::Rate(_)))
        .count();
    ui.set_status_text(
        format!(
            "Total {} · After filter {} · Selected {} · Tested {}",
            app.all.len(),
            app.list.len(),
            selected(app).len(),
            tested
        )
        .into(),
    );
    ui.set_test_done(app.test_done as i32);
    ui.set_test_total(app.test_total as i32);
}

fn refresh_presets(app: &App) {
    app.presets_model.set_vec(
        app.config
            .presets
            .keys()
            .map(|k| k.as_str().into())
            .collect::<Vec<SharedString>>(),
    );
}

fn save_config(app: &mut App) -> Result<(), String> {
    app.config.last = app.settings.clone();
    match &app.config_path {
        Some(p) => app.config.save(p).map_err(|e| e.to_string()),
        None => Err("no config directory available".into()),
    }
}

fn do_fetch(ui: &MainWindow, app: &mut App) {
    ui.set_busy_fetch(true);
    set_msg(ui, "Fetching mirror status…", false);
    let weak = ui.as_weak();
    app.rt.spawn(async move {
        let res = async {
            let c = http_client(Duration::from_secs(10))?;
            fetch_status(&c, None).await
        }
        .await;
        on_ui(&weak, move |ui, app| {
            ui.set_busy_fetch(false);
            match res {
                Ok(doc) => {
                    app.all = doc.urls;
                    app.speeds.clear();
                    refresh(ui, app);
                    set_msg(ui, format!("Loaded {} mirrors", app.all.len()), false);
                }
                Err(e) => set_msg(ui, format!("Fetch failed: {e}"), true),
            }
        });
    });
}

fn do_speedtest(ui: &MainWindow, app: &mut App) {
    if app.cancel.is_some() {
        return;
    }
    let bases: Vec<String> = app
        .list
        .iter()
        .filter(|m| m.protocol != Protocol::Rsync)
        .map(|m| m.url.clone())
        .collect();
    if bases.is_empty() {
        set_msg(ui, "No http(s) mirrors to test", true);
        return;
    }
    for m in app.list.iter().filter(|m| m.protocol == Protocol::Rsync) {
        app.speeds
            .insert(m.url.clone(), Speed::Failed("n/a (rsync)".into()));
    }
    for b in &bases {
        app.speeds.remove(b);
    }
    let cancel = CancellationToken::new();
    app.cancel = Some(cancel.clone());
    app.test_done = 0;
    app.test_total = bases.len();
    ui.set_busy_test(true);
    set_msg(ui, format!("Testing {} mirrors…", bases.len()), false);
    refresh(ui, app);

    let opts = SpeedOptions {
        concurrency: app.settings.concurrency,
        timeout: Duration::from_secs(app.settings.timeout_secs),
    };
    let weak = ui.as_weak();
    let weak_ev = weak.clone();
    app.rt.spawn(async move {
        let client = match http_client(Duration::from_secs(opts.timeout.as_secs().max(1))) {
            Ok(c) => c,
            Err(e) => {
                on_ui(&weak, move |ui, app| {
                    app.cancel = None;
                    ui.set_busy_test(false);
                    set_msg(ui, format!("Speed test failed to start: {e}"), true);
                });
                return;
            }
        };
        run_speedtest(client, bases, opts, cancel, move |ev| {
            on_ui(&weak_ev, move |ui, app| handle_speed_event(ui, app, ev));
        })
        .await;
        on_ui(&weak, |ui, app| {
            let cancelled = app.cancel.as_ref().is_some_and(|c| c.is_cancelled());
            app.cancel = None;
            ui.set_busy_test(false);
            // Anything still marked as testing was cancelled.
            app.speeds.retain(|_, v| !matches!(v, Speed::Testing));
            refresh(ui, app);
            let ok = app
                .speeds
                .values()
                .filter(|s| matches!(s, Speed::Rate(_)))
                .count();
            set_msg(
                ui,
                if cancelled {
                    format!("Speed test cancelled ({ok} results kept)")
                } else {
                    format!("Speed test finished: {ok} mirrors measured")
                },
                false,
            );
        });
    });
}

fn handle_speed_event(ui: &MainWindow, app: &mut App, ev: SpeedEvent) {
    let (url, new) = match ev {
        SpeedEvent::Started(u) => (u, Some(Speed::Testing)),
        SpeedEvent::Finished(u, out) => {
            app.test_done += 1;
            let s = match out {
                SpeedOutcome::Rate(r) => Some(Speed::Rate(r)),
                SpeedOutcome::Timeout => Some(Speed::Failed("timeout".into())),
                SpeedOutcome::Error(e) => Some(Speed::Failed(format!("error: {e}"))),
                SpeedOutcome::Cancelled => None,
            };
            (u, s)
        }
    };
    match new {
        Some(s) => {
            app.speeds.insert(url.clone(), s);
        }
        None => {
            app.speeds.remove(&url);
        }
    }
    if app.settings.sort == SortKey::Rate && !matches!(app.speeds.get(&url), Some(Speed::Testing)) {
        refresh(ui, app);
        return;
    }
    if let Some(i) = app.list.iter().position(|m| m.url == url) {
        if let Some(mut row) = app.rows_model.row_data(i) {
            let (t, st) = speed_cells(app.speeds.get(&url));
            row.speed = t.into();
            row.state = st;
            app.rows_model.set_row_data(i, row);
        }
    }
    update_status(ui, app);
}

fn do_write_system(ui: &MainWindow, app: &mut App) {
    let sel = selected(app);
    if sel.is_empty() {
        set_msg(ui, "Nothing selected to write", true);
        return;
    }
    let text = preview_text(app);
    if let Err(e) = save_config(app) {
        eprintln!("warning: could not save config: {e}");
    }
    ui.set_busy_write(true);
    set_msg(ui, "Waiting for authorization…", false);
    let weak = ui.as_weak();
    app.rt.spawn(async move {
        let res =
            tokio::task::spawn_blocking(move || privileged::run_helper(&["write"], &text)).await;
        on_ui(&weak, move |ui, _| {
            ui.set_busy_write(false);
            match res {
                Ok(Ok(out)) => set_msg(
                    ui,
                    format!(
                        "Wrote {SYSTEM_MIRRORLIST} ({})",
                        out.replace('\n', "; ").trim_end_matches("; ")
                    ),
                    false,
                ),
                Ok(Err(e)) => set_msg(ui, format!("Write failed: {e}"), true),
                Err(e) => set_msg(ui, format!("Write failed: {e}"), true),
            }
        });
    });
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(p)
}

fn do_save_file(ui: &MainWindow, app: &mut App) {
    let path = ui.get_save_path().to_string();
    if path.trim().is_empty() {
        set_msg(ui, "Enter a file path first", true);
        return;
    }
    let p = expand_home(path.trim());
    let text = preview_text(app);
    let tmp = p.with_extension("mirrorscope-tmp");
    let res = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &p));
    match res {
        Ok(()) => set_msg(ui, format!("Saved {}", p.display()), false),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            set_msg(ui, format!("Save failed: {e}"), true)
        }
    }
}

fn config_path_string(app: &App) -> Option<String> {
    app.config_path.as_ref().map(|p| p.display().to_string())
}

fn refresh_backups(ui: &MainWindow, app: &mut App) {
    match list_backups(std::path::Path::new(PACMAN_DIR)) {
        Ok(b) => {
            app.backups = b;
            app.backups_model.set_vec(
                app.backups
                    .iter()
                    .map(|b| BackupRow {
                        name: b.name.clone().into(),
                        size: format!("{} B", b.size).into(),
                    })
                    .collect::<Vec<_>>(),
            );
            ui.set_selected_backup(-1);
            ui.set_diff_text("".into());
        }
        Err(e) => set_msg(ui, format!("Cannot list backups: {e}"), true),
    }
}

fn wire(ui: &MainWindow) {
    let w = ui.as_weak();
    macro_rules! cb {
        ($setter:ident, |$ui:ident, $app:ident $(, $arg:ident)*| $body:block) => {{
            let w = w.clone();
            ui.$setter(move |$($arg),*| {
                if let Some($ui) = w.upgrade() {
                    with_app(|$app| $body);
                }
            });
        }};
    }
    cb!(on_filters_changed, |ui, app| {
        pull(&ui, app);
        refresh(&ui, app);
    });
    cb!(on_country_search_changed, |ui, app| {
        refresh(&ui, app);
    });
    cb!(on_country_toggled, |ui, app, i, on| {
        if let Some(row) = app.countries_model.row_data(i as usize) {
            let name = row.name.to_string();
            let c = &mut app.settings.filter.countries;
            // Match by name or code, case-insensitively.
            let code = app
                .country_counts
                .iter()
                .find(|(n, _, _)| *n == name)
                .map(|(_, c, _)| c.clone())
                .unwrap_or_default();
            c.retain(|x| !x.eq_ignore_ascii_case(&name) && !x.eq_ignore_ascii_case(&code));
            if on {
                c.push(name);
            }
            refresh(&ui, app);
        }
    });
    cb!(on_countries_all, |ui, app, on| {
        let search = ui.get_country_search().to_lowercase();
        let visible: Vec<(String, String)> = app
            .country_counts
            .iter()
            .filter(|(n, c, _)| {
                search.is_empty()
                    || n.to_lowercase().contains(&search)
                    || c.to_lowercase().contains(&search)
            })
            .map(|(n, c, _)| (n.clone(), c.clone()))
            .collect();
        for (n, c) in visible {
            app.settings
                .filter
                .countries
                .retain(|x| !x.eq_ignore_ascii_case(&n) && !x.eq_ignore_ascii_case(&c));
            if on {
                app.settings.filter.countries.push(n);
            }
        }
        refresh(&ui, app);
    });
    cb!(on_fetch, |ui, app| {
        do_fetch(&ui, app);
    });
    cb!(on_start_test, |ui, app| {
        pull(&ui, app);
        do_speedtest(&ui, app);
    });
    cb!(on_cancel_test, |ui, app| {
        if let Some(c) = &app.cancel {
            c.cancel();
            set_msg(&ui, "Cancelling…", false);
        }
    });
    cb!(on_write_system, |ui, app| {
        do_write_system(&ui, app);
    });
    cb!(on_save_file, |ui, app| {
        do_save_file(&ui, app);
    });
    cb!(on_row_toggled, |ui, app, i, on| {
        if let Some(m) = app.list.get(i as usize) {
            app.overrides.insert(m.url.clone(), on);
            // Only the preview and checkbox states change; rebuild is cheap.
            refresh(&ui, app);
        }
    });
    cb!(on_sort_by, |ui, app, col| {
        if let Some(key) = col_key(col) {
            if app.settings.sort == key {
                app.settings.order = app.settings.order.flip();
            } else {
                app.settings.sort = key;
                app.settings.order = key.default_order();
            }
            refresh(&ui, app);
        }
    });
    cb!(on_preset_picked, |ui, app, name| {
        if let Some(p) = app.config.presets.get(name.as_str()).cloned() {
            app.settings = p;
            app.overrides.clear();
            push(&ui, &app.settings);
            ui.set_preset_name(name);
            refresh(&ui, app);
            set_msg(&ui, "Preset loaded", false);
        }
    });
    cb!(on_preset_save, |ui, app| {
        let name = ui.get_preset_name().to_string();
        if !valid_preset_name(&name) {
            set_msg(
                &ui,
                "Preset name: 1-32 characters, letters, digits, '-' or '_'",
                true,
            );
        } else {
            pull(&ui, app);
            app.config
                .presets
                .insert(name.clone(), app.settings.clone());
            refresh_presets(app);
            match save_config(app) {
                Ok(()) => set_msg(&ui, format!("Preset {name:?} saved"), false),
                Err(e) => set_msg(&ui, format!("Could not save config: {e}"), true),
            }
        }
    });
    cb!(on_preset_delete, |ui, app| {
        let name = ui.get_preset_name().to_string();
        if app.config.presets.remove(&name).is_some() {
            refresh_presets(app);
            let _ = save_config(app);
            set_msg(&ui, format!("Preset {name:?} deleted"), false);
        } else {
            set_msg(&ui, "No such preset", true);
        }
    });
    cb!(on_theme_changed, |ui, app, idx| {
        ui.set_theme_index(idx);
        app.config.theme = match idx {
            1 => Theme::Dark,
            2 => Theme::Light,
            _ => Theme::System,
        };
        ui.invoke_apply_theme();
    });
    cb!(on_backups_refresh, |ui, app| {
        refresh_backups(&ui, app);
    });
    cb!(on_backup_diff, |ui, app, i| {
        if let Some(b) = app.backups.get(i as usize) {
            let cur = std::fs::read_to_string(SYSTEM_MIRRORLIST).unwrap_or_default();
            match std::fs::read_to_string(&b.path) {
                Ok(old) => ui.set_diff_text(diff_text(&cur, &old).into()),
                Err(e) => set_msg(&ui, format!("Cannot read backup: {e}"), true),
            }
        }
    });
    cb!(on_backup_restore, |ui, app, i| {
        if let Some(b) = app.backups.get(i as usize) {
            let name = b.name.clone();
            set_msg(&ui, "Waiting for authorization…", false);
            let weak = ui.as_weak();
            app.rt.spawn(async move {
                let n = name.clone();
                let res = tokio::task::spawn_blocking(move || {
                    privileged::run_helper(&["restore", &n], "")
                })
                .await;
                on_ui(&weak, move |ui, app| match res {
                    Ok(Ok(_)) => {
                        set_msg(ui, format!("Restored {name}"), false);
                        refresh_backups(ui, app);
                    }
                    Ok(Err(e)) => set_msg(ui, format!("Restore failed: {e}"), true),
                    Err(e) => set_msg(ui, format!("Restore failed: {e}"), true),
                });
            });
        }
    });
    cb!(on_units_generate, |ui, app| {
        let preset = ui.get_timer_preset().to_string();
        if !app.config.presets.contains_key(&preset) {
            set_msg(&ui, "Save a preset with that name first", true);
            return;
        }
        let Some(cfg) = config_path_string(app) else {
            set_msg(&ui, "No config path available", true);
            return;
        };
        if let Err(e) = save_config(app) {
            set_msg(&ui, format!("Could not save config: {e}"), true);
            return;
        }
        match units::generate_units(&preset, &cfg) {
            Ok(u) => {
                ui.set_unit_text(
                    format!(
                        "# /etc/systemd/system/{}\n{}\n# /etc/systemd/system/{}\n{}",
                        units::SERVICE_NAME,
                        u.service,
                        units::TIMER_NAME,
                        u.timer
                    )
                    .into(),
                );
                app.units_preset = Some(preset);
                set_msg(&ui, "Review the unit files, then install", false);
            }
            Err(e) => set_msg(&ui, e.to_string(), true),
        }
    });
    cb!(on_units_install, |ui, app| {
        let (Some(preset), Some(cfg)) = (app.units_preset.clone(), config_path_string(app)) else {
            set_msg(&ui, "Generate the unit files first", true);
            return;
        };
        set_msg(&ui, "Waiting for authorization…", false);
        let weak = ui.as_weak();
        app.rt.spawn(async move {
            let res = tokio::task::spawn_blocking(move || {
                privileged::run_helper(&["install-timer", &preset, &cfg], "")
            })
            .await;
            on_ui(&weak, move |ui, _| match res {
                Ok(Ok(_)) => set_msg(ui, "Timer installed and enabled", false),
                Ok(Err(e)) => set_msg(ui, format!("Install failed: {e}"), true),
                Err(e) => set_msg(ui, format!("Install failed: {e}"), true),
            });
        });
    });
    cb!(on_units_remove, |ui, app| {
        set_msg(&ui, "Waiting for authorization…", false);
        let weak = ui.as_weak();
        app.rt.spawn(async move {
            let res =
                tokio::task::spawn_blocking(|| privileged::run_helper(&["remove-timer"], "")).await;
            on_ui(&weak, move |ui, _| match res {
                Ok(Ok(_)) => set_msg(ui, "Timer removed", false),
                Ok(Err(e)) => set_msg(ui, format!("Remove failed: {e}"), true),
                Err(e) => set_msg(ui, format!("Remove failed: {e}"), true),
            });
        });
    });
}

fn run_gui() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(512 * 1024)
        .enable_all()
        .build()?;
    let config_path = default_config_path();
    let (config, cfg_err) = match config_path.as_deref().map(Config::load) {
        Some(Ok(c)) => (c, None),
        Some(Err(e)) => (Config::default(), Some(e.to_string())),
        None => (Config::default(), None),
    };
    let ui = MainWindow::new()?;
    let app = App {
        rt,
        settings: config.last.clone(),
        config,
        config_path,
        all: Vec::new(),
        list: Vec::new(),
        speeds: HashMap::new(),
        overrides: HashMap::new(),
        country_counts: Vec::new(),
        backups: Vec::new(),
        cancel: None,
        test_done: 0,
        test_total: 0,
        units_preset: None,
        rows_model: Rc::new(VecModel::default()),
        countries_model: Rc::new(VecModel::default()),
        backups_model: Rc::new(VecModel::default()),
        presets_model: Rc::new(VecModel::default()),
    };
    ui.set_rows(ModelRc::from(app.rows_model.clone()));
    ui.set_countries(ModelRc::from(app.countries_model.clone()));
    ui.set_backups(ModelRc::from(app.backups_model.clone()));
    ui.set_preset_names(ModelRc::from(app.presets_model.clone()));
    refresh_presets(&app);
    push(&ui, &app.settings);
    ui.set_theme_index(match app.config.theme {
        Theme::System => 0,
        Theme::Dark => 1,
        Theme::Light => 2,
    });
    if let Some(home) = std::env::var_os("HOME") {
        ui.set_save_path(
            PathBuf::from(home)
                .join("mirrorlist")
                .display()
                .to_string()
                .into(),
        );
    }
    ui.invoke_apply_theme();
    ui.set_sort_col(key_col(app.settings.sort));
    ui.set_sort_desc(app.settings.order == SortOrder::Descending);
    update_status(&ui, &app);
    if let Some(e) = cfg_err {
        set_msg(&ui, format!("Config ignored: {e}"), true);
    }
    APP.with(|a| *a.borrow_mut() = Some(app));
    wire(&ui);

    // Persist the last-used settings on close.
    ui.window().on_close_requested({
        let w = ui.as_weak();
        move || {
            if let Some(ui) = w.upgrade() {
                with_app(|app| {
                    pull(&ui, app);
                    if let Err(e) = save_config(app) {
                        eprintln!("warning: could not save config: {e}");
                    }
                });
            }
            slint::CloseRequestResponse::HideWindow
        }
    });

    with_app(|app| do_fetch(&ui, app));
    ui.run()?;
    APP.with(|a| a.borrow_mut().take());
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().skip(1).any(|a| a == "--cli") {
        std::process::exit(mirrorscope_cli::run(args));
    }
    if let Err(e) = run_gui() {
        eprintln!("mirrorscope: {e:#}");
        std::process::exit(1);
    }
}
