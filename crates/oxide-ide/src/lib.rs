// Copyright (C) 2026 AnalyseDeCircuit
// SPDX-License-Identifier: GPL-3.0-only

//! GPUI owner surface for oxide's native IDE path.
//!
//! This crate is the first real UI owner for `oxide-ide-core`: it owns the
//! project tree, opened editor tabs, save dispatch, and reconnect snapshot
//! restore surface. It is deliberately transport-agnostic except for the
//! node-first IDE file-system adapter passed in from the app layer.

mod labels;
mod surface;

pub use labels::IdeLabels;
pub use oxide_ide_core::{IdePluginFileSnapshot, IdePluginProjectSnapshot, IdePluginSnapshot};
pub use oxide_ide_fs::NodeAgentMode;
pub use surface::{IdeLoadState, IdeRuntimeSettings, IdeSurface, IdeSurfaceEvent, IdeSurfaceMount};
