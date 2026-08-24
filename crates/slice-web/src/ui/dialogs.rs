//! The modal dialogs: About, the error report, and the progress indicator.

use leptos::prelude::*;

use crate::state::AppState;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit this was built from, stamped by `build.rs`.
///
/// `unknown` when there was no git to ask, and suffixed with `+` when the tree had
/// uncommitted changes — in which case it is not the commit it names and must not be
/// offered as a link to one.
pub const COMMIT: &str = env!("SLICE_COMMIT");

/// Where the repository lives, for turning a commit into something clickable.
pub const REPOSITORY: &str = "https://github.com/felipesanches/slice-web";

/// The commit as a person should read it: seven characters, or a word saying why not.
pub fn commit_label() -> String {
    label_for(COMMIT)
}

/// The commit's URL, or `None` when there is nothing honest to link to.
pub fn commit_url() -> Option<String> {
    url_for(COMMIT)
}

/// The two above, as functions of their input rather than of the build, so that the
/// cases that matter can be tested without arranging three different builds to get them.
fn label_for(commit: &str) -> String {
    if commit == "unknown" {
        return "unknown build".into();
    }
    let short: String = commit.chars().take(7).collect();
    if commit.ends_with('+') {
        format!("{short} (modified)")
    } else {
        short
    }
}

/// A build from a dirty tree gets no link. The commit it names exists, but the code
/// running is not that code, and a link would quietly claim otherwise -- which is the
/// one failure this whole mechanism is meant to prevent.
fn url_for(commit: &str) -> Option<String> {
    if commit == "unknown" || commit.ends_with('+') {
        return None;
    }
    Some(format!("{REPOSITORY}/commit/{commit}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN: &str = "0ffd3fd92c7614acbeaf3511ee07a9bf3ea45360";

    #[test]
    fn a_clean_build_links_to_its_full_commit() {
        // Full hash in the href, short one on screen: the link has to resolve, the label
        // has to fit in a status bar.
        assert_eq!(label_for(CLEAN), "0ffd3fd");
        assert_eq!(
            url_for(CLEAN).unwrap(),
            format!("{REPOSITORY}/commit/{CLEAN}")
        );
    }

    #[test]
    fn a_modified_tree_is_never_linked_to_a_commit() {
        let dirty = format!("{CLEAN}+");
        assert_eq!(url_for(&dirty), None);
        assert_eq!(label_for(&dirty), "0ffd3fd (modified)");
    }

    #[test]
    fn a_build_with_no_git_says_so_rather_than_inventing_one() {
        assert_eq!(url_for("unknown"), None);
        assert_eq!(label_for("unknown"), "unknown build");
    }

    #[test]
    fn the_stamp_this_was_built_with_is_one_of_the_three_shapes() {
        // Guards the build script's contract from the other side: whatever it emitted for
        // *this* build must be something the interface can render.
        assert!(
            COMMIT == "unknown"
                || (COMMIT.trim_end_matches('+').len() == 40
                    && COMMIT
                        .trim_end_matches('+')
                        .chars()
                        .all(|c| c.is_ascii_hexdigit())),
            "build.rs emitted an unusable commit stamp: {COMMIT:?}"
        );
    }
}

/// The error dialog: one sentence, with the technical detail behind a disclosure.
///
/// This is the shape the original uses, and it is the right one: the sentence is for the
/// person, and the detail is for whoever they forward it to.
#[component]
pub fn ErrorDialog(state: AppState) -> impl IntoView {
    view! {
        <Show when=move || state.error.get().is_some()>
            {move || {
                let message = state.error.get().expect("guarded by Show");
                view! {
                    <div class="modal-backdrop" on:click=move |_| state.clear_error()>
                        <div
                            class="modal error"
                            role="alertdialog"
                            aria-modal="true"
                            aria-labelledby="error-title"
                            on:click=|ev| ev.stop_propagation()
                        >
                            <h2 id="error-title">"Error"</h2>
                            <p>{message.summary.clone()}</p>
                            {message
                                .details
                                .clone()
                                .map(|details| {
                                    view! {
                                        <details>
                                            <summary>"Details"</summary>
                                            <pre>{details}</pre>
                                        </details>
                                    }
                                })}
                            <div class="buttons">
                                <button class="primary" on:click=move |_| state.clear_error()>
                                    "OK"
                                </button>
                            </div>
                        </div>
                    </div>
                }
            }}
        </Show>
    }
}

/// Shown while a slice runs.
///
/// The engine runs on the main thread, so this is painted before the work starts and
/// stays put until it finishes; it cannot animate meaningfully in between. It is
/// deliberately honest about that rather than showing a bar that pretends to move.
#[component]
pub fn ProgressDialog(state: AppState) -> impl IntoView {
    view! {
        <Show when=move || state.busy.get()>
            <div class="modal-backdrop">
                <div class="modal progress" role="status" aria-live="polite">
                    <h2>"Slicing…"</h2>
                    <div class="indeterminate"><div class="bar"></div></div>
                    <p class="hint">"The page will be unresponsive until this finishes."</p>
                </div>
            </div>
        </Show>
    }
}

#[component]
pub fn AboutDialog(state: AppState) -> impl IntoView {
    view! {
        <Show when=move || state.about_open.get()>
            <div class="modal-backdrop" on:click=move |_| state.about_open.set(false)>
                <div
                    class="modal about"
                    role="dialog"
                    aria-modal="true"
                    aria-labelledby="about-title"
                    on:click=|ev| ev.stop_propagation()
                >
                    <div class="about-head">
                        <SliceLogo/>
                        <h2 id="about-title">"Slice"</h2>
                    </div>
                    <p>
                        "Version " {VERSION} " · "
                        {move || match commit_url() {
                            Some(url) => {
                                view! {
                                    <a href=url target="_blank" rel="noreferrer">
                                        {commit_label()}
                                    </a>
                                }
                                    .into_any()
                            }
                            None => view! { <span>{commit_label()}</span> }.into_any(),
                        }}
                    </p>
                    <p class="about-lead">
                        "Builds custom design sub-spaces from variable fonts, in the "
                        "browser. Fonts are read and written locally; nothing is uploaded."
                    </p>
                    <p>
                        "A reimplementation of "
                        <a href="https://github.com/source-foundry/Slice" target="_blank" rel="noreferrer">
                            "Slice"
                        </a>
                        " by Source Foundry (Christopher Simpkins), which was a PyQt5 "
                        "desktop application built on fontTools. This version keeps its "
                        "interface, moves the engine to Rust and WebAssembly, and adds "
                        "overlap removal."
                    </p>
                    <h3>"Built with"</h3>
                    <ul class="credits">
                        <li>
                            <a href="https://github.com/googlefonts/fontations" target="_blank" rel="noreferrer">
                                "fontations"
                            </a>
                            " — read-fonts, write-fonts and skrifa"
                        </li>
                        <li>
                            <a href="https://crates.io/crates/linesweeper" target="_blank" rel="noreferrer">
                                "linesweeper"
                            </a>
                            " — a robust sweep line, for overlap removal"
                        </li>
                        <li>
                            <a href="https://github.com/linebender/kurbo" target="_blank" rel="noreferrer">
                                "kurbo"
                            </a>
                            " — curve geometry"
                        </li>
                        <li>
                            <a href="https://leptos.dev" target="_blank" rel="noreferrer">"Leptos"</a>
                            " — the interface"
                        </li>
                        <li>
                            "The sub-space solver is a port of the one in "
                            <a href="https://github.com/fonttools/fonttools" target="_blank" rel="noreferrer">
                                "fontTools"
                            </a>
                        </li>
                        <li>
                            "Fonts are remembered between visits with code adapted from "
                            <a
                                href="https://github.com/FontBureau/TypeRoof"
                                target="_blank"
                                rel="noreferrer"
                            >
                                "TypeRoof"
                            </a>
                            " by Font Bureau, under the Apache License 2.0"
                        </li>
                        // The application ships the icon, so its attribution has to be
                        // reachable from inside the application, not only from the
                        // repository's thirdparty/ directory.
                        <li>
                            "The icon is the original Slice project's, itself a derivative "
                            "of a "
                            <a
                                href="https://www.flaticon.com/free-icon/cheesecake_3400263"
                                target="_blank"
                                rel="noreferrer"
                            >
                                "cheesecake icon"
                            </a>
                            " by flaticon.com, used under the Flaticon license"
                        </li>
                    </ul>
                    <p class="licence">
                        "GNU General Public License v3 or later, as the original is."
                    </p>
                    <div class="buttons">
                        <button class="primary" on:click=move |_| state.about_open.set(false)>
                            "OK"
                        </button>
                    </div>
                </div>
            </div>
        </Show>
    }
}

/// The application mark, shared with the documentation site.
#[component]
pub fn SliceLogo() -> impl IntoView {
    // The original Slice project's icon, so this is recognisable as the same tool. It is
    // one file shared with the documentation site rather than inline markup: `build.sh`
    // copies `docs/assets/slice-icon.svg` into the build, so the app and the website
    // cannot drift into showing different logos.
    //
    // It is decorative here -- every place it appears sits beside the word "Slice" -- so
    // it carries an empty alt and is hidden from assistive technology rather than making
    // a screen reader announce the name twice.
    view! {
        <img class="logo" src="./slice-icon.svg" alt="" aria-hidden="true"/>
    }
}
