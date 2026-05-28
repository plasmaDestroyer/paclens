//! Execution layer: runs pre-built `ActionPlan`s.
//!
//! Never decides what to do — all decisions come from the user. Logs every
//! command before and after, reports exit codes without interpretation.
//!
//! Built in v0.0.6 (first Flatpak update) and v0.1.0 (pacman + sudo).
