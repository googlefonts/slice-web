//! The application shell: layout, the font drop zone, and the Slice button.

use leptos::prelude::*;
use leptos::task::spawn_local;
use slice_core::OutputFormat;
use wasm_bindgen::JsCast;
use web_sys::{HtmlInputElement, HtmlSelectElement};

use crate::files;
use crate::state::AppState;
use crate::ui::dialogs::{
    commit_label, commit_url, AboutDialog, ErrorDialog, ProgressDialog, SliceLogo, VERSION,
};
use crate::ui::editors::{AxisEditor, BitFlagEditor, NameEditor};
use crate::ui::menubar::MenuBar;

#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    let file_input: NodeRef<leptos::html::Input> = NodeRef::new();

    // Opening a file, from the button, the menu, or a drop.
    let open_dialog = Callback::new(move |_: ()| {
        if let Some(input) = file_input.get() {
            input.click();
        }
    });

    let accept_file = move |file: web_sys::File| {
        spawn_local(async move {
            match files::read_file(file).await {
                Ok((name, bytes)) => {
                    state.load_font(name, bytes);
                    remember(state);
                }
                Err(message) => state.report("The file could not be read.", Some(message)),
            }
        });
    };

    let on_file_chosen = move |ev: web_sys::Event| {
        let input: HtmlInputElement = ev.target().unwrap().unchecked_into();
        if let Some(files) = input.files() {
            if let Some(file) = files.get(0) {
                accept_file(file);
            }
        }
        // Clear the value so choosing the same file twice still fires a change event.
        input.set_value("");
    };

    let load_sample = Callback::new(move |_: ()| {
        spawn_local(async move {
            match files::fetch_same_origin(files::SAMPLE_PATH).await {
                Ok(bytes) => {
                    state.load_font(files::SAMPLE_NAME.to_string(), bytes);
                    remember(state);
                }
                Err(message) => state.report("The sample font could not be loaded.", Some(message)),
            }
        });
    });

    // The remembered fonts, refreshed whenever the store changes underneath us.
    let recent = state.recent;
    spawn_local(async move {
        recent.set(crate::recent::list().await);
    });

    // Choosing one loads the bytes *and* the settings that were last used with it, which
    // is the difference between this and opening the file again.
    let recall = Callback::new(move |id: String| {
        spawn_local(async move {
            let Some(bytes) = crate::recent::get(&id).await else {
                // Evicted between the list being drawn and the click landing.
                recent.set(crate::recent::list().await);
                return;
            };
            let name = recent.with_untracked(|fonts| {
                fonts
                    .iter()
                    .find(|f| f.id == id)
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| "font.ttf".to_string())
            });
            let settings = recent.with_untracked(|fonts| {
                fonts
                    .iter()
                    .find(|f| f.id == id)
                    .map(|f| f.settings.clone())
                    .unwrap_or_default()
            });
            state.load_font(name, bytes);
            if state.font.with_untracked(Option::is_some) && !settings.is_empty() {
                let settings = crate::settings::Settings::from_query(&settings);
                state.apply_settings(&settings);
                crate::settings::write_to_location(&settings);
            }
        });
    });

    let forget = Callback::new(move |id: String| {
        spawn_local(async move {
            crate::recent::forget(&id).await;
            recent.set(crate::recent::list().await);
        });
    });

    // A tool that keeps copies of someone's fonts owes them a way to say stop.
    let forget_all = Callback::new(move |_: ()| {
        spawn_local(async move {
            crate::recent::forget_all().await;
            recent.set(crate::recent::list().await);
        });
    });

    // Settings the page was opened with, applied as soon as there is a font to apply them
    // to. Held rather than consumed, so that opening a second font restores them again --
    // a bookmarked weight is a thing you want for the next font too, and the address bar
    // still says so.
    let pending = StoredValue::new(crate::settings::from_location());
    Effect::new(move |_| {
        // Reruns whenever a font is loaded, because `axes` is read here.
        let axes = state.axes.get();
        if axes.is_empty() {
            return;
        }
        pending.with_value(|settings| {
            if !settings.is_empty() {
                state.apply_settings(settings);
            }
        });
    });

    // `?sample` in the URL opens the bundled font on start, so a link can show the tool
    // already working. It is also what the browser smoke test drives.
    if files::wants_sample() {
        load_sample.run(());
    }

    // The File menu advertises Ctrl+O, so it has to work. Bound on the document rather
    // than the app element, because the shortcut should fire wherever focus happens to
    // be -- except inside a text field, where Ctrl+O is the browser's to interpret.
    {
        use wasm_bindgen::closure::Closure;
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            let handler = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
                move |event: web_sys::KeyboardEvent| {
                    if (event.ctrl_key() || event.meta_key())
                        && !event.alt_key()
                        && event.key().eq_ignore_ascii_case("o")
                    {
                        event.prevent_default();
                        open_dialog.run(());
                    }
                },
            );
            let _ = document
                .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
            // The listener outlives this scope; the document owns it for the page's life.
            handler.forget();
        }
    }

    let dragging = RwSignal::new(false);

    let on_drop = move |ev: web_sys::DragEvent| {
        ev.prevent_default();
        dragging.set(false);
        if let Some(transfer) = ev.data_transfer() {
            if let Some(files) = transfer.files() {
                if let Some(file) = files.get(0) {
                    accept_file(file);
                }
            }
        }
    };

    view! {
        <div
            class="app"
            class:dragging=move || dragging.get()
            on:dragover=move |ev: web_sys::DragEvent| {
                ev.prevent_default();
                dragging.set(true);
            }
            on:dragleave=move |_| dragging.set(false)
            on:drop=on_drop
        >
            <MenuBar state=state on_open=open_dialog/>

            <header class="title">
                <SliceLogo/>
                <h1>"Slice"</h1>
                <p class="tagline">"Custom design sub-spaces from variable fonts"</p>
            </header>

            <main>
                <FontPathRow
                    state=state
                    open_dialog=open_dialog
                    load_sample=load_sample
                    recall=recall
                    forget=forget
                    forget_all=forget_all
                    dragging=dragging
                />
                <AxisEditor state=state/>
                <NameEditor state=state/>
                <BitFlagEditor state=state/>
                <OutlineOptions state=state/>
                <SliceButton state=state/>
                <ResultNotes state=state/>
            </main>

            <footer class="statusbar">
                <span class="status">{move || state.status.get()}</span>
                <span class="version">
                    "v" {VERSION}
                    // The commit this page was built from, so that what is running here
                    // can be read rather than guessed at. A build from a modified tree
                    // gets the label without the link -- see `commit_url`.
                    {move || match commit_url() {
                        Some(url) => {
                            view! {
                                <a
                                    class="commit"
                                    href=url
                                    target="_blank"
                                    rel="noreferrer"
                                    title="The commit this build came from"
                                >
                                    {commit_label()}
                                </a>
                            }
                                .into_any()
                        }
                        None => {
                            view! { <span class="commit">{commit_label()}</span> }.into_any()
                        }
                    }}
                </span>
            </footer>

            <input
                node_ref=file_input
                type="file"
                accept=files::ACCEPTED_EXTENSIONS
                class="hidden-file-input"
                on:change=on_file_chosen
            />

            <ErrorDialog state=state/>
            <AboutDialog state=state/>
            <ProgressDialog state=state/>
        </div>
    }
}

/// Remember the loaded font, with whatever the editors currently say.
///
/// Called when a font arrives and again when one is sliced. The first is what puts it in
/// the list at all -- and what dates it, since "when did I get this copy" is a question
/// about the file arriving, not about anything done to it afterwards. The second attaches
/// the settings; an empty settings string is treated by the store as "no opinion", so the
/// arrival does not erase what a previous session left there.
///
/// What is kept is the decoded sfnt, so a WOFF2 that was opened comes back as a plain
/// sfnt; the output format travels in the settings, so recalling it and pressing Slice
/// still produces a WOFF2.
fn remember(state: AppState) {
    let file_name = state.file_name.get_untracked();
    let query = state.capture_settings().to_query();
    let details = state.font.with_untracked(|font| {
        font.as_ref().map(|font| {
            (
                font.data().to_vec(),
                font.family_name().unwrap_or_default(),
                font.version().unwrap_or_default(),
            )
        })
    });
    let Some((bytes, family, version)) = details else {
        return;
    };
    spawn_local(async move {
        let id = crate::recent::identity(&file_name, &family, &bytes);
        crate::recent::put(&id, &file_name, &family, &version, &bytes, &query).await;
        state.recent.set(crate::recent::list().await);
    });
}

/// Fonts opened before, offered for instant recall.
///
/// This replaces a lone "try the sample" link, which was only ever useful once. What is
/// recalled is the font *and* the settings last used with it, so a weekly job of cutting
/// the same three instances is two clicks rather than a file dialogue and five fields.
///
/// The panel is absent, rather than empty, when there is nothing to show -- on a first
/// visit, and in a browser that will not give the page any storage.
#[component]
fn RecentFonts(
    state: AppState,
    recall: Callback<String>,
    forget: Callback<String>,
    forget_all: Callback<()>,
) -> impl IntoView {
    let recent = state.recent;
    view! {
        <Show when=move || !recent.get().is_empty()>
            <div class="recent">
                <h3>
                    "Opened before"
                    <button class="linklike forget-all" on:click=move |_| forget_all.run(())>
                        "Forget all"
                    </button>
                </h3>
                <ul>
                    <For
                        each=move || recent.get()
                        key=|font| font.id.clone()
                        let:font
                    >
                        {
                            // Everything the row shows, worked out before the view so the
                            // closures inside it do not have to share ownership of one
                            // record between an event handler, two `Show` predicates and
                            // half a dozen text nodes.
                            let id = font.id.clone();
                            let forget_id = font.id.clone();
                            let file_name = font.name.clone();
                            let label = font.label().to_string();
                            let aria = format!("Forget {label}");
                            let version = font.version.clone();
                            let has_version = !version.is_empty();
                            let when = font.added_label();
                            let age = font.age_label();
                            let stale = font.is_stale();
                            let stale_note =
                                format!("Cached {age} — worth checking for a newer release");

                            let settings = crate::settings::Settings::from_query(&font.settings);
                            let detail = if settings.axes.is_empty() {
                                font.size_label()
                            } else {
                                let axes = settings
                                    .axes
                                    .iter()
                                    .map(|(tag, value)| format!("{tag} {value}"))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                format!("{axes} · {}", font.size_label())
                            };

                            view! {
                                <li class:stale=move || stale>
                                    <button
                                        class="recall"
                                        title=file_name
                                        on:click=move |_| recall.run(id.clone())
                                    >
                                        <span class="label">
                                            {label}
                                            <Show when=move || has_version>
                                                <span class="version">{version.clone()}</span>
                                            </Show>
                                        </span>
                                        <span class="detail">{detail}</span>
                                        <span class="when" title=age>{when}</span>
                                        <Show when=move || stale>
                                            <span class="note">{stale_note.clone()}</span>
                                        </Show>
                                    </button>
                                    <button
                                        class="forget"
                                        title="Forget this font"
                                        aria-label=aria
                                        on:click=move |_| forget.run(forget_id.clone())
                                    >
                                        "\u{00d7}"
                                    </button>
                                </li>
                            }
                        }
                    </For>
                </ul>
            </div>
        </Show>
    }
}

/// The font path row: a drop target and an Open button.
#[component]
fn FontPathRow(
    state: AppState,
    open_dialog: Callback<()>,
    load_sample: Callback<()>,
    recall: Callback<String>,
    forget: Callback<String>,
    forget_all: Callback<()>,
    dragging: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <section class="group font-path">
            <div class="dropzone" class:active=move || dragging.get()>
                <Show
                    when=move || state.font.with(Option::is_some)
                    fallback=move || {
                        view! {
                            <p class="placeholder">
                                "Drop a variable font here, or click Open."
                                <button class="linklike" on:click=move |_| load_sample.run(())>
                                    "Try the sample font"
                                </button>
                            </p>
                            <RecentFonts state=state recall=recall forget=forget forget_all=forget_all/>
                        }
                    }
                >
                    <div class="loaded">
                        <strong>{move || state.file_name.get()}</strong>
                        <span class="detail">
                            {move || {
                                state
                                    .font
                                    .with(|font| {
                                        font.as_ref()
                                            .map(|font| {
                                                format!(
                                                    "{} glyphs · {} units per em · {}",
                                                    font.glyph_count(),
                                                    font.units_per_em(),
                                                    if font.is_truetype() {
                                                        "TrueType outlines"
                                                    } else {
                                                        "CFF outlines"
                                                    },
                                                )
                                            })
                                            .unwrap_or_default()
                                    })
                            }}
                        </span>
                    </div>
                </Show>
            </div>
            <button class="secondary" on:click=move |_| open_dialog.run(())>
                "Open"
            </button>
        </section>
    }
}

/// The overlap-removal option and the output format.
///
/// This group has no counterpart in the original, which never removed overlaps and
/// inferred the output format from the file name in a save dialog the browser does not
/// give us.
#[component]
fn OutlineOptions(state: AppState) -> impl IntoView {
    view! {
        <section class="group">
            <h2>"Outlines and Output"</h2>
            <label class="option">
                <input
                    type="checkbox"
                    prop:checked=move || state.remove_overlaps.get()
                    on:change=move |ev| state.remove_overlaps.set(event_target_checked(&ev))
                />
                <span>
                    <strong>"Remove overlapping contours"</strong>
                    <span class="detail">
                        "Merges each glyph's contours into one non-overlapping outline. "
                        "Needs every axis pinned, and drops hinting. Worth it because "
                        "design applications still handle overlaps badly."
                    </span>
                </span>
            </label>

            <label class="option format">
                <span><strong>"Output format"</strong></span>
                <select on:change=move |ev| {
                    let select: HtmlSelectElement = ev.target().unwrap().unchecked_into();
                    state
                        .format
                        .set(match select.value().as_str() {
                            "woff" => OutputFormat::Woff,
                            "woff2" => OutputFormat::Woff2,
                            _ => OutputFormat::Sfnt,
                        });
                }>
                    <option value="sfnt" selected=move || state.format.get() == OutputFormat::Sfnt>
                        "TrueType / OpenType (.ttf, .otf)"
                    </option>
                    <option value="woff" selected=move || state.format.get() == OutputFormat::Woff>
                        "WOFF (.woff)"
                    </option>
                    <option
                        value="woff2"
                        selected=move || state.format.get() == OutputFormat::Woff2
                    >
                        "WOFF2 (.woff2)"
                    </option>
                </select>
            </label>

            <p class="hint">
                "Saves as "
                <code>{move || state.suggested_output_name()}</code>
            </p>
        </section>
    }
}

/// The Slice button, and the work it starts.
#[component]
fn SliceButton(state: AppState) -> impl IntoView {
    let run = move |_| {
        if state.font.with(Option::is_none) {
            state.status.set("Requires a font path".to_string());
            return;
        }

        state.clear_error();
        state.busy.set(true);
        state.status.set("Slicing…".to_string());

        // Yield to the browser so the progress dialog is actually painted before the
        // engine takes over the main thread. Without this the dialog would only appear
        // after the work it is meant to cover had already finished.
        spawn_local(async move {
            next_tick().await;
            perform(state);
            state.busy.set(false);
        });
    };

    view! {
        <div class="actions">
            <button
                class="primary slice"
                disabled=move || state.busy.get() || state.font.with(Option::is_none)
                on:click=run
            >
                "Slice"
            </button>
        </div>
    }
}

/// Run the job and hand the result to the browser.
fn perform(state: AppState) {
    let job = match state.build_job() {
        Ok(job) => job,
        Err(e) => {
            state.report(&e.to_string(), None);
            return;
        }
    };

    let output = state.font.with(|font| {
        let font = font.as_ref().expect("guarded by the caller");
        job.run(font)
    });

    match output {
        Ok(output) => {
            let name = state.suggested_output_name();
            match files::download(&output.bytes, &name, job.format.mime_type()) {
                Ok(()) => {
                    state.status.set(format!("Saved {name}"));

                    // The address bar becomes a description of the slice that just ran,
                    // so the browser's own bookmark button saves the settings. Done here
                    // rather than on every keystroke: a URL that changes while you type
                    // is one you cannot copy.
                    let settings = state.capture_settings();
                    crate::settings::write_to_location(&settings);

                    // Store the settings against the font, so recalling it resumes
                    // here rather than at the font's own defaults.
                    remember(state);

                    let mut notes = output.notes;
                    if !settings.is_empty() {
                        notes.push(
                            "These settings are now in the address bar — bookmark the page \
                             to keep them."
                                .to_string(),
                        );
                    }
                    state.last_result.set(notes);
                }
                Err(message) => state.report("The font could not be saved.", Some(message)),
            }
        }
        Err(e) => {
            // Errors that name an axis or explain a rule read better as the summary;
            // there is no extra detail to hide behind a disclosure.
            state.report(
                "Font processing failed. See details below.",
                Some(e.to_string()),
            );
        }
    }
}

/// What the last successful slice did.
#[component]
fn ResultNotes(state: AppState) -> impl IntoView {
    view! {
        <Show when=move || !state.last_result.get().is_empty()>
            <section class="group notes">
                <h2>"Last slice"</h2>
                <ul>
                    <For
                        each=move || state.last_result.get()
                        key=|note| note.clone()
                        let:note
                    >
                        <li>{note}</li>
                    </For>
                </ul>
            </section>
        </Show>
    }
}

/// Resolve after the browser has had a chance to paint.
async fn next_tick() {
    use wasm_bindgen::prelude::*;
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().expect("a window");
        // Two frames: one to paint the dialog, one to be sure it is on screen.
        let inner = Closure::once_into_js(move || {
            let window = web_sys::window().expect("a window");
            window
                .request_animation_frame(resolve.unchecked_ref())
                .expect("request_animation_frame");
        });
        window
            .request_animation_frame(inner.unchecked_ref())
            .expect("request_animation_frame");
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
