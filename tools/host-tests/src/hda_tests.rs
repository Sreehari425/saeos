//! The HD Audio codec layer against fake codecs built from
//! `/proc/asound/card*/codec#*` dumps: a real Realtek ALC257 and ATI HDMI
//! codec (from a Lenovo laptop), and a hand-made codec with the awkward
//! cases. The fake answers the GET verbs from the dump and records every
//! SET verb, so the tests check exactly what the driver would tell the
//! hardware.

use crate::hda_codec::{
    CodecError, CodecLink, EAPD_ENABLE, GET_CONFIG_DEFAULT, GET_CONNECTION_LIST, GET_PARAMETER,
    PARAMETER_AMP_IN, PARAMETER_AMP_OUT, PARAMETER_CONNECTIONS, PARAMETER_FUNCTION_TYPE,
    PARAMETER_NODE_COUNT, PARAMETER_PCM, PARAMETER_PIN_CAPS, PARAMETER_VENDOR,
    PARAMETER_WIDGET_CAPS, PIN_CONTROL_HEADPHONE, PIN_CONTROL_OUT, PinDevice, SET_AMP_GAIN_MUTE,
    SET_CHANNEL_STREAM, SET_CONNECTION_SELECT, SET_CONVERTER_FORMAT, SET_EAPD, SET_PIN_CONTROL,
    SET_POWER_STATE, choose_rate, command_word, configure, discover, plan_outputs, set_stream,
    stream_format, verb4, verb12,
};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
struct Node {
    caps: u32,
    pin_caps: u32,
    pin_default: u32,
    amp_in: Option<u32>,
    amp_out: Option<u32>,
    pcm: Option<u32>,
    connections: Vec<u8>,
}

/// Amp state: (node, output?, index) -> (gain, muted).
type Amps = BTreeMap<(u8, bool, u8), (u8, bool)>;

#[derive(Debug, Default)]
struct FakeCodec {
    vendor: u32,
    default_pcm: u32,
    default_in: u32,
    default_out: u32,
    nodes: BTreeMap<u8, Node>,
    // What the driver set.
    power: BTreeMap<u8, u8>,
    select: BTreeMap<u8, u8>,
    pin_control: BTreeMap<u8, u8>,
    eapd: BTreeMap<u8, u8>,
    amps: Amps,
    stream: BTreeMap<u8, u8>,
    format: BTreeMap<u8, u16>,
    sets: usize,
}

fn hex(text: &str) -> u32 {
    let text = text.trim().trim_end_matches(':').trim_end_matches(',');
    u32::from_str_radix(text.trim_start_matches("0x"), 16).unwrap()
}

/// `ofs=0x57, nsteps=0x57, stepsize=0x02, mute=0` as an amp caps word.
fn amp_caps(text: &str) -> Option<u32> {
    if text.contains("N/A") {
        return None;
    }
    let field = |name: &str| {
        let start = text.find(name).unwrap() + name.len();
        let value = text[start..].split([',', ' ']).next().unwrap();
        if value.starts_with("0x") {
            hex(value)
        } else {
            value.parse().unwrap()
        }
    };
    Some(field("mute=") << 31 | field("stepsize=") << 16 | field("nsteps=") << 8 | field("ofs="))
}

/// Every codec in a dump.
fn parse_dump(text: &str) -> Vec<FakeCodec> {
    let mut codecs: Vec<FakeCodec> = Vec::new();
    let mut lines = text.lines().peekable();
    let mut node: Option<u8> = None;
    let mut pcm_target: Option<Option<u8>> = None;
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with("Codec:") {
            codecs.push(FakeCodec::default());
            node = None;
            continue;
        }
        let codec = codecs.last_mut().expect("a dump starts with Codec:");
        if let Some(rest) = trimmed.strip_prefix("Vendor Id:") {
            codec.vendor = hex(rest);
        } else if trimmed == "Default PCM:" {
            pcm_target = Some(None);
        } else if trimmed == "PCM:" {
            pcm_target = Some(node);
        } else if let Some(rest) = trimmed.strip_prefix("rates [") {
            let rates = hex(rest.split(']').next().unwrap());
            let target = pcm_target.unwrap();
            let pcm = match target {
                None => &mut codec.default_pcm,
                Some(nid) => codec.nodes.get_mut(&nid).unwrap().pcm.get_or_insert(0),
            };
            *pcm |= rates;
        } else if let Some(rest) = trimmed.strip_prefix("bits [") {
            let bits = hex(rest.split(']').next().unwrap());
            let target = pcm_target.unwrap();
            let pcm = match target {
                None => &mut codec.default_pcm,
                Some(nid) => codec.nodes.get_mut(&nid).unwrap().pcm.get_or_insert(0),
            };
            *pcm |= bits << 16;
        } else if let Some(rest) = trimmed.strip_prefix("Default Amp-In caps:") {
            codec.default_in = amp_caps(rest).unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("Default Amp-Out caps:") {
            codec.default_out = amp_caps(rest).unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("Node ") {
            let nid = hex(rest.split(' ').next().unwrap()) as u8;
            let caps = hex(rest
                .split("wcaps ")
                .nth(1)
                .unwrap()
                .split(':')
                .next()
                .unwrap());
            codec.nodes.insert(
                nid,
                Node {
                    caps,
                    ..Node::default()
                },
            );
            node = Some(nid);
        } else if let Some(nid) = node {
            let entry = codec.nodes.get_mut(&nid).unwrap();
            if let Some(rest) = trimmed.strip_prefix("Pincap ") {
                entry.pin_caps = hex(rest.split(':').next().unwrap());
            } else if let Some(rest) = trimmed.strip_prefix("Pin Default ") {
                entry.pin_default = hex(rest.split(':').next().unwrap());
            } else if let Some(rest) = trimmed.strip_prefix("Amp-In caps:") {
                entry.amp_in = amp_caps(rest);
            } else if let Some(rest) = trimmed.strip_prefix("Amp-Out caps:") {
                entry.amp_out = amp_caps(rest);
            } else if trimmed.starts_with("Connection:") {
                let list = lines.next().unwrap();
                entry.connections = list
                    .split_whitespace()
                    .map(|item| hex(item.trim_end_matches('*')) as u8)
                    .collect();
            }
        }
    }
    codecs
}

/// Embedded, so the tests also run under Miri (which has no file system).
fn fixture(name: &str) -> Vec<FakeCodec> {
    let text = match name {
        "alc257-lenovo.txt" => include_str!("../fixtures/hda/alc257-lenovo.txt"),
        "ati-hdmi.txt" => include_str!("../fixtures/hda/ati-hdmi.txt"),
        "awkward.txt" => include_str!("../fixtures/hda/awkward.txt"),
        other => panic!("no fixture {other}"),
    };
    parse_dump(text)
}

impl FakeCodec {
    fn first_and_count(&self) -> (u8, u8) {
        let first = *self.nodes.keys().next().unwrap();
        let last = *self.nodes.keys().last().unwrap();
        (first, last - first + 1)
    }

    fn parameter(&self, node: u8, which: u8) -> u32 {
        if node == 0 {
            return match which {
                PARAMETER_VENDOR => self.vendor,
                PARAMETER_NODE_COUNT => (1 << 16) | 1,
                _ => 0,
            };
        }
        if node == 1 {
            let (first, count) = self.first_and_count();
            return match which {
                PARAMETER_FUNCTION_TYPE => 1,
                PARAMETER_NODE_COUNT => (u32::from(first) << 16) | u32::from(count),
                PARAMETER_PCM => self.default_pcm,
                PARAMETER_AMP_IN => self.default_in,
                PARAMETER_AMP_OUT => self.default_out,
                _ => 0,
            };
        }
        let Some(widget) = self.nodes.get(&node) else {
            return 0;
        };
        match which {
            PARAMETER_WIDGET_CAPS => widget.caps,
            PARAMETER_PCM => widget.pcm.unwrap_or(self.default_pcm),
            PARAMETER_PIN_CAPS => widget.pin_caps,
            PARAMETER_AMP_IN => widget.amp_in.unwrap_or(self.default_in),
            PARAMETER_AMP_OUT => widget.amp_out.unwrap_or(self.default_out),
            PARAMETER_CONNECTIONS => widget.connections.len() as u32,
            _ => 0,
        }
    }
}

impl CodecLink for FakeCodec {
    fn command(&mut self, codec: u8, node: u8, verb: u32) -> Option<u32> {
        assert_eq!(codec, 0, "one codec address in these fixtures");
        let payload8 = (verb & 0xff) as u8;
        let payload16 = (verb & 0xffff) as u16;
        match verb >> 16 {
            0x2 => {
                self.sets += 1;
                self.format.insert(node, payload16);
                return Some(0);
            }
            0x3 => {
                self.sets += 1;
                let output = payload16 & (1 << 15) != 0;
                assert!(
                    output || payload16 & (1 << 14) != 0,
                    "an amp verb must name input or output"
                );
                assert_eq!(payload16 & (0b11 << 12), 0b11 << 12, "both channels");
                let index = ((payload16 >> 8) & 0xf) as u8;
                let muted = payload16 & 0x80 != 0;
                self.amps
                    .insert((node, output, index), ((payload16 & 0x7f) as u8, muted));
                return Some(0);
            }
            _ => {}
        }
        let id = (verb >> 8) as u16;
        let response = match id {
            GET_PARAMETER => self.parameter(node, payload8),
            GET_CONFIG_DEFAULT => self.nodes.get(&node).map_or(0, |widget| widget.pin_default),
            GET_CONNECTION_LIST => {
                let list = self
                    .nodes
                    .get(&node)
                    .map(|widget| widget.connections.clone())
                    .unwrap_or_default();
                (0..4)
                    .filter_map(|slot| list.get(usize::from(payload8) + slot).map(|&n| (slot, n)))
                    .fold(0, |word, (slot, n)| word | (u32::from(n) << (slot * 8)))
            }
            _ => {
                self.sets += 1;
                let map = match id {
                    SET_POWER_STATE => &mut self.power,
                    SET_CONNECTION_SELECT => &mut self.select,
                    SET_PIN_CONTROL => &mut self.pin_control,
                    SET_EAPD => &mut self.eapd,
                    SET_CHANNEL_STREAM => &mut self.stream,
                    other => panic!("unexpected verb {other:#x} to node {node:#x}"),
                };
                map.insert(node, payload8);
                0
            }
        };
        Some(response)
    }
}

fn paths(plan: &crate::hda_codec::OutputPlan) -> Vec<(PinDevice, Vec<u8>)> {
    plan.outputs
        .iter()
        .map(|output| (output.device, output.nodes.clone()))
        .collect()
}

#[test]
fn alc257_speaker_and_headphone_get_their_own_dacs() {
    let mut codecs = fixture("alc257-lenovo.txt");
    let mut fake = codecs.remove(0);
    let codec = discover(&mut fake, 0).unwrap();
    assert_eq!(codec.vendor, 0x10ec0257);
    assert_eq!(codec.function_group, 1);
    let plan = plan_outputs(&codec).unwrap();
    assert_eq!(
        paths(&plan),
        [
            (PinDevice::Speaker, vec![0x14, 0x02]),
            (PinDevice::Headphone, vec![0x21, 0x03]),
        ]
    );
    assert_eq!(plan.dacs, [0x02, 0x03]);
    assert_eq!(plan.rates, [44_100, 48_000]);

    configure(&mut fake, &codec, &plan);
    // Powered on: the function group and every widget on the paths.
    for node in [0x01, 0x02, 0x03, 0x14, 0x21] {
        assert_eq!(fake.power.get(&node), Some(&0), "node {node:#x} in D0");
    }
    // Headphone pin 0x21 lists 0x02 0x03: select index 1.
    assert_eq!(fake.select, BTreeMap::from([(0x21, 1)]));
    assert_eq!(
        fake.pin_control,
        BTreeMap::from([
            (0x14, PIN_CONTROL_OUT),
            (0x21, PIN_CONTROL_OUT | PIN_CONTROL_HEADPHONE)
        ])
    );
    // Laptop speakers stay silent without EAPD.
    assert_eq!(
        fake.eapd,
        BTreeMap::from([(0x14, EAPD_ENABLE), (0x21, EAPD_ENABLE)])
    );
    // DACs at 0 dB (their offset step, 0x57), pin amps unmuted.
    assert_eq!(
        fake.amps,
        BTreeMap::from([
            ((0x02, true, 0), (0x57, false)),
            ((0x03, true, 0), (0x57, false)),
            ((0x14, true, 0), (0x00, false)),
            ((0x21, true, 0), (0x00, false)),
        ])
    );

    let format = stream_format(48_000, 2).unwrap();
    set_stream(&mut fake, &plan, 1, format);
    assert_eq!(fake.stream, BTreeMap::from([(0x02, 0x10), (0x03, 0x10)]));
    assert_eq!(
        fake.format,
        BTreeMap::from([(0x02, format), (0x03, format)])
    );
    assert!(plan.describe().contains("output: headphone 0x21 <- 0x03"));
}

#[test]
fn hdmi_only_codec_has_no_outputs_and_is_left_alone() {
    let mut codecs = fixture("ati-hdmi.txt");
    let mut fake = codecs.remove(0);
    let codec = discover(&mut fake, 0).unwrap();
    assert_eq!(codec.vendor, 0x1002aa01);
    assert_eq!(plan_outputs(&codec), Err(CodecError::NoOutputs));
    assert_eq!(fake.sets, 0, "discovery only reads");
}

#[test]
fn awkward_codec_routes_through_mixers_and_selectors() {
    let mut fake = fixture("awkward.txt").remove(0);
    let codec = discover(&mut fake, 0).unwrap();
    let plan = plan_outputs(&codec).unwrap();
    assert_eq!(
        paths(&plan),
        [
            // The shortest way: the selector straight to DAC 0x04.
            (PinDevice::Speaker, vec![0x08, 0x07, 0x04]),
            // Through the mixer to the DAC no one has yet.
            (PinDevice::Headphone, vec![0x09, 0x05, 0x03]),
            // Every DAC it reaches is taken: it shares the nearest.
            (PinDevice::LineOut, vec![0x0c, 0x07, 0x04]),
        ],
        "digital DAC 0x02, S/PDIF pin 0x0b and unconnected 0x0a are left out"
    );
    assert_eq!(plan.dacs, [0x03, 0x04]);
    configure(&mut fake, &codec, &plan);
    // Selector 0x07 lists the digital DAC first: 0x04 is index 1.
    assert_eq!(fake.select, BTreeMap::from([(0x07, 1)]));
    assert_eq!(
        fake.amps,
        BTreeMap::from([
            // The DACs use the function group's default amp: 0 dB at 0x20.
            ((0x03, true, 0), (0x20, false)),
            ((0x04, true, 0), (0x20, false)),
            // Only the mixer input on the path; the mic input stays muted.
            ((0x05, false, 0), (0x00, false)),
        ])
    );
    assert_eq!(fake.eapd, BTreeMap::from([(0x09, EAPD_ENABLE)]));
    assert_eq!(
        fake.pin_control,
        BTreeMap::from([
            (0x08, PIN_CONTROL_OUT),
            (0x09, PIN_CONTROL_OUT | PIN_CONTROL_HEADPHONE),
            (0x0c, PIN_CONTROL_OUT),
        ])
    );
}

#[test]
fn verbs_and_formats_encode_as_the_spec_says() {
    assert_eq!(verb12(0xf00, 0x09), 0xf0009);
    assert_eq!(verb4(SET_AMP_GAIN_MUTE, 0xb057), 0x3b057);
    assert_eq!(verb4(SET_CONVERTER_FORMAT, 0x11), 0x20011);
    assert_eq!(command_word(2, 0x21, 0x70740), 0x2217_0740);
    // 48 kHz stereo 16-bit: 0x0011; 44.1 kHz: base bit 14.
    assert_eq!(stream_format(48_000, 2), Some(0x0011));
    assert_eq!(stream_format(44_100, 2), Some(0x4011));
    // 8 kHz = 48 kHz / 6; 22.05 kHz = 44.1 kHz / 2; 96 kHz = 48 kHz x 2.
    assert_eq!(stream_format(8_000, 2), Some(0x0511));
    assert_eq!(stream_format(22_050, 1), Some(0x4110));
    assert_eq!(stream_format(96_000, 2), Some(0x0811));
    assert_eq!(stream_format(12_345, 2), None);
}

#[test]
fn rates_round_to_clean_multiples() {
    let both = [44_100, 48_000];
    assert_eq!(choose_rate(&both, 48_000), 48_000);
    assert_eq!(choose_rate(&both, 22_050), 44_100);
    assert_eq!(choose_rate(&both, 11_025), 44_100);
    assert_eq!(choose_rate(&both, 8_000), 48_000);
    assert_eq!(choose_rate(&both, 16_000), 48_000);
    assert_eq!(choose_rate(&both, 30_000), 44_100);
    assert_eq!(choose_rate(&both, 96_000), 48_000);
    assert_eq!(choose_rate(&[48_000], 44_100), 48_000);
}
