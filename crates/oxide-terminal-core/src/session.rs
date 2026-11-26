use std::{
    cell::Cell,
    collections::VecDeque,
    io::{Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};

use alacritty_terminal::{
    event::{Event as AlacEvent, EventListener},
    grid::{Dimensions, Scroll},
    index::Line,
    sync::FairMutex,
    term::{Config, Term},
    vte::ansi::{self, Handler, Processor},
};
use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, unbounded};
use oxide_modem_transfer::{ModemConsumer, ModemConsumerEvent, ModemTransfer};
use oxide_ssh::{
    ConnectionConsumer, ManagedKeyResolver, SshConfig, SshConnectionHandle, SshConnectionRegistry,
    SshOutputChunk, SshPromptHandler, SshPtyHandle, SshTransportClient, SshTransportCommand,
    X11ForwardPolicy,
};
use oxide_terminal_encoding::{
    EncodingMismatchDetector, TerminalEncoding, TerminalInputEncoder, TerminalOutputDecoder,
};
use oxide_terminal_graphics::{
    GraphicsIngress, GraphicsOptions, KittyFileTransmissionControl, TerminalGraphicsSegment,
};
use oxide_trzsz::{TrzszConsumer, TrzszConsumerEvent, TrzszTransfer, TrzszTransferPolicy};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    runtime::Runtime,
};

pub use crate::backpressure::{TerminalDrainBudget, TerminalDrainReport, TerminalMagicKind};

use crate::{
    LocalEventListener, LocalEventReceiver, LocalPtyConfig, LocalPtySession, TermMode,
    TerminalActivityReceiver, TerminalCommandMark, TerminalCwdIntegrationLaunchState,
    TerminalEvent, TerminalGraphicsState, TerminalLifecycle, TerminalModemTransferRequest,
    TerminalPalette, TerminalProcessInfo, TerminalProcessProbe, TerminalSearchMatch, TerminalSize,
    TerminalSnapshot, append_grid_line_text, backpressure::MagicScanWindow, focus_report_sequence,
    graphics_cursor_from_term, incremental_snapshot_from_term, interactive_terminal_config,
    local_event_channel, privilege_prompt::TerminalPrivilegePromptStream,
    scroll_snapshot_from_term, search_matches_from_term,
    shell_integration::TerminalShellIntegration, snapshot_from_term,
    snapshot_from_term_with_display_offset,
};

const MAX_COMMAND_OUTPUT_LINES: usize = 400;
const MAX_COMMAND_OUTPUT_CHARS: usize = 24_000;
const MAX_AI_TERMINAL_BUFFER_LINES: usize = 500;

// AI terminal tools mirror the Tauri registry getter: expose the most recent
// physical buffer rows, including scrollback, rather than only the viewport.
fn terminal_buffer_text_from_term<T: EventListener>(term: &Term<T>, max_cols: usize) -> String {
    let grid = term.grid();
    let top_line = -(term.total_lines().saturating_sub(term.screen_lines()) as i32);
    let bottom_line = term.screen_lines() as i32;
    let line_count = (bottom_line - top_line).max(0) as usize;
    let kept_lines = line_count.min(MAX_AI_TERMINAL_BUFFER_LINES);
    let start_line = bottom_line - kept_lines as i32;
    let mut lines = Vec::with_capacity(kept_lines);

    for line in start_line..bottom_line {
        let row = &grid[Line(line)];
        let mut text = String::new();
        let mut cell_map = Vec::new();
        append_grid_line_text(row.iter(), line, max_cols, &mut text, &mut cell_map);
        lines.push(text.trim_end().to_string());
    }

    lines.join("\n")
}

fn command_output_text_from_term<T: EventListener>(
    term: &Term<T>,
    mark: &TerminalCommandMark,
) -> String {
    if mark.stale {
        return String::new();
    }
    if let Some(start) = mark.output_start {
        if mark.is_closed && mark.output_end.is_none() {
            return String::new();
        }
        let end = mark
            .output_end
            .unwrap_or_else(|| crate::shell_integration::cursor_position(term));
        return crate::shell_integration::text_between_positions(
            term,
            start,
            end,
            MAX_COMMAND_OUTPUT_LINES,
            MAX_COMMAND_OUTPUT_CHARS,
        );
    }
    if mark.detection_source == crate::TerminalCommandMarkDetectionSource::ShellIntegration {
        return String::new();
    }
    let start = mark.output_start_line();
    let end = mark.end_line.unwrap_or_else(|| {
        let scrollback = term.total_lines().saturating_sub(term.screen_lines());
        let cursor_line = term.renderable_content().cursor.point.line.0.max(0) as usize;
        scrollback.saturating_add(cursor_line)
    });
    if start > end {
        return String::new();
    }

    let mut text = String::new();
    for absolute_line in start..=end {
        if absolute_line - start >= MAX_COMMAND_OUTPUT_LINES
            || text.len() >= MAX_COMMAND_OUTPUT_CHARS
        {
            break;
        }
        if absolute_line > start {
            text.push('\n');
        }
        let remaining = MAX_COMMAND_OUTPUT_CHARS.saturating_sub(text.len());
        if remaining == 0 {
            break;
        }
        let line = crate::shell_integration::line_text(term, absolute_line);
        if line.len() > remaining {
            let mut end = 0;
            for (index, ch) in line.char_indices() {
                let next = index + ch.len_utf8();
                if next > remaining {
                    break;
                }
                end = next;
            }
            text.push_str(&line[..end]);
            break;
        }
        text.push_str(&line);
    }
    text
}

pub(crate) fn clear_terminal_buffer<T: EventListener>(term: &mut Term<T>) {
    // Clear host-owned emulator state without sending control bytes to the process or device.
    // New output must start at the cleared origin after viewport and scrollback removal.
    Handler::clear_screen(term, ansi::ClearMode::All);
    Handler::clear_screen(term, ansi::ClearMode::Saved);
    Handler::goto(term, 0, 0);
}

fn apply_terminal_output_processor<'a>(
    processor: &Option<TerminalOutputProcessor>,
    bytes: &'a [u8],
) -> std::borrow::Cow<'a, [u8]> {
    let Some(processor) = processor else {
        return std::borrow::Cow::Borrowed(bytes);
    };
    std::borrow::Cow::Owned(processor(bytes))
}

// Session backends are kept in this module scope so the TerminalSession
// facade, local PTY adapter, and SSH PTY owner keep their existing API and
// private access while avoiding another thousand-line implementation file.
include!("session/types.rs");
include!("session/line_endings.rs");
include!("session/facade.rs");
include!("session/playback.rs");
include!("session/local_backend.rs");
include!("session/ssh_config.rs");
mod ssh_parser;
use ssh_parser::SshParser;
include!("session/ssh_pty.rs");
mod ssh_worker;
pub use ssh_worker::SshPtySession;
include!("session/telnet.rs");
include!("session/mosh.rs");
include!("session/serial.rs");
