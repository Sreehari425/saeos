//! Sample-rate conversion in the sound core: `Converter` with a card rate
//! other than the program's.

use crate::sound::format::{Converter, Format, Sample};
use crate::sound::resample::Resampler;

fn format(rate: u32) -> Format {
    Format {
        rate,
        sample: Sample::S16,
        channels: 1,
    }
}

/// A mono 16-bit sine at `rate`.
fn sine(frequency: f64, rate: u32, seconds: f64) -> Vec<u8> {
    (0..(f64::from(rate) * seconds) as usize)
        .flat_map(|n| {
            let value = (12_000.0
                * (2.0 * std::f64::consts::PI * frequency * n as f64 / f64::from(rate)).sin())
                as i16;
            value.to_le_bytes()
        })
        .collect()
}

fn convert_all(converter: &mut Converter, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut consumed = 0;
    // Through a small window, as the ring offers space a period at a time.
    loop {
        let mut window = [0u8; 256];
        let (taken, produced) = converter.convert(&input[consumed..], &mut window);
        consumed += taken;
        output.extend_from_slice(&window[..produced]);
        if taken == 0 && produced == 0 {
            break;
        }
    }
    assert_eq!(consumed, input.len());
    output
}

/// Left-channel samples of 16-bit stereo output.
fn left(output: &[u8]) -> Vec<i16> {
    output
        .chunks_exact(4)
        .map(|frame| i16::from_le_bytes([frame[0], frame[1]]))
        .collect()
}

/// Frequency from upward zero crossings.
fn frequency(samples: &[i16], rate: u32) -> f64 {
    let crossings: Vec<f64> = samples
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0] < 0 && pair[1] >= 0)
        .map(|(index, pair)| {
            index as f64 + f64::from(-pair[0]) / (f64::from(pair[1]) - f64::from(pair[0]))
        })
        .collect();
    let cycles = (crossings.len() - 1) as f64;
    f64::from(rate) * cycles / (crossings.last().unwrap() - crossings[0])
}

#[test]
fn equal_rates_pass_samples_through() {
    let input = sine(440.0, 48_000, 0.01);
    let mut converter = Converter::new(format(48_000), 48_000);
    let output = convert_all(&mut converter, &input);
    let expected: Vec<i16> = input
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect();
    assert_eq!(left(&output), expected);
}

#[test]
fn resampling_keeps_pitch_amplitude_and_length() {
    let seconds = if cfg!(miri) { 0.01 } else { 0.5 };
    for (frequency_in, rate_in, rate_out) in [
        (880.0, 8_000, 48_000),
        (1000.0, 22_050, 44_100),
        (660.0, 16_000, 48_000),
        (440.0, 48_000, 44_100),
        (440.0, 96_000, 48_000),
    ] {
        let input = sine(frequency_in, rate_in, seconds);
        let mut converter = Converter::new(format(rate_in), rate_out);
        let output = left(&convert_all(&mut converter, &input));
        let expected = (input.len() / 2) as f64 * f64::from(rate_out) / f64::from(rate_in);
        assert!(
            (output.len() as f64 - expected).abs() <= f64::from(rate_out / rate_in) + 2.0,
            "{rate_in}->{rate_out}: {} frames, expected {expected}",
            output.len()
        );
        if !cfg!(miri) {
            let measured = frequency(&output, rate_out);
            assert!(
                (measured - frequency_in).abs() < frequency_in * 0.002,
                "{rate_in}->{rate_out}: {measured} Hz, expected {frequency_in}"
            );
            let peak = output
                .iter()
                .map(|sample| sample.unsigned_abs())
                .max()
                .unwrap();
            assert!((11_000..=12_000).contains(&peak), "peak {peak}");
        }
    }
}

#[test]
fn splitting_the_input_does_not_change_the_output() {
    let input = sine(700.0, 22_050, 0.02);
    let whole = convert_all(&mut Converter::new(format(22_050), 48_000), &input);
    for split in [1, 2, 3, 101, 400, input.len() - 1] {
        let mut converter = Converter::new(format(22_050), 48_000);
        let mut output = convert_all(&mut converter, &input[..split]);
        output.extend(convert_all(&mut converter, &input[split..]));
        assert_eq!(output, whole, "split at {split}");
    }
}

#[test]
fn resampler_interpolates_between_frames() {
    // Doubling the rate puts a midpoint between every pair.
    let mut resampler = Resampler::new(1, 2);
    let mut output = Vec::new();
    for frame in [[0, 0], [100, -100], [200, -200]] {
        resampler.feed(frame);
        while let Some(frame) = resampler.produce() {
            output.push(frame);
        }
    }
    assert_eq!(output, [[0, 0], [50, -50], [100, -100], [150, -150]]);
}
