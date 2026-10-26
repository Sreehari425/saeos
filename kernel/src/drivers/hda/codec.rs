//! HD Audio codecs: finding the outputs and wiring them to DACs, generically
//! (no per-codec tables), from what the codec reports about itself.
//!
//! A codec is a tree of nodes: the root (node 0), function groups (the
//! audio one, "AFG"), and the AFG's widgets. Widgets form a graph through
//! their connection lists, each naming the widgets that feed it:
//!
//! ```text
//! DAC (audio output) ──▶ [mixer / selector ...] ──▶ pin complex ──▶ jack/speaker
//!   0x02 ─────────────────────────────────────────▶ 0x14 internal speaker (ALC257)
//!   0x03 ──▶ 0x21 headphone jack (connection list: 0x02 0x03, select = 1)
//! ```
//!
//! Commands ("verbs") are 32 bits: codec address (4) | node (8) | verb
//! (20), the last either a 12-bit verb with an 8-bit payload or a 4-bit
//! verb with a 16-bit payload. Each answers with 32 bits.
//!
//! | parameter (`GET_PARAMETER` 0xF00) | meaning                              |
//! |-------------------|------------------------------------------------------|
//! | 0x00 vendor ID    | `vvvv dddd`                                          |
//! | 0x04 node count   | first node << 16, count                              |
//! | 0x05 function type| bits 7:0, 1 = audio                                  |
//! | 0x09 widget caps  | type 23:20 (0 DAC, 1 ADC, 2 mixer, 3 selector, 4 pin), 0 stereo, 1 in amp, 2 out amp, 3 amp caps override, 4 format override, 8 connection list, 9 digital, 10 power control |
//! | 0x0A PCM          | rates 11:0 (8k, 11.025k, 16k, 22.05k, 32k, 44.1k, 48k, 88.2k, 96k, 176.4k, 192k, 384k), bit depths 20:16 (8, 16, 20, 24, 32) |
//! | 0x0C pin caps     | 3 headphone drive, 4 output, 5 input, 6 HDMI, 16 EAPD, 24 DisplayPort |
//! | 0x0D/0x12 amp caps| offset (0 dB step) 6:0, steps 14:8, step size 22:16, 31 can mute |
//! | 0x0E connections  | length 6:0, 7 long form (16-bit entries)             |
//!
//! The pin default configuration (`GET_CONFIG_DEFAULT` 0xF1C), set by the
//! board's firmware, says what a pin is wired to: connectivity 31:30 (0
//! jack, 1 nothing, 2 fixed, 3 both), device 23:20 (0 line out, 1 speaker,
//! 2 headphone, 4 S/PDIF, 5 digital other, ...), association 7:4 and
//! sequence 3:0 (order within a group).
//!
//! Outputs: every pin that can drive output, is connected to something,
//! and is an analog line out, speaker or headphone; digital pins (HDMI,
//! DisplayPort, S/PDIF) are left out. Each, in association and sequence
//! order, is traced back breadth-first through mixers and selectors to an
//! analog DAC, preferring one no earlier pin took (else sharing). Then
//! along each path: power on (D0), connection selects set, every output
//! amp at 0 dB unmuted (and the mixer input used unmuted), pin control
//! OUT (plus HP for headphones), and EAPD (the external amplifier switch,
//! without which a laptop's speakers are silent) where the pin has it. Every
//! DAC gets the same stream, so all outputs play the same audio; with
//! headphones in, the speaker keeps playing (no jack detection yet).
//!
//! Pure: the codec is reached through [`CodecLink`], which the controller
//! implements with its command ring and the host model with fake codecs
//! built from `/proc/asound` dumps of real ones.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// Sends one verb to a codec node and returns the response (`None`: no
/// answer).
pub trait CodecLink {
    fn command(&mut self, codec: u8, node: u8, verb: u32) -> Option<u32>;
}

/// A 12-bit verb with an 8-bit payload.
pub const fn verb12(verb: u16, payload: u8) -> u32 {
    ((verb as u32) << 8) | payload as u32
}

/// A 4-bit verb with a 16-bit payload.
pub const fn verb4(verb: u8, payload: u16) -> u32 {
    ((verb as u32) << 16) | payload as u32
}

/// The whole command word for the controller's command ring.
pub const fn command_word(codec: u8, node: u8, verb: u32) -> u32 {
    ((codec as u32) << 28) | ((node as u32) << 20) | (verb & 0xf_ffff)
}

pub const GET_PARAMETER: u16 = 0xf00;
pub const GET_CONNECTION_LIST: u16 = 0xf02;
pub const GET_CONFIG_DEFAULT: u16 = 0xf1c;
pub const SET_CONNECTION_SELECT: u16 = 0x701;
pub const SET_POWER_STATE: u16 = 0x705;
pub const SET_CHANNEL_STREAM: u16 = 0x706;
pub const SET_PIN_CONTROL: u16 = 0x707;
pub const SET_EAPD: u16 = 0x70c;
pub const SET_CONVERTER_FORMAT: u8 = 0x2;
pub const SET_AMP_GAIN_MUTE: u8 = 0x3;

pub const PARAMETER_VENDOR: u8 = 0x00;
pub const PARAMETER_NODE_COUNT: u8 = 0x04;
pub const PARAMETER_FUNCTION_TYPE: u8 = 0x05;
pub const PARAMETER_WIDGET_CAPS: u8 = 0x09;
pub const PARAMETER_PCM: u8 = 0x0a;
pub const PARAMETER_PIN_CAPS: u8 = 0x0c;
pub const PARAMETER_AMP_IN: u8 = 0x0d;
pub const PARAMETER_CONNECTIONS: u8 = 0x0e;
pub const PARAMETER_AMP_OUT: u8 = 0x12;

const CAPS_IN_AMP: u32 = 1 << 1;
const CAPS_OUT_AMP: u32 = 1 << 2;
const CAPS_AMP_OVERRIDE: u32 = 1 << 3;
const CAPS_FORMAT_OVERRIDE: u32 = 1 << 4;
const CAPS_CONNECTIONS: u32 = 1 << 8;
const CAPS_DIGITAL: u32 = 1 << 9;
const CAPS_POWER: u32 = 1 << 10;

const PIN_HEADPHONE_DRIVE: u32 = 1 << 3;
const PIN_OUTPUT: u32 = 1 << 4;
const PIN_HDMI: u32 = 1 << 6;
const PIN_EAPD: u32 = 1 << 16;
const PIN_DISPLAY_PORT: u32 = 1 << 24;

pub const PIN_CONTROL_OUT: u8 = 0x40;
pub const PIN_CONTROL_HEADPHONE: u8 = 0x80;
pub const EAPD_ENABLE: u8 = 0x02;
const PCM_16_BIT: u32 = 1 << 17;

/// Rates by their bit in the PCM parameter.
pub const RATES: [u32; 12] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
    384_000,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetType {
    Output,
    Input,
    Mixer,
    Selector,
    Pin,
    Other(u8),
}

impl WidgetType {
    fn from_caps(caps: u32) -> Self {
        match (caps >> 20) & 0xf {
            0 => Self::Output,
            1 => Self::Input,
            2 => Self::Mixer,
            3 => Self::Selector,
            4 => Self::Pin,
            other => Self::Other(other as u8),
        }
    }
}

/// What a pin's default configuration says it drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDevice {
    LineOut,
    Speaker,
    Headphone,
}

impl PinDevice {
    fn name(self) -> &'static str {
        match self {
            Self::LineOut => "line out",
            Self::Speaker => "speaker",
            Self::Headphone => "headphone",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Widget {
    pub node: u8,
    pub kind: WidgetType,
    pub caps: u32,
    pub pin_caps: u32,
    pub pin_default: u32,
    pub amp_in: u32,
    pub amp_out: u32,
    pub pcm: u32,
    /// The widgets that feed this one, in connection-index order.
    pub connections: Vec<u8>,
}

impl Widget {
    fn digital(&self) -> bool {
        self.caps & CAPS_DIGITAL != 0
    }

    /// The analog output this pin drives, if it is one to use.
    fn output_device(&self) -> Option<PinDevice> {
        if self.kind != WidgetType::Pin
            || self.pin_caps & PIN_OUTPUT == 0
            || self.pin_caps & (PIN_HDMI | PIN_DISPLAY_PORT) != 0
            || self.digital()
            || self.pin_default >> 30 == 1
        {
            return None;
        }
        match (self.pin_default >> 20) & 0xf {
            0 => Some(PinDevice::LineOut),
            1 => Some(PinDevice::Speaker),
            2 => Some(PinDevice::Headphone),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Codec {
    pub address: u8,
    pub vendor: u32,
    pub function_group: u8,
    pub widgets: Vec<Widget>,
}

impl Codec {
    pub fn widget(&self, node: u8) -> Option<&Widget> {
        self.widgets.iter().find(|widget| widget.node == node)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecError {
    NoResponse,
    NoAudioFunction,
    /// Nothing analog to play on (an HDMI-only codec).
    NoOutputs,
    /// The DACs share no 16-bit rate.
    NoFormat,
}

/// One output: the pin, then each widget towards its DAC (the last node).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPath {
    pub device: PinDevice,
    pub nodes: Vec<u8>,
}

impl OutputPath {
    pub fn pin(&self) -> u8 {
        self.nodes[0]
    }

    pub fn dac(&self) -> u8 {
        *self.nodes.last().unwrap()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlan {
    pub address: u8,
    pub vendor: u32,
    pub function_group: u8,
    pub outputs: Vec<OutputPath>,
    /// Each DAC once.
    pub dacs: Vec<u8>,
    /// Rates every DAC plays with 16-bit samples, ascending.
    pub rates: Vec<u32>,
}

impl OutputPlan {
    /// For `/proc/sound`.
    pub fn describe(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "codec: {} ({:08x})", self.address, self.vendor);
        for output in &self.outputs {
            let _ = write!(
                text,
                "output: {} {:#04x}",
                output.device.name(),
                output.pin()
            );
            for node in &output.nodes[1..] {
                let _ = write!(text, " <- {node:#04x}");
            }
            text.push('\n');
        }
        let _ = writeln!(text, "card rates: {:?}", self.rates);
        text
    }
}

fn parameter(
    link: &mut dyn CodecLink,
    codec: u8,
    node: u8,
    parameter: u8,
) -> Result<u32, CodecError> {
    link.command(codec, node, verb12(GET_PARAMETER, parameter))
        .ok_or(CodecError::NoResponse)
}

/// The connection list, with ranges expanded.
fn connections(link: &mut dyn CodecLink, codec: u8, node: u8) -> Result<Vec<u8>, CodecError> {
    let info = parameter(link, codec, node, PARAMETER_CONNECTIONS)?;
    let length = (info & 0x7f) as usize;
    let long = info & 0x80 != 0;
    let (per_word, bits, range_bit) = if long {
        (2, 16, 1 << 15)
    } else {
        (4, 8, 1 << 7)
    };
    let mut list: Vec<u8> = Vec::new();
    let mut index = 0;
    while index < length {
        let word = link
            .command(codec, node, verb12(GET_CONNECTION_LIST, index as u8))
            .ok_or(CodecError::NoResponse)?;
        for slot in 0..per_word {
            if index + slot >= length {
                break;
            }
            let entry = (word >> (slot * bits)) & ((1 << bits) - 1);
            let target = (entry & (range_bit - 1)) as u8;
            match list.last() {
                // A range: every node from the previous entry to this one.
                Some(&previous) if entry & range_bit != 0 && target > previous => {
                    list.extend(previous + 1..=target);
                }
                _ => list.push(target),
            }
        }
        index += per_word;
    }
    Ok(list)
}

/// Read codec `address`: its audio function group and every widget.
pub fn discover(link: &mut dyn CodecLink, address: u8) -> Result<Codec, CodecError> {
    let vendor = parameter(link, address, 0, PARAMETER_VENDOR)?;
    let groups = parameter(link, address, 0, PARAMETER_NODE_COUNT)?;
    let (first, count) = (((groups >> 16) & 0xff) as u8, (groups & 0xff) as u8);
    let function_group = (first..first.saturating_add(count))
        .find(|&node| {
            parameter(link, address, node, PARAMETER_FUNCTION_TYPE)
                .is_ok_and(|kind| kind & 0xff == 1)
        })
        .ok_or(CodecError::NoAudioFunction)?;
    let default_pcm = parameter(link, address, function_group, PARAMETER_PCM)?;
    let default_in = parameter(link, address, function_group, PARAMETER_AMP_IN)?;
    let default_out = parameter(link, address, function_group, PARAMETER_AMP_OUT)?;
    let nodes = parameter(link, address, function_group, PARAMETER_NODE_COUNT)?;
    let (first, count) = (((nodes >> 16) & 0xff) as u8, (nodes & 0xff) as u8);
    let mut widgets = Vec::new();
    for node in first..first.saturating_add(count) {
        let caps = parameter(link, address, node, PARAMETER_WIDGET_CAPS)?;
        let kind = WidgetType::from_caps(caps);
        let read = |link: &mut dyn CodecLink, which, present: bool, default| {
            if !present {
                Ok(0)
            } else if caps & CAPS_AMP_OVERRIDE != 0 {
                parameter(link, address, node, which)
            } else {
                Ok(default)
            }
        };
        let amp_in = read(link, PARAMETER_AMP_IN, caps & CAPS_IN_AMP != 0, default_in)?;
        let amp_out = read(
            link,
            PARAMETER_AMP_OUT,
            caps & CAPS_OUT_AMP != 0,
            default_out,
        )?;
        let pcm = if kind == WidgetType::Output && caps & CAPS_FORMAT_OVERRIDE != 0 {
            parameter(link, address, node, PARAMETER_PCM)?
        } else {
            default_pcm
        };
        let (pin_caps, pin_default) = if kind == WidgetType::Pin {
            (
                parameter(link, address, node, PARAMETER_PIN_CAPS)?,
                link.command(address, node, verb12(GET_CONFIG_DEFAULT, 0))
                    .ok_or(CodecError::NoResponse)?,
            )
        } else {
            (0, 0)
        };
        let connections = if caps & CAPS_CONNECTIONS != 0 {
            connections(link, address, node)?
        } else {
            Vec::new()
        };
        widgets.push(Widget {
            node,
            kind,
            caps,
            pin_caps,
            pin_default,
            amp_in,
            amp_out,
            pcm,
            connections,
        });
    }
    Ok(Codec {
        address,
        vendor,
        function_group,
        widgets,
    })
}

/// Shortest paths from `pin` back to every analog DAC it can reach, in
/// order of distance (pin first, DAC last in each).
fn paths_to_dacs(codec: &Codec, pin: u8) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    let mut visited = alloc::vec![pin];
    let mut queue = alloc::collections::VecDeque::from([alloc::vec![pin]]);
    while let Some(path) = queue.pop_front() {
        let Some(widget) = codec.widget(*path.last().unwrap()) else {
            continue;
        };
        for &source in &widget.connections {
            if visited.contains(&source) {
                continue;
            }
            visited.push(source);
            let Some(next) = codec.widget(source) else {
                continue;
            };
            let mut longer = path.clone();
            longer.push(source);
            match next.kind {
                WidgetType::Output if !next.digital() => found.push(longer),
                WidgetType::Mixer | WidgetType::Selector => queue.push_back(longer),
                _ => {}
            }
        }
    }
    found
}

/// Choose outputs and the DAC behind each.
pub fn plan_outputs(codec: &Codec) -> Result<OutputPlan, CodecError> {
    let mut pins: Vec<(&Widget, PinDevice)> = codec
        .widgets
        .iter()
        .filter_map(|widget| Some((widget, widget.output_device()?)))
        .collect();
    // Association, then sequence: the order the firmware groups them in.
    pins.sort_by_key(|(widget, _)| (widget.pin_default & 0xff, widget.node));
    let mut outputs: Vec<OutputPath> = Vec::new();
    for (pin, device) in pins {
        let paths = paths_to_dacs(codec, pin.node);
        let taken = |path: &&Vec<u8>| {
            outputs
                .iter()
                .any(|output| output.dac() == *path.last().unwrap())
        };
        let Some(nodes) = paths
            .iter()
            .find(|path| !taken(path))
            .or_else(|| paths.first())
        else {
            continue;
        };
        outputs.push(OutputPath {
            device,
            nodes: nodes.clone(),
        });
    }
    if outputs.is_empty() {
        return Err(CodecError::NoOutputs);
    }
    let mut dacs: Vec<u8> = outputs.iter().map(OutputPath::dac).collect();
    dacs.sort_unstable();
    dacs.dedup();
    let common = dacs.iter().fold(u32::MAX, |rates, &dac| {
        codec.widget(dac).map_or(0, |widget| widget.pcm) & rates
    });
    if common & PCM_16_BIT == 0 {
        return Err(CodecError::NoFormat);
    }
    let rates: Vec<u32> = RATES
        .iter()
        .enumerate()
        .filter(|&(bit, _)| common & (1 << bit) != 0)
        .map(|(_, &rate)| rate)
        .filter(|&rate| stream_format(rate, 2).is_some())
        .collect();
    if rates.is_empty() {
        return Err(CodecError::NoFormat);
    }
    Ok(OutputPlan {
        address: codec.address,
        vendor: codec.vendor,
        function_group: codec.function_group,
        outputs,
        dacs,
        rates,
    })
}

/// `SET_AMP_GAIN_MUTE` payload: output or input amp, both channels.
fn amp(output: bool, index: u8, gain: u8, mute: bool) -> u32 {
    let direction = if output { 1 << 15 } else { 1 << 14 };
    let payload = direction
        | (0b11 << 12)
        | (u16::from(index & 0xf) << 8)
        | (u16::from(mute) << 7)
        | u16::from(gain & 0x7f);
    verb4(SET_AMP_GAIN_MUTE, payload)
}

/// Power the outputs' paths on and route them (not the stream yet).
pub fn configure(link: &mut dyn CodecLink, codec: &Codec, plan: &OutputPlan) {
    let address = codec.address;
    let mut send = |node: u8, verb: u32| {
        let _ = link.command(address, node, verb);
    };
    send(codec.function_group, verb12(SET_POWER_STATE, 0));
    for output in &plan.outputs {
        for (position, &node) in output.nodes.iter().enumerate() {
            let Some(widget) = codec.widget(node) else {
                continue;
            };
            if widget.caps & CAPS_POWER != 0 {
                send(node, verb12(SET_POWER_STATE, 0));
            }
            // The widget feeding this one along the path.
            if let Some(&upstream) = output.nodes.get(position + 1)
                && let Some(index) = widget.connections.iter().position(|&n| n == upstream)
            {
                match widget.kind {
                    WidgetType::Mixer => {
                        if widget.caps & CAPS_IN_AMP != 0 {
                            send(
                                node,
                                amp(false, index as u8, (widget.amp_in & 0x7f) as u8, false),
                            );
                        }
                    }
                    _ if widget.connections.len() > 1 => {
                        send(node, verb12(SET_CONNECTION_SELECT, index as u8));
                    }
                    _ => {}
                }
            }
            if widget.caps & CAPS_OUT_AMP != 0 {
                send(node, amp(true, 0, (widget.amp_out & 0x7f) as u8, false));
            }
        }
        let pin = output.pin();
        let Some(widget) = codec.widget(pin) else {
            continue;
        };
        let mut control = PIN_CONTROL_OUT;
        if output.device == PinDevice::Headphone && widget.pin_caps & PIN_HEADPHONE_DRIVE != 0 {
            control |= PIN_CONTROL_HEADPHONE;
        }
        send(pin, verb12(SET_PIN_CONTROL, control));
        if widget.pin_caps & PIN_EAPD != 0 {
            send(pin, verb12(SET_EAPD, EAPD_ENABLE));
        }
    }
}

/// Point every DAC at stream `tag` (channels from 0) in `format`; tag 0
/// stops them.
pub fn set_stream(link: &mut dyn CodecLink, plan: &OutputPlan, tag: u8, format: u16) {
    for &dac in &plan.dacs {
        let _ = link.command(plan.address, dac, verb4(SET_CONVERTER_FORMAT, format));
        let _ = link.command(plan.address, dac, verb12(SET_CHANNEL_STREAM, tag << 4));
    }
}

/// The stream format word (controller `SDnFMT` and codec converter): base
/// rate 48 or 44.1 kHz (bit 14), multiplier (13:11), divisor (10:8), 16-bit
/// samples (6:4 = 1), channels - 1 (3:0).
pub fn stream_format(rate: u32, channels: u8) -> Option<u16> {
    let (base_44k, multiply, divide): (u16, u16, u16) = match rate {
        8_000 => (0, 1, 6),
        11_025 => (1, 1, 4),
        16_000 => (0, 1, 3),
        22_050 => (1, 1, 2),
        32_000 => (0, 2, 3),
        44_100 => (1, 1, 1),
        48_000 => (0, 1, 1),
        88_200 => (1, 2, 1),
        96_000 => (0, 2, 1),
        176_400 => (1, 4, 1),
        192_000 => (0, 4, 1),
        _ => return None,
    };
    if !(1..=16).contains(&channels) {
        return None;
    }
    Some(
        (base_44k << 14)
            | ((multiply - 1) << 11)
            | ((divide - 1) << 8)
            | (1 << 4)
            | u16::from(channels - 1),
    )
}

/// The card rate for a program asking `wanted`: itself if supported, else
/// the smallest supported whole multiple (22050 -> 44100, 8000 -> 48000:
/// clean resampling), else the nearest above, else the highest.
pub fn choose_rate(supported: &[u32], wanted: u32) -> u32 {
    if supported.contains(&wanted) {
        return wanted;
    }
    supported
        .iter()
        .copied()
        .find(|&rate| wanted != 0 && rate % wanted == 0)
        .or_else(|| supported.iter().copied().find(|&rate| rate > wanted))
        .or_else(|| supported.last().copied())
        .unwrap_or(48_000)
}
