//! Shared HTTP layer state, handler helpers, and the router entry points.

/// Passes an initialized optional `AppState` service through, otherwise answers `503`.
///
/// The expansion is the `let`-`else` early return that handlers used to spell out:
///
/// ```ignore
/// let Some(service) = state.example.as_ref() else {
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
/// This must be defined before the child module declarations below, because `macro_rules!`
/// macros are visible by textual scope only.
macro_rules! require_service {
    ($state:ident.$field:ident) => {{
        let Some(service) = $state.$field.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        service
    }};
}

pub mod lux;

mod legacy;

pub use legacy::{AppState, app, app_with_state};
