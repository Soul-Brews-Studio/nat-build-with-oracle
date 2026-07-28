// audio.rs — mic capture via cpal, opening ALSA "default" which routes through the
// user PipeWire graph (coexists with PipeWire owning the Samson). The cpal callback
// thread downmixes to mono and pushes into a lock-free SPSC ring; the render loop drains.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, SampleRate, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

pub struct AudioCapture {
    _stream: cpal::Stream, // MUST stay alive; dropping it stops capture
    cons: ringbuf::HeapCons<f32>,
    pub sample_rate: u32,
}

impl AudioCapture {
    pub fn start() -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let dev = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("no default input device"))?;
        let def = dev.default_input_config()?;
        let ch = def.channels() as usize;
        let sr = def.sample_rate().0;
        eprintln!(
            "audio: device={:?} fmt={:?} ch={} sr={}",
            dev.name().unwrap_or_default(),
            def.sample_format(),
            ch,
            sr
        );
        let cfg = StreamConfig {
            channels: def.channels(),
            sample_rate: SampleRate(sr),
            buffer_size: BufferSize::Default,
        };

        let (mut prod, cons) = HeapRb::<f32>::new((sr as usize) / 2).split(); // ~0.5s headroom
        let err = |e| eprintln!("audio stream error: {e}");

        let stream = match def.sample_format() {
            SampleFormat::F32 => dev.build_input_stream(
                &cfg,
                move |d: &[f32], _: &_| {
                    for f in d.chunks_exact(ch) {
                        let m = f.iter().sum::<f32>() / ch as f32;
                        let _ = prod.try_push(m);
                    }
                },
                err,
                None,
            )?,
            SampleFormat::I16 => dev.build_input_stream(
                &cfg,
                move |d: &[i16], _: &_| {
                    for f in d.chunks_exact(ch) {
                        let mut s = 0.0f32;
                        for &x in f {
                            s += x as f32 / 32768.0;
                        }
                        let _ = prod.try_push(s / ch as f32);
                    }
                },
                err,
                None,
            )?,
            SampleFormat::U16 => dev.build_input_stream(
                &cfg,
                move |d: &[u16], _: &_| {
                    for f in d.chunks_exact(ch) {
                        let mut s = 0.0f32;
                        for &x in f {
                            s += (x as f32 - 32768.0) / 32768.0;
                        }
                        let _ = prod.try_push(s / ch as f32);
                    }
                },
                err,
                None,
            )?,
            f => anyhow::bail!("unsupported sample format {f:?}"),
        };
        stream.play()?;
        Ok(Self {
            _stream: stream,
            cons,
            sample_rate: sr,
        })
    }

    /// Drain the newest available samples into `out`, returns count written.
    pub fn drain(&mut self, out: &mut [f32]) -> usize {
        self.cons.pop_slice(out)
    }
}
