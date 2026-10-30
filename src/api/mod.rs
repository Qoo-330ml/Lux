//! Shared HTTP layer state, handler helpers, and the router entry points.

/// Passes an initialized `AppState` service through, otherwise answers `503`.
///
/// The expansion is the `let`-`else` early return that handlers used to spell out:
///
/// ```ignore
/// let Some(service) = state.example() else {
///     return StatusCode::SERVICE_UNAVAILABLE.into_response();
/// };
/// service
/// ```
///
/// It therefore keeps the original status code, empty body, and control flow, and it stays a
/// macro (not a function) because the failure arm has to return from the calling handler. The
/// binding name is chosen at the call site, so existing `let <service> = ...` bindings keep
/// their names: `let database = require_service!(state.database);`.
///
/// The slot itself lives in `CoreState` (`database`, `config_dir`) or in the ready-state
/// `ReadyServices`; the generated read accessors answer `Option<&Service>`, so the call sites did
/// not change when the business services moved out of `AppState`. `require_ready!` replaces this
/// per-service guard with a single one where a handler needs more than one service.
///
/// This must be defined before the child module declarations below, because `macro_rules!`
/// macros are visible by textual scope only.
macro_rules! require_service {
    ($state:ident.$field:ident) => {{
        let Some(service) = $state.$field() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        service
    }};
}

/// Answers `503` unless the state carries the ready services, then yields the shared handle.
///
/// ```ignore
/// let services = require_ready!(state);
/// services.catalog.list(..).await
/// ```
///
/// It replaces the per-service guards of a handler that needs more than one service with a single
/// guard, and it answers exactly the same bare `503` (no body, no message) as `require_service!`.
/// It returns the shared `Arc<ReadyServices>` clone, so a handler keeps observing the same service
/// instances as the workers and plugins; only the reference count changes.
///
/// Call it where the first per-service guard of the handler used to be, not necessarily on the
/// first line: the readiness flag never changes while a request is in flight, so every guard in
/// one handler returns the same response, and hoisting them to the earliest guard position keeps
/// the status-code ordering (authentication and validation errors stay first).
macro_rules! require_ready {
    ($state:ident) => {{
        let Some(services) = $state.ready_services() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        ::std::sync::Arc::clone(services)
    }};
}

pub mod lux;

mod legacy;

pub use legacy::{AppState, CoreState, ReadyServices, app, app_with_state};
