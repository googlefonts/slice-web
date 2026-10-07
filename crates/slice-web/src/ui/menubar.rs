//! The menu bar, carrying over the original's File / References / Help menus.

use leptos::prelude::*;

use crate::state::AppState;

#[component]
pub fn MenuBar(state: AppState, on_open: Callback<()>) -> impl IntoView {
    // Which menu is showing, by index; `None` when the bar is closed.
    let open = RwSignal::new(None::<usize>);

    let close = move || open.set(None);

    view! {
        <nav class="menubar" on:mouseleave=move |_| close()>
            <Menu index=0 open=open label="File">
                <button on:click=move |_| { close(); on_open.run(()); }>
                    "Open Font…"
                    <kbd>"Ctrl+O"</kbd>
                </button>
                <button
                    disabled=move || state.font.with(Option::is_none)
                    on:click=move |_| { close(); state.revert_editors(); }
                >
                    "Revert Editors"
                </button>
            </Menu>

            <Menu index=1 open=open label="References">
                <span class="menu-heading">"OpenType Specification"</span>
                <MenuLink
                    label="fvar Table"
                    href="https://learn.microsoft.com/en-us/typography/opentype/spec/fvar"
                />
                <MenuLink
                    label="gvar Table"
                    href="https://learn.microsoft.com/en-us/typography/opentype/spec/gvar"
                />
                <MenuLink
                    label="head Table"
                    href="https://learn.microsoft.com/en-us/typography/opentype/spec/head"
                />
                <MenuLink
                    label="name Table"
                    href="https://learn.microsoft.com/en-us/typography/opentype/spec/name"
                />
                <MenuLink
                    label="OS/2 Table"
                    href="https://learn.microsoft.com/en-us/typography/opentype/spec/os2"
                />
                <hr/>
                <MenuLink
                    label="Google Fonts Axis Registry"
                    href="https://fonts.google.com/variablefonts#axis-definitions"
                />
            </Menu>

            <Menu index=2 open=open label="Help">
                <button on:click=move |_| { close(); state.about_open.set(true); }>
                    "About…"
                </button>
                <MenuLink
                    label="Documentation"
                    href="https://github.com/googlefonts/slice-web#readme"
                />
                <MenuLink
                    label="Release Notes"
                    href="https://github.com/googlefonts/slice-web/releases"
                />
                <hr/>
                <MenuLink
                    label="View License"
                    href="https://github.com/googlefonts/slice-web/blob/main/LICENSE"
                />
                <MenuLink
                    label="View Source"
                    href="https://github.com/googlefonts/slice-web"
                />
                <hr/>
                <MenuLink
                    label="Issue Tracker"
                    href="https://github.com/googlefonts/slice-web/issues"
                />
                <MenuLink
                    label="Report a Bug"
                    href="https://github.com/googlefonts/slice-web/issues/new"
                />
                <hr/>
                <MenuLink
                    label="The original Slice"
                    href="https://github.com/source-foundry/Slice"
                />
                <MenuLink
                    label="Original documentation"
                    href="https://slice-gui.netlify.app/docs/"
                />
            </Menu>
        </nav>
    }
}

#[component]
fn Menu(
    index: usize,
    open: RwSignal<Option<usize>>,
    label: &'static str,
    children: Children,
) -> impl IntoView {
    let is_open = move || open.get() == Some(index);
    view! {
        <div class="menu" class:open=is_open>
            <button
                class="menu-title"
                aria-haspopup="true"
                aria-expanded=move || is_open().to_string()
                on:click=move |_| {
                    open.update(|current| {
                        *current = if *current == Some(index) { None } else { Some(index) };
                    })
                }
                // Once one menu is open, moving across the bar should switch to the
                // next, the way a native menu bar behaves.
                on:mouseenter=move |_| {
                    if open.get().is_some() {
                        open.set(Some(index));
                    }
                }
            >
                {label}
            </button>
            // Rendered always and hidden with CSS rather than mounted on demand: the
            // children are built once, and there is nothing here worth the churn of
            // tearing down and rebuilding on every open.
            <div class="menu-items" class:open=is_open role="menu">
                {children()}
            </div>
        </div>
    }
}

#[component]
fn MenuLink(label: &'static str, href: &'static str) -> impl IntoView {
    view! {
        <a href=href target="_blank" rel="noreferrer" role="menuitem">
            {label}
            <span class="external" aria-hidden="true">"↗"</span>
        </a>
    }
}
