// flowengine — wgpu/winit port of the Linux/DRM audio-reactive visualizer.
//
// Locked stack (every wgpu/winit API below is written for EXACTLY these):
//     wgpu   = "25.0"   (resolves 25.0.2 — Metal on Apple Silicon, Vulkan on Linux)
//     winit  = "0.30"   (resolves 0.30.13 — ApplicationHandler + run_app model)
//     pollster = "0.4", bytemuck = "1", image = "0.25", anyhow = "1"
//     cpal = "0.15", ringbuf = "0.4", realfft = "3"  (audio/dsp ported unchanged)
//
// audio.rs + dsp.rs are copied verbatim from flowengine/src (backend-agnostic:
// cpal picks CoreAudio on macOS, the default input = the Mac mic).

mod audio;
mod dsp;

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const SPEC_BINS: u32 = 256;
const SNAPSHOT_EVERY: u32 = 30; // frames (~0.5s at 60fps)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    time: f32,     // off 0
    level: f32,    // off 4
    peak: f32,     // off 8
    bass: f32,     // off 12
    res: [f32; 2], // off 16  (vec2 align 8; 16 is a multiple of 8)
    _pad: [f32; 2], // off 24 -> total 32 (UBO 16-byte multiple)
}
// Guard the ABI: WGSL derives struct size 24; Rust hands a 32-byte buffer (a legal
// superset), with `res` landing exactly on offset 16 on both sides.
const _: () = assert!(std::mem::size_of::<Uniforms>() == 32);

/// All GPU + per-frame state. Built once the window exists (async -> pollster).
struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>, // 'static because we hand it an Arc<Window>
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,

    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    spectrum_tex: wgpu::Texture,

    // audio/dsp (ported modules)
    capture: audio::AudioCapture,
    analyzer: dsp::AudioAnalyzer,
    ring: Vec<f32>,    // scratch for drained samples
    win_buf: Vec<f32>, // rolling 1024-sample FFT window

    start: Instant,
    last: Instant,
    frame: u32,

    // snapshot readback
    can_snapshot: bool,
    readback: wgpu::Buffer,
    padded_bpr: u32,
}

impl Gfx {
    async fn new(window: Arc<Window>) -> anyhow::Result<Self> {
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        // --- Instance / Surface / Adapter / Device / Queue ---
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY, // Metal on macOS, Vulkan on Linux
            ..Default::default()
        });

        // Arc<Window> -> Surface<'static>. Gfx owns both, so the surface never
        // outlives the window.
        let surface = instance.create_surface(window.clone())?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("no compatible GPU adapter"); // Result in wgpu 25

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("flowengine-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off, // wgpu 25: trace lives in the descriptor
            })
            .await?;

        // --- Surface configuration ---
        let caps = surface.get_capabilities(&adapter);
        // The scene shader already bakes tonemap + pow(col,0.85), so we render into
        // a NON-sRGB target (typically Bgra8Unorm on Metal) to match the original
        // GLES output 1:1. Using an _srgb view here would double-gamma.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        // COPY_SRC lets us read the frame back for PNG snapshots (supported on Metal).
        let can_snapshot = caps.usages.contains(wgpu::TextureUsages::COPY_SRC);
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        if can_snapshot {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }

        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            width: w,
            height: h,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // --- Uniform buffer ---
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- Spectrum texture (256x1 R8Unorm) + sampler ---
        let spectrum_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("spectrum"),
            size: wgpu::Extent3d {
                width: SPEC_BINS,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let spectrum_view = spectrum_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("spec-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // --- Bind group layout / bind group ---
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene-bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&spectrum_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        // --- Pipeline (fullscreen triangle, no vertex buffers) ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene-layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[], // fullscreen triangle synthesized from vertex_index
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(), // TriangleList
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // --- Audio / DSP (ported modules) ---
        let capture = audio::AudioCapture::start()?;
        let analyzer = dsp::AudioAnalyzer::new(capture.sample_rate);

        // --- Snapshot readback buffer (rows padded to 256B) ---
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT; // 256
        let padded_bpr = (w * 4).div_ceil(align) * align;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded_bpr * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let now = Instant::now();
        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            size,
            pipeline,
            bind_group,
            uniform_buf,
            spectrum_tex,
            capture,
            analyzer,
            ring: vec![0.0; 4096],
            win_buf: vec![0.0; 1024],
            start: now,
            last: now,
            frame: 0,
            can_snapshot,
            readback,
            padded_bpr,
        })
    }

    fn resize(&mut self, new: winit::dpi::PhysicalSize<u32>) {
        if new.width == 0 || new.height == 0 {
            return;
        }
        self.size = new;
        self.config.width = new.width;
        self.config.height = new.height;
        self.surface.configure(&self.device, &self.config);

        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        self.padded_bpr = (new.width * 4).div_ceil(align) * align;
        self.readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (self.padded_bpr * new.height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
    }

    fn update_audio(&mut self) {
        // Drain newest mic samples, keep the last 1024 as the FFT window.
        let n = self.capture.drain(&mut self.ring);
        if n > 0 {
            let take = n.min(self.win_buf.len());
            let src = &self.ring[n - take..n];
            self.win_buf.copy_within(take.., 0);
            let len = self.win_buf.len();
            self.win_buf[len - take..].copy_from_slice(src);
        }
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;

        self.update_audio();
        let (level, peak, bass) = self.analyzer.process(&self.win_buf, dt);

        // upload uniforms
        let u = Uniforms {
            time: (now - self.start).as_secs_f32(),
            level,
            peak,
            bass,
            res: [self.config.width as f32, self.config.height as f32],
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        // upload spectrum bytes -> R8Unorm texture (256 wide = exactly one 256B row)
        self.queue.write_texture(
            self.spectrum_tex.as_image_copy(),
            &self.analyzer.bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SPEC_BINS),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: SPEC_BINS,
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        // acquire, draw, present
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, &self.bind_group, &[]);
            rp.draw(0..3, 0..1); // fullscreen triangle
        }

        // periodic snapshot: copy this frame's texture into the readback buffer
        let do_snap = self.can_snapshot && self.frame % SNAPSHOT_EVERY == 0;
        if do_snap {
            enc.copy_texture_to_buffer(
                frame.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(self.padded_bpr),
                        rows_per_image: Some(self.config.height),
                    },
                },
                wgpu::Extent3d {
                    width: self.config.width,
                    height: self.config.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        self.queue.submit(std::iter::once(enc.finish()));
        frame.present();

        if do_snap {
            self.write_png();
        }
        self.frame = self.frame.wrapping_add(1);
        Ok(())
    }

    fn write_png(&self) {
        let (w, h) = (self.config.width, self.config.height);
        let slice = self.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        // block until copy+map completes (wgpu 25: PollType, not Maintain)
        let _ = self.device.poll(wgpu::PollType::Wait);

        let data = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        let bgra = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        for row in 0..h {
            let start = (row * self.padded_bpr) as usize;
            let line = &data[start..start + (w * 4) as usize];
            if bgra {
                for px in line.chunks_exact(4) {
                    rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]); // BGRA->RGBA
                }
            } else {
                rgba.extend_from_slice(line);
            }
        }
        drop(data);
        self.readback.unmap();

        if let Some(buf) = image::RgbaImage::from_raw(w, h, rgba) {
            let _ = buf.save("/tmp/flowsnap.png"); // deterministic, in-app
        }
    }
}

/// winit 0.30 ApplicationHandler. The GPU is built lazily in `resumed`.
#[derive(Default)]
struct App {
    gfx: Option<Gfx>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return; // don't rebuild on resume
        }
        let attrs = Window::default_attributes()
            .with_title("flowengine (wgpu/Metal)")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        match pollster::block_on(Gfx::new(window)) {
            Ok(g) => self.gfx = Some(g),
            Err(e) => {
                eprintln!("gpu init failed: {e:#}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(gfx) = self.gfx.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit()
            }
            WindowEvent::Resized(new) => gfx.resize(new),
            WindowEvent::RedrawRequested => match gfx.render() {
                Ok(()) => {}
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    gfx.resize(gfx.size)
                }
                Err(wgpu::SurfaceError::OutOfMemory) => event_loop.exit(),
                Err(e) => eprintln!("surface error: {e:?}"),
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // continuous animation: request a redraw every wakeup
        if let Some(gfx) = self.gfx.as_ref() {
            gfx.window.request_redraw();
        }
    }
}

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}
