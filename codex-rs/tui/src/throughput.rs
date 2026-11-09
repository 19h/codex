//! Token-throughput estimates for the TUI.
//!
//! The status row can show a live estimate of how fast the model is emitting
//! output while a turn streams, and the status line can show a rolling average
//! for the current model. Live deltas only contain text, so the live estimate
//! converts characters to tokens with a coarse heuristic; completed responses
//! are reconciled against the exact output-token counts reported by the server.

use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

/// Rough number of characters per output token used by the live estimator.
///
/// Streaming deltas do not carry token counts, so the live rate is necessarily
/// approximate. Four characters per token matches common English/code text
/// closely enough to make the displayed rate useful without waiting for the
/// response to finish.
const CHARS_PER_TOKEN: f64 = 4.0;

/// Sliding window used for the live rate next to the working timer.
const LIVE_WINDOW: Duration = Duration::from_secs(5);

/// Minimum observed streaming time before the live rate is shown, so the first
/// few deltas do not produce a misleading spike or dip.
const LIVE_MIN_DENOMINATOR: Duration = Duration::from_secs(1);

/// Rolling window for the per-model average shown in the status line.
pub(crate) const HISTORY_WINDOW: Duration = Duration::from_secs(30 * 60);

/// Ignore samples too short to produce a meaningful rate.
const MIN_SAMPLE_DURATION: Duration = Duration::from_millis(250);

/// Tracks streamed output so the status row can display a live tokens/second
/// estimate. Samples age out of a short sliding window, so the estimate
/// disappears when the model stops emitting.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveOutputRate {
    samples: VecDeque<(Instant, f64)>,
}

impl LiveOutputRate {
    /// Records streamed output characters observed at `now`.
    pub(crate) fn record_delta(&mut self, chars: usize, now: Instant) {
        if chars == 0 {
            return;
        }
        self.prune(now);
        self.samples
            .push_back((now, chars as f64 / CHARS_PER_TOKEN));
    }

    /// Returns the estimated output rate in tokens per second, or `None` when
    /// no output has been observed recently.
    pub(crate) fn tokens_per_second(&self, now: Instant) -> Option<u64> {
        let newest = self.samples.back()?;
        if now.saturating_duration_since(newest.0) > LIVE_WINDOW {
            return None;
        }
        let window_start = now.checked_sub(LIVE_WINDOW).unwrap_or(now);
        let mut oldest_in_window = None;
        let mut tokens = 0.0;
        for (at, sample_tokens) in &self.samples {
            if *at < window_start {
                continue;
            }
            oldest_in_window.get_or_insert(*at);
            tokens += sample_tokens;
        }
        let oldest_in_window = oldest_in_window?;
        let elapsed = now.saturating_duration_since(oldest_in_window);
        // Require a full second of streaming so the first few deltas do not
        // show a misleadingly low rate.
        if elapsed < LIVE_MIN_DENOMINATOR {
            return None;
        }
        let rate = tokens / elapsed.as_secs_f64();
        (rate >= 0.5).then(|| rate.round() as u64)
    }

    pub(crate) fn reset(&mut self) {
        self.samples.clear();
    }

    fn prune(&mut self, now: Instant) {
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > LIVE_WINDOW)
        {
            self.samples.pop_front();
        }
    }
}

#[derive(Debug)]
struct ThroughputSample {
    model: String,
    output_tokens: u64,
    duration: Duration,
    recorded_at: Instant,
}

/// Tracks completed model responses per model, so the status line can display
/// a rolling average output rate. Exact output-token counts come from the
/// token-usage notifications; the streamed-delta timestamps bound the
/// generation window.
#[derive(Debug, Default)]
pub(crate) struct ModelThroughputHistory {
    stream_started_at: Option<Instant>,
    /// Cumulative output tokens from the last accepted sample. Token-usage
    /// notifications can repeat (for example after a rate-limit refresh), so
    /// an unchanged total must not complete the in-flight measurement.
    last_total_output_tokens: Option<u64>,
    samples: Vec<ThroughputSample>,
}

impl ModelThroughputHistory {
    /// Clears any in-flight stream boundary, for example when a new turn
    /// starts after an interrupted response.
    pub(crate) fn reset_stream(&mut self) {
        self.stream_started_at = None;
    }

    /// Marks the start of a new streamed model response.
    pub(crate) fn record_stream_delta(&mut self, chars: usize, now: Instant) {
        if chars > 0 {
            self.stream_started_at.get_or_insert(now);
        }
    }

    /// Records a completed model response using the exact output-token count
    /// reported by the server. Responses without an observed stream (for
    /// example, replayed history) are ignored.
    pub(crate) fn complete_response(
        &mut self,
        model: &str,
        total_output_tokens: u64,
        response_output_tokens: u64,
        now: Instant,
    ) {
        let Some(started_at) = self.stream_started_at.take() else {
            return;
        };
        if self.last_total_output_tokens == Some(total_output_tokens) {
            // A repeated notification is not a new response; keep the stream
            // open so the real completion can still be measured.
            self.stream_started_at = Some(started_at);
            return;
        }
        self.last_total_output_tokens = Some(total_output_tokens);
        let duration = now.saturating_duration_since(started_at);
        if response_output_tokens == 0 || duration < MIN_SAMPLE_DURATION {
            return;
        }
        self.samples.push(ThroughputSample {
            model: model.to_string(),
            output_tokens: response_output_tokens,
            duration,
            recorded_at: now,
        });
        self.prune(now);
    }

    /// Returns the average output tokens per second for `model` over the last
    /// 30 minutes, or `None` until at least one response has been measured.
    pub(crate) fn average_tokens_per_second(&self, model: &str, now: Instant) -> Option<f64> {
        let mut total_tokens = 0u64;
        let mut total_duration = Duration::ZERO;
        for sample in self.samples.iter().filter(|sample| {
            sample.model == model
                && now.saturating_duration_since(sample.recorded_at) <= HISTORY_WINDOW
        }) {
            total_tokens = total_tokens.saturating_add(sample.output_tokens);
            total_duration = total_duration.saturating_add(sample.duration);
        }
        if total_duration < MIN_SAMPLE_DURATION {
            return None;
        }
        Some(total_tokens as f64 / total_duration.as_secs_f64())
    }

    fn prune(&mut self, now: Instant) {
        self.samples
            .retain(|sample| now.saturating_duration_since(sample.recorded_at) <= HISTORY_WINDOW);
    }
}

/// Formats a tokens-per-second value for display, e.g. `~45 tps`.
pub(crate) fn format_tps(tps: f64) -> Option<String> {
    let rounded = tps.round();
    if rounded < 1.0 {
        return None;
    }
    Some(format!("~{} tps", rounded as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_rate_uses_recent_deltas() {
        let mut rate = LiveOutputRate::default();
        let start = Instant::now();
        rate.record_delta(/*chars*/ 160, start);
        rate.record_delta(/*chars*/ 160, start + Duration::from_secs(/*secs*/ 1));

        assert_eq!(
            rate.tokens_per_second(start + Duration::from_secs(/*secs*/ 2)),
            Some(40)
        );
    }

    #[test]
    fn live_rate_is_hidden_until_a_second_of_streaming() {
        let mut rate = LiveOutputRate::default();
        let start = Instant::now();
        rate.record_delta(/*chars*/ 160, start);

        assert_eq!(
            rate.tokens_per_second(start + Duration::from_millis(/*millis*/ 500)),
            None
        );
    }

    #[test]
    fn live_rate_expires_without_streaming() {
        let mut rate = LiveOutputRate::default();
        let start = Instant::now();
        rate.record_delta(/*chars*/ 160, start);

        assert_eq!(
            rate.tokens_per_second(start + Duration::from_secs(/*secs*/ 6)),
            None
        );
    }

    #[test]
    fn live_rate_ignores_empty_deltas() {
        let mut rate = LiveOutputRate::default();
        rate.record_delta(/*chars*/ 0, Instant::now());
        assert_eq!(rate.tokens_per_second(Instant::now()), None);
    }

    #[test]
    fn history_averages_only_matching_model_within_window() {
        let mut history = ModelThroughputHistory::default();
        let start = Instant::now();
        history.record_stream_delta(/*chars*/ 4, start);
        history.complete_response(
            "gpt-5",
            /*total_output_tokens*/ 600,
            /*response_output_tokens*/ 600,
            start + Duration::from_secs(/*secs*/ 2),
        );

        let second = start + Duration::from_secs(/*secs*/ 10);
        history.record_stream_delta(/*chars*/ 4, second);
        history.complete_response(
            "gpt-5",
            /*total_output_tokens*/ 900,
            /*response_output_tokens*/ 300,
            second + Duration::from_secs(/*secs*/ 1),
        );

        let third = start + Duration::from_secs(/*secs*/ 20);
        history.record_stream_delta(/*chars*/ 4, third);
        history.complete_response(
            "other-model",
            /*total_output_tokens*/ 1800,
            /*response_output_tokens*/ 900,
            third + Duration::from_secs(/*secs*/ 3),
        );

        assert_eq!(
            history.average_tokens_per_second("gpt-5", start + Duration::from_secs(/*secs*/ 30)),
            Some(300.0)
        );
    }

    #[test]
    fn history_drops_samples_outside_window() {
        let mut history = ModelThroughputHistory::default();
        let start = Instant::now();
        history.record_stream_delta(/*chars*/ 4, start);
        history.complete_response(
            "gpt-5",
            /*total_output_tokens*/ 500,
            /*response_output_tokens*/ 500,
            start + Duration::from_secs(/*secs*/ 1),
        );

        assert_eq!(
            history
                .average_tokens_per_second("gpt-5", start + Duration::from_secs(/*secs*/ 31 * 60)),
            None
        );
    }

    #[test]
    fn repeated_usage_notification_keeps_the_stream_open() {
        let mut history = ModelThroughputHistory::default();
        let start = Instant::now();
        history.record_stream_delta(/*chars*/ 4, start);
        history.complete_response(
            "gpt-5",
            /*total_output_tokens*/ 400,
            /*response_output_tokens*/ 400,
            start + Duration::from_secs(/*secs*/ 1),
        );

        let second = start + Duration::from_secs(/*secs*/ 10);
        history.record_stream_delta(/*chars*/ 4, second);
        history.complete_response(
            "gpt-5",
            /*total_output_tokens*/ 400,
            /*response_output_tokens*/ 400,
            second + Duration::from_secs(/*secs*/ 1),
        );
        history.complete_response(
            "gpt-5",
            /*total_output_tokens*/ 700,
            /*response_output_tokens*/ 300,
            second + Duration::from_secs(/*secs*/ 2),
        );

        assert_eq!(history.samples.len(), 2);
        assert_eq!(history.samples[1].output_tokens, 300);
        assert_eq!(history.samples[1].duration, Duration::from_secs(/*secs*/ 2));
    }

    #[test]
    fn history_ignores_unstreamed_responses() {
        let mut history = ModelThroughputHistory::default();
        history.complete_response(
            "gpt-5",
            /*total_output_tokens*/ 500,
            /*response_output_tokens*/ 500,
            Instant::now(),
        );
        assert_eq!(
            history.average_tokens_per_second("gpt-5", Instant::now()),
            None
        );
    }

    #[test]
    fn format_tps_rounds_and_hides_tiny_rates() {
        assert_eq!(format_tps(45.4), Some("~45 tps".to_string()));
        assert_eq!(format_tps(0.4), None);
    }
}
