# ADR-001: Concurrency Model — async-channel between GTK and Tokio

## Status

Accepted

## Context

Gemini Lite has two runtime worlds that must cooperate:

1. **GTK main thread** — owns all widgets, processes user input, redraws the UI. GTK 3 is single-threaded: every widget operation must happen on this thread.
2. **Tokio async runtime** — handles HTTP streaming to the Gemini API. Network I/O is inherently async and must not block the UI.

The core challenge is: *How do we send data from an async task (the streaming response) to the GTK main loop without blocking either side?*

### Alternatives Considered

| Approach | Pros | Cons |
|---|---|---|
| **glib::MainContext::channel** | Native GTK integration, automatic main-loop wakeup | Only works GTK→GTK. Cannot be `Send` across Tokio tasks easily. Ties the channel to the GLib event loop. |
| **crossbeam-channel** | Zero-copy, very fast | Synchronous API — the receiver blocks. Would require a polling timer on the GTK side (`glib::timeout_add`), adding latency and CPU overhead. |
| **std::sync::mpsc** | No extra dependency | Same blocking-receive problem as crossbeam. |
| **async-channel** | Fully async `send`/`recv`. The receiver can be `.await`ed inside `glib::MainContext::spawn_local`, giving zero-latency wakeup without polling. | Extra crate (~15 KB). |

## Decision

Use **`async-channel`** (unbounded) as the bridge between Tokio and GTK.

- The Tokio task calls `tx.send(UiEvent::Delta(...)).await` for each SSE fragment.
- The GTK side runs `glib::MainContext::default().spawn_local(async { while let Ok(ev) = rx.recv().await { ... } })`, which integrates seamlessly with the GLib event loop.

Conversation history is shared via `Arc<Mutex<Vec<Content>>>`. The mutex is held briefly (clone-on-read, push-on-write), minimizing contention. The `.unwrap()` on `lock()` is acceptable because the only panic path (Tokio task panicking while holding the lock) would already be a fatal error.

## Consequences

- **Positive:** Streaming text appears in the UI with sub-millisecond latency after each SSE chunk arrives. No polling, no busy-wait.
- **Positive:** The Tokio runtime is fully decoupled from GTK. The `api` and `sse` modules can be tested without any GTK dependency.
- **Risk:** If the Tokio task panics while holding the `Mutex`, the lock is poisoned and the next `lock().unwrap()` on the GTK thread will panic. This is an intentional fail-fast: a panic in the network layer indicates a bug, not a recoverable error.
- **Trade-off:** `async-channel` adds a small dependency. Given its single purpose (async MPMC channel) and minimal footprint, this is acceptable.
