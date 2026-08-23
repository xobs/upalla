//! Level metering for the VU displays.

/// Input and output levels accumulated over one status interval, driving a
/// chain's pair of VU meters.
///
/// Levels are pushed as per-frame sums across the two channels and read back as
/// per-channel means, so a signal at a constant amplitude reads back as that
/// amplitude rather than as the two-channel sum.
#[derive(Default)]
pub struct RmsMeter {
    input: f32,
    output: f32,
    count: u32,
}

impl RmsMeter {
    /// Records the input and output level of one processed frame. Both figures are
    /// sums across the two channels, so the count advances by two.
    pub fn push(&mut self, input: f32, output: f32) {
        self.input += input;
        self.output += output;
        self.count += 2;
    }

    pub fn mean_input(&self) -> f32 {
        self.mean(self.input)
    }

    pub fn mean_output(&self) -> f32 {
        self.mean(self.output)
    }

    /// How many frames have been recorded since the last reset.
    pub fn frames(&self) -> u32 {
        self.count / 2
    }

    fn mean(&self, total: f32) -> f32 {
        if self.count > 0 {
            total / self.count as f32
        } else {
            0.0
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The meters report a per-channel mean, so a frame at a constant amplitude
    /// reads back as that amplitude rather than the two-channel sum.
    #[test]
    fn reports_per_channel_mean() {
        let mut meter = RmsMeter::default();
        meter.push(1.0, 0.5); // per-frame sums across two channels
        assert_eq!(meter.frames(), 1);
        assert_eq!(meter.mean_input(), 0.5);
        assert_eq!(meter.mean_output(), 0.25);

        meter.push(1.0, 0.5);
        assert_eq!(meter.frames(), 2);
        assert_eq!(
            meter.mean_input(),
            0.5,
            "mean must not drift as frames add up"
        );
    }

    #[test]
    fn empty_meter_reads_zero_rather_than_dividing_by_zero() {
        let meter = RmsMeter::default();
        assert_eq!(meter.frames(), 0);
        assert_eq!(meter.mean_input(), 0.0);
        assert_eq!(meter.mean_output(), 0.0);
    }

    #[test]
    fn reset_clears_every_field() {
        let mut meter = RmsMeter::default();
        meter.push(3.0, 4.0);
        meter.reset();
        assert_eq!(meter.frames(), 0);
        assert_eq!(meter.input, 0.0);
        assert_eq!(meter.output, 0.0);
    }
}
