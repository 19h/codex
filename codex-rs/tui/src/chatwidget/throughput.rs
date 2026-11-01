//! Output-throughput bookkeeping for `ChatWidget`.
//!
//! The status row shows a live estimate while the model streams, and the status
//! line can show a rolling 30-minute average for the current model. This module
//! connects protocol deltas and token-usage notifications to those trackers.

use std::time::Instant;

use super::ChatWidget;
use crate::bottom_pane::StatusLineItem;
use crate::throughput::format_tps;

impl ChatWidget {
    /// Feeds streamed model output into the live and rolling throughput
    /// trackers. The live estimate lives in the status row so it can decay as
    /// output ages out of its short window.
    pub(super) fn record_streamed_output(&mut self, delta: &str) {
        let chars = delta.chars().count();
        if chars == 0 {
            return;
        }
        self.throughput.record_stream_delta(chars, Instant::now());
        self.bottom_pane.record_output_delta(chars);
    }

    /// Records the exact output-token count for a completed model response and
    /// refreshes the status line when it displays throughput.
    pub(super) fn note_model_response_completed(
        &mut self,
        total_output_tokens: u64,
        response_output_tokens: u64,
    ) {
        let model = self.current_model().to_string();
        self.throughput.complete_response(
            &model,
            total_output_tokens,
            response_output_tokens,
            Instant::now(),
        );
        if self
            .status_line_items_with_invalids()
            .0
            .contains(&StatusLineItem::Throughput)
        {
            self.refresh_status_line();
        }
    }

    /// Returns the rolling average throughput label for the current model, for
    /// example `~45 tps`.
    pub(super) fn model_throughput_label(&self) -> Option<String> {
        format_tps(
            self.throughput
                .average_tokens_per_second(self.current_model(), Instant::now())?,
        )
    }
}
