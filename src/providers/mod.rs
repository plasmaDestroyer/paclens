//! Package source providers (pacman, flatpak) and the command-execution seam.
//!
//! `CommandRunner` is the injectable seam used for testing; `Provider` is the
//! per-source trait. Providers never call sudo and never know about each other.
//!
//! Built in v0.0.2 (probing) and v0.0.3 (full `pacman -Qi` parser).
