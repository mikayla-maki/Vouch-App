# Vouch

Vouch is a local-first, privacy-preserving database of recommendations, by you and your trusted friends.

See [ARCHITECTURE.md](./ARCHITECTURE.md) for the full system design.

## Development Guidelines

### Rust Coding Guidelines

* Prioritize code correctness and clarity. Speed and efficiency are secondary priorities unless otherwise specified.
* Do not write organizational comments that summarize the code. Comments should only be written to explain "why" the code is written in some way when there is a tricky or non-obvious reason.
* Prefer implementing functionality in existing files unless it is a new logical component. Avoid creating many small files.
* Avoid using functions that panic like `unwrap()`, instead use mechanisms like `?` to propagate errors.
* Be careful with operations like indexing which may panic if the indexes are out of bounds.
* Never silently discard errors with `let _ =` on fallible operations. Always handle errors appropriately:
  - Propagate errors with `?` when the calling function should handle them
  - Log the errors or similar when you need to ignore errors but want visibility
  - Use explicit error handling with `match` or `if let Err(...)` when you need custom logic
* When implementing async operations that may fail, ensure errors propagate to the UI layer so users get meaningful feedback.
* Never create files with `mod.rs` paths - prefer `src/some_module.rs` instead of `src/some_module/mod.rs`.
* Use full words for variable names (no abbreviations like "q" for "queue").

### GPUI

GPUI is a GPU-accelerated UI framework which also provides primitives for state and concurrency management.

#### Context

Context types allow interaction with global state, windows, entities, and system services. They are typically passed to functions as the argument named `cx`. When a function takes callbacks they come after the `cx` parameter.

* `App` is the root context type, providing access to global state and read and update of entities.
* `Context<T>` is provided when updating an `Entity<T>`. This context dereferences into `App`, so functions which take `&App` can also take `&Context<T>`.
* `AsyncApp` and `AsyncWindowContext` are provided by `cx.spawn` and `cx.spawn_in`. These can be held across await points.

#### Window

`Window` provides access to the state of an application window. It is passed to functions as an argument named `window` and comes before `cx` when present. It is used for managing focus, dispatching actions, directly drawing, getting user input state, etc.

#### Entities

An `Entity<T>` is a handle to state of type `T`. With `thing: Entity<T>`:

* `thing.entity_id()` returns `EntityId`
* `thing.downgrade()` returns `WeakEntity<T>`
* `thing.read(cx: &App)` returns `&T`.
* `thing.update(cx, |thing: &mut T, cx: &mut Context<T>| ...)` allows the closure to mutate the state, and provides a `Context<T>` for interacting with the entity. It returns the closure's return value.

#### Elements and Rendering

The `Render` trait is used to render some state into an element tree that is laid out using flexbox layout. An `Entity<T>` where `T` implements `Render` is sometimes called a "view".

UI components that are constructed just to be turned into elements can instead implement the `RenderOnce` trait, which is similar to `Render`, but its `render` method takes ownership of `self`. To use a `RenderOnce` component as a child element, you must also derive `IntoElement`:

```rust
#[derive(IntoElement)]
pub struct MyComponent {
    label: SharedString,
    is_active: bool,
}

impl MyComponent {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            is_active: false,
        }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.is_active = active;
        self
    }
}

impl RenderOnce for MyComponent {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .p_2()
            .bg(if self.is_active { Theme::selected() } else { Theme::surface() })
            .child(self.label)
    }
}
```

Use `RenderOnce` components for stateless UI that is constructed fresh each render. Use `Entity<T>` with `Render` for stateful components that need to persist state across renders.

The style methods on elements are similar to those used by Tailwind CSS.

#### Input Events

Input event handlers can be registered on an element via methods like `.on_click(|event, window, cx: &mut App| ...)`.

Often event handlers will want to update the entity that's in the current `Context<T>`. The `cx.listener` method provides this - its use looks like `.on_click(cx.listener(|this: &mut T, event, window, cx: &mut Context<T>| ...))`.

#### Actions

Actions are dispatched via user keyboard interaction or in code via `window.dispatch_action(SomeAction.boxed_clone(), cx)`.

Actions with no data are defined with the `actions!(some_namespace, [SomeAction, AnotherAction])` macro call.

Action handlers can be registered on an element via `.on_action(|action, window, cx| ...)`, or globally with `cx.on_action(|action, cx| ...)`.

#### Notify

When a view's state has changed in a way that may affect its rendering, it should call `cx.notify()`. This will cause the view to be rerendered.

#### Div and Stateful Elements

A `Div` becomes a `Stateful<Div>` when you call `.id()` on it. This distinction matters because:

* Methods like `.overflow_y_scroll()` are only available on `Stateful<Div>` (via the `StatefulInteractiveElement` trait)
* When a function returns a div with an id, the return type must be `Stateful<Div>`, not `Div`
* If building a `Vec` of elements that have ids, use `Vec<Stateful<Div>>`

```rust
// This returns Div
div().flex().p_4()

// This returns Stateful<Div>
div().id("my-element").flex().p_4()

// overflow_y_scroll requires Stateful<Div>
div().id("scrollable").overflow_y_scroll()
```

#### Styling Patterns

Style methods follow Tailwind CSS naming conventions:

* Layout: `.flex()`, `.flex_col()`, `.flex_row()`, `.flex_1()`, `.gap_2()`, `.p_4()`, `.px_3()`, `.py_2()`
* Sizing: `.w_full()`, `.h_full()`, `.size_full()`, `.w_72()`, `.min_w_40()`
* Colors: `.bg(color)`, `.text_color(color)`, `.border_color(color)`
* Borders: `.border_1()`, `.border_r_1()`, `.border_b_1()`, `.rounded_md()`, `.rounded_lg()`
* Typography: `.text_sm()`, `.text_lg()`, `.text_xl()`, `.text_2xl()`, `.text_3xl()`, `.font_weight(FontWeight::BOLD)`
* Overflow: `.overflow_y_scroll()` (requires id), `.overflow_hidden()`

#### Conditional Styling

Use the `FluentBuilder` trait's `.when()` method for conditional styling. Import it with `use gpui::prelude::FluentBuilder;`:

```rust
div()
    .id("item")
    .when(is_selected, |this| this.bg(Theme::selected()))
```

#### Building Element Lists

When building lists of clickable elements, create them in a loop and use `.children()`:

```rust
let mut items: Vec<Stateful<Div>> = Vec::new();
for item in &self.items {
    let item_id = item.id;
    items.push(
        div()
            .id(ElementId::Name(format!("item-{}", item.id).into()))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.selected = Some(item_id);
                cx.notify();
            }))
            .child(/* ... */)
    );
}
div().children(items)
```

#### SharedString

GPUI uses `SharedString` for efficient string handling. Convert with `.into()`:

```rust
let name: SharedString = "Hello".into();
```

#### Global State

GPUI provides a global state mechanism for app-wide data like themes. To use it:

1. Define your data type and a wrapper that implements `Global`:
```rust
use gpui::{Global, Hsla};
use std::rc::Rc;

pub struct Theme {
    pub primary: Hsla,
    pub background: Hsla,
    // ...
}

#[derive(Clone)]
pub struct ActiveTheme {
    pub name: &'static str,
    pub theme: Rc<Theme>,
}

impl Global for ActiveTheme {}

impl std::ops::Deref for ActiveTheme {
    type Target = Theme;
    fn deref(&self) -> &Self::Target {
        &self.theme
    }
}
```

Using `Rc<Theme>` makes cloning cheap, and the `Deref` impl allows accessing theme fields directly through `ActiveTheme`.

2. Set the global in your app initialization:
```rust
cx.set_global(ActiveTheme::light());
```

3. Access it in components:
```rust
let theme = cx.global::<ActiveTheme>();
// Use theme.primary, theme.background, etc. (via Deref)
```

4. To automatically refresh the UI when the theme changes, use `observe_global`:
```rust
let mut previous_theme_name: &'static str = "light";
cx.observe_global::<ActiveTheme>(move |cx| {
    let current_name = cx.global::<ActiveTheme>().name;
    if current_name != previous_theme_name {
        previous_theme_name = current_name;
        cx.refresh_windows();
    }
})
.detach();
```

The `name` field allows detecting when the theme actually changed (vs just being set to the same value).

**Important**: When holding a reference from `cx.global()`, you cannot mutably borrow `cx` until that reference is dropped. Clone the global if you need to call other methods on `cx`:
```rust
let theme = cx.global::<ActiveTheme>().clone();
// Now you can call methods that take &mut cx
```

### gpui-component

This project uses [gpui-component](https://github.com/longbridge/gpui-component) for UI components.

#### Initialization

Always call `gpui_component::init(cx)` early in your `Application::new().run()` closure before opening windows.

#### Root Component

Wrap your main view in a `Root` component:

```rust
cx.open_window(options, |window, cx| {
    let view = cx.new(|cx| MyApp::new(cx));
    cx.new(|cx| Root::new(view, window, cx))
})
```
