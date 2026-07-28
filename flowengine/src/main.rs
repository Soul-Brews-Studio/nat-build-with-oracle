// flowengine — native audio-reactive GPU engine on Raspberry Pi VideoCore.
// Two-pass: the heavy visual renders to a 720p FBO, then a cheap pass upscales it to
// 1080p and overlays a crisp full-res dev HUD. Direct DRM/KMS scanout, no compositor.
mod audio;
mod dsp;
mod text;

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::os::unix::io::{AsFd, BorrowedFd};
use std::time::{Duration, Instant};

use drm::control::{connector, crtc, framebuffer, Device as ControlDevice};
use drm::Device as BasicDevice;
use gbm::{AsRaw, BufferObjectFlags, Device as GbmDevice, Format};
use glow::HasContext;
use khronos_egl as egl;

const RENDER_W: i32 = 1280;
const RENDER_H: i32 = 720;

struct Card(std::fs::File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl BasicDevice for Card {}
impl ControlDevice for Card {}

// --- VT-switch awareness: coexist with the desktop; `chvt` hands the screens over. ---
// The kernel signals us when the user switches VT away (release) / back (acquire); we
// drop / re-grab DRM master and re-modeset, so no stop/start is needed to share HDMI.
use std::sync::atomic::{AtomicBool, Ordering};
static VT_RELEASE: AtomicBool = AtomicBool::new(false);
static VT_ACQUIRE: AtomicBool = AtomicBool::new(false);
extern "C" fn vt_relsig(_: libc::c_int) { VT_RELEASE.store(true, Ordering::SeqCst); }
extern "C" fn vt_acqsig(_: libc::c_int) { VT_ACQUIRE.store(true, Ordering::SeqCst); }
const VT_SETMODE: libc::c_ulong = 0x5602; // linux/vt.h
const VT_RELDISP: libc::c_ulong = 0x5605;
const VT_PROCESS: libc::c_char = 1;
const VT_ACKACQ: libc::c_int = 2;
#[repr(C)]
struct VtMode {
    mode: libc::c_char,
    waitv: libc::c_char,
    relsig: libc::c_short,
    acqsig: libc::c_short,
    frsig: libc::c_short,
}

const VS: &str = r#"attribute vec2 p; void main(){ gl_Position = vec4(p,0.0,1.0); }"#;

// Pass 1 — the alien Jarvis visual (rendered at 720p). No HUD here.
const FS_SCENE: &str = r#"
precision highp float;
uniform float u_time, u_level, u_peak, u_bass, u_voice, u_flip;
uniform vec2  u_res;
uniform sampler2D u_spec;
#define TAU 6.28318530718
#define RAD 0.40
// "Starry Night porthole": swirling Van Gogh sky + FFT voiceprint, cropped to a living
// golden circle. u_flip: 0 = FFT bars inward, 1 = outward.
void main(){
  vec2 uv=(gl_FragCoord.xy-0.5*u_res)/u_res.y;
  float r=length(uv);
  float t=u_time;
  float lv=clamp(u_level,0.0,1.0);
  float voice=u_voice;
  float drive=max(lv,u_bass);
  float dc=drive/(0.6+drive);

  vec3 deep=vec3(0.02,0.05,0.15);
  vec3 ultra=vec3(0.08,0.28,0.68);
  vec3 cyan=vec3(0.32,0.72,1.0);
  vec3 gold=vec3(1.0,0.82,0.26);

  // swirling flow field: domain-warp uv into eddies (flows, never strobes)
  vec2 p=uv;
  float turb=0.26+0.50*voice+0.12*lv;
  for(int k=0;k<3;k++){
    float fk=2.2+float(k)*2.3;
    p+=turb*0.14*vec2(sin(fk*p.y+t*0.55+float(k)*1.7), cos(fk*p.x-t*0.48+float(k)*1.1));
  }
  float rw=length(p);
  float sang=atan(p.y,p.x);

  // painterly brushstrokes following the swirl
  float stroke=0.5+0.5*sin(rw*30.0 - t*1.1 + 5.0*sin(sang*2.0+t*0.2));
  float paint=0.5+0.5*sin(p.x*20.0 + 9.0*p.y + t*0.15);
  float tex=mix(stroke,paint,0.4);

  vec3 col=deep;
  float body=tex*0.9+0.14;
  col=mix(col,ultra,clamp(body,0.0,1.0));
  float crest=tex*tex*tex*tex;
  col += cyan*crest*(0.35+0.6*lv);
  col += gold*smoothstep(0.78,0.98,tex)*(0.22+0.7*voice);

  // central golden star + slow radiant rays
  float breath=0.5+0.5*sin(t*0.6);
  float core=exp(-r*r*(26.0-10.0*dc));
  col += mix(cyan,gold,0.35+0.5*dc)*core*(0.30+0.9*dc);
  col += gold*exp(-r*r*90.0)*(0.40+0.8*dc);
  float rays=0.5+0.5*sin(sang*6.0+t*0.3);
  col += gold*rays*exp(-r*r*12.0)*(0.06+0.10*breath+0.20*voice);

  // FFT voiceprint bars hanging from the golden rim; u_flip toggles inward/outward
  float a0=atan(uv.y,uv.x);
  float aa=fract(a0/TAU+0.75);
  float bin=aa*aa;
  float mag=texture2D(u_spec, vec2(clamp(bin,0.003,0.997),0.5)).r;
  mag=mag*mag;
  float slot=fract((a0/TAU)*96.0);
  float barMask=smoothstep(0.60,0.28, abs(slot-0.5)*2.0);
  float barLen=0.20;
  float inner=RAD-mag*barLen;
  float outb=RAD+mag*barLen;
  float inA=step(inner,r)*step(r,RAD);
  float outA=step(RAD,r)*step(r,outb);
  float inbar=mix(inA,outA,u_flip)*barMask;
  float tipR=mix(inner,outb,u_flip);
  float barGate=0.18+1.05*voice;
  vec3 fbar=vec3(0.0);
  fbar += mix(gold,cyan,mag)*inbar*(0.40+0.8*mag)*barGate;
  float dtip=(r-tipR)*42.0;
  fbar += gold*exp(-dtip*dtip)*barMask*mag*barGate;

  // peak-hold bloom (white-gold flush)
  float hold=u_peak*u_peak;
  col += mix(vec3(1.0),gold,0.5)*exp(-r*r*5.0)*hold*0.6;

  // vignette + tonemap + gamma on the sky
  col *= 0.55+0.45*smoothstep(RAD,0.05,r);
  col = col/(1.0+col);
  col = pow(col, vec3(0.85));

  // TWO ZONES: inside vivid; outside dimmed to a faint filtered backdrop (not black)
  float disc=1.0-smoothstep(RAD-0.02,RAD+0.08,r);
  col *= mix(0.15,1.0,disc);

  // FFT bars on top (full brightness in or out)
  col += fbar/(1.0+fbar);

  // LIVING gold rim: breathes, a shimmer courses around, a glint travels the ring
  float breathR=RAD+0.006*sin(t*1.3);
  float drim=(r-breathR)*55.0;
  float ring=exp(-drim*drim);
  float flow=0.70+0.30*sin(a0*5.0-t*1.6)*(0.6+0.4*sin(a0*2.0+t*0.7));
  float rimI=0.40+0.10*sin(t*1.3)+0.55*voice;
  col += gold*ring*flow*rimI;
  float glint=pow(max(0.5+0.5*sin(a0-t*0.9),0.0),8.0);
  col += vec3(1.0,0.94,0.65)*ring*glint*(0.5+0.6*voice);

  col = clamp(col,0.0,1.0);
  gl_FragColor=vec4(col,1.0);
}
"#;

// Pass 2 — upscale the 720p scene to 1080p + crisp full-res HUD text overlay.
const FS_COMP: &str = r#"
precision highp float;
uniform vec2 u_res;
uniform sampler2D u_scene;
uniform sampler2D u_hud;
void main(){
  vec2 uv = gl_FragCoord.xy / u_res;
  vec3 col = texture2D(u_scene, uv).rgb;
  float sc=2.0;
  float rxT=(gl_FragCoord.x-22.0)/sc;
  float grT=(u_res.y-22.0-gl_FragCoord.y)/sc;
  if(rxT>=0.0&&rxT<768.0&&grT>=0.0&&grT<8.0){
    float a=texture2D(u_hud, vec2(rxT/768.0, grT/16.0)).r;
    col=mix(col, vec3(0.62,0.90,1.0), a);
  }
  float rxB=(gl_FragCoord.x-22.0)/sc;
  float grB=(22.0+8.0*sc-gl_FragCoord.y)/sc;
  if(rxB>=0.0&&rxB<768.0&&grB>=0.0&&grB<8.0){
    float a=texture2D(u_hud, vec2(rxB/768.0, (8.0+grB)/16.0)).r;
    col=mix(col, vec3(0.42,1.0,0.80), a);
  }
  gl_FragColor=vec4(col,1.0);
}
"#;

fn compile_program(gl: &glow::Context, fs: &str) -> glow::Program {
    unsafe {
        let p = gl.create_program().unwrap();
        for (ty, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, fs)] {
            let sh = gl.create_shader(ty).unwrap();
            gl.shader_source(sh, src);
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                panic!("shader: {}", gl.get_shader_info_log(sh));
            }
            gl.attach_shader(p, sh);
        }
        gl.bind_attrib_location(p, 0, "p");
        gl.link_program(p);
        if !gl.get_program_link_status(p) {
            panic!("link: {}", gl.get_program_info_log(p));
        }
        p
    }
}

fn main() -> anyhow::Result<()> {
    if let Ok(uid) = std::env::var("SUDO_UID") {
        let run = format!("/run/user/{uid}");
        std::env::set_var("XDG_RUNTIME_DIR", &run);
        std::env::set_var("PULSE_SERVER", format!("unix:{run}/pulse/native"));
    }

    let card_path = std::env::args().nth(1).unwrap_or_else(|| "/dev/dri/card1".into());
    let duration: f64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0.0);

    let mut audio = match audio::AudioCapture::start() {
        Ok(a) => { eprintln!("audio: capture started @ {} Hz", a.sample_rate); Some(a) }
        Err(e) => { eprintln!("audio: DISABLED ({e})"); None }
    };
    let sr = audio.as_ref().map(|a| a.sample_rate).unwrap_or(48000);
    let mut analyzer = dsp::AudioAnalyzer::new(sr);
    let mut window = [0.0f32; 1024];
    let mut drain = vec![0.0f32; 8192];
    let mut last_audio = Instant::now();

    let file = OpenOptions::new().read(true).write(true).open(&card_path)?;
    let gbm = GbmDevice::new(Card(file))?;
    let res = gbm.resource_handles()?;
    // ALL connected screens (dual-HDMI CLONE): one rendered framebuffer, scanned out onto
    // every connected connector, each on its own CRTC. All chosen modes share the FB size.
    let conns: Vec<_> = res.connectors().iter()
        .filter_map(|&c| gbm.get_connector(c, true).ok())
        .filter(|c| c.state() == connector::State::Connected && !c.modes().is_empty())
        .collect();
    let first_conn = conns.first().ok_or_else(|| anyhow::anyhow!("no connected connector"))?;
    let mode = first_conn.modes().iter()
        .find(|m| m.mode_type().contains(drm::control::ModeTypeFlags::PREFERRED))
        .copied().unwrap_or_else(|| first_conn.modes()[0]);
    let (w, h) = mode.size();
    let (w, h) = (w as u32, h as u32);
    let target = (w as u16, h as u16);

    // (connector, crtc, its own mode at the shared resolution)
    let mut used: Vec<crtc::Handle> = Vec::new();
    let mut outputs: Vec<(connector::Handle, crtc::Handle, drm::control::Mode)> = Vec::new();
    for c in &conns {
        // a mode on THIS connector at the shared FB size, closest to 60 Hz — so every
        // screen runs the SAME refresh and their vblanks stay in phase (mismatched
        // 60/120 Hz screens beat against each other and halve the effective fps).
        let cmode = c.modes().iter()
            .filter(|m| m.size() == target)
            .min_by_key(|m| (m.vrefresh() as i32 - 60).unsigned_abs())
            .copied();
        let cmode = match cmode { Some(m) => m, None => { eprintln!("clone: skip a connector (no {}x{} mode)", w, h); continue } };
        // its current CRTC if free, else the next unused CRTC
        let mut cr = c.current_encoder().and_then(|e| gbm.get_encoder(e).ok()).and_then(|e| e.crtc());
        if cr.map_or(true, |h| used.contains(&h)) {
            cr = res.crtcs().iter().copied().find(|h| !used.contains(h));
        }
        let cr = match cr { Some(cr) => cr, None => { eprintln!("clone: no free CRTC left"); continue } };
        used.push(cr);
        outputs.push((c.handle(), cr, cmode));
    }
    if outputs.is_empty() { return Err(anyhow::anyhow!("no usable output")); }
    eprintln!("cloning onto {} screen(s), mode {}x{} (render {}x{})", outputs.len(), w, h, RENDER_W, RENDER_H);

    let mut surface = gbm.create_surface::<()>(w, h, Format::Xrgb8888,
        BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING)?;

    let egl = egl::Instance::new(egl::Static);
    let display = unsafe { egl.get_display(gbm.as_raw_mut() as *mut c_void) }.unwrap();
    egl.initialize(display)?;
    egl.bind_api(egl::OPENGL_ES_API)?;
    let attribs = [egl::SURFACE_TYPE, egl::WINDOW_BIT, egl::RENDERABLE_TYPE, egl::OPENGL_ES2_BIT,
        egl::RED_SIZE, 8, egl::GREEN_SIZE, 8, egl::BLUE_SIZE, 8, egl::ALPHA_SIZE, 0, egl::NONE];
    let mut configs: Vec<egl::Config> = Vec::with_capacity(64);
    egl.choose_config(display, &attribs, &mut configs)?;
    let mut config = *configs.first().ok_or_else(|| anyhow::anyhow!("no egl config"))?;
    for c in &configs {
        let vid = egl.get_config_attrib(display, *c, egl::NATIVE_VISUAL_ID).unwrap_or(0);
        if vid as u32 == 0x3432_5258 { config = *c; break; }
    }
    let ctx_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
    let context = egl.create_context(display, config, None, &ctx_attribs)?;
    let egl_surface = unsafe {
        egl.create_window_surface(display, config, surface.as_raw_mut() as egl::NativeWindowType, None)?
    };
    egl.make_current(display, Some(egl_surface), Some(egl_surface), Some(context))?;

    let gl = unsafe {
        glow::Context::from_loader_function(|s| {
            egl.get_proc_address(s).map(|p| p as *const c_void).unwrap_or(std::ptr::null())
        })
    };

    let scene_prog = compile_program(&gl, FS_SCENE);
    let comp_prog = compile_program(&gl, FS_COMP);

    // shared fullscreen quad
    unsafe {
        let vbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_cast(&quad), glow::STATIC_DRAW);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 0, 0);
        gl.enable_vertex_attrib_array(0);
    }

    // scene FBO + color texture (720p, LINEAR for the upscale)
    let (scene_fbo, scene_tex) = unsafe {
        let tex = gl.create_texture().unwrap();
        gl.active_texture(glow::TEXTURE2);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA as i32, RENDER_W, RENDER_H, 0,
            glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
        for (p, v) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR), (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE), (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] { gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32); }
        let fbo = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
        if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            panic!("scene FBO incomplete");
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        (fbo, tex)
    };

    // scene-program uniforms
    let (u_time, u_res_s, u_level, u_peak, u_bass, u_voice, u_flip) = unsafe {
        gl.use_program(Some(scene_prog));
        gl.uniform_1_i32(gl.get_uniform_location(scene_prog, "u_spec").as_ref(), 0);
        (gl.get_uniform_location(scene_prog, "u_time"),
         gl.get_uniform_location(scene_prog, "u_res"),
         gl.get_uniform_location(scene_prog, "u_level"),
         gl.get_uniform_location(scene_prog, "u_peak"),
         gl.get_uniform_location(scene_prog, "u_bass"),
         gl.get_uniform_location(scene_prog, "u_voice"),
         gl.get_uniform_location(scene_prog, "u_flip"))
    };
    // FFT bars inward by default (0.0); set FLOWENGINE_FLIP=1 for outward.
    let flip_val: f32 = if std::env::var("FLOWENGINE_FLIP").ok().as_deref() == Some("1") { 1.0 } else { 0.0 };
    // comp-program uniforms
    let u_res_c = unsafe {
        gl.use_program(Some(comp_prog));
        gl.uniform_1_i32(gl.get_uniform_location(comp_prog, "u_scene").as_ref(), 2);
        gl.uniform_1_i32(gl.get_uniform_location(comp_prog, "u_hud").as_ref(), 1);
        gl.get_uniform_location(comp_prog, "u_res")
    };

    // FFT spectrum texture (unit 0)
    const SPEC_W: i32 = 256;
    let spec_tex = unsafe {
        let t = gl.create_texture().unwrap();
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::LUMINANCE as i32, SPEC_W, 1, 0,
            glow::LUMINANCE, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
        for (p, v) in [(glow::TEXTURE_MIN_FILTER, glow::LINEAR), (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE), (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE)] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
        }
        t
    };
    // HUD text texture (unit 1)
    let hud_tex = unsafe {
        let t = gl.create_texture().unwrap();
        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::LUMINANCE as i32,
            text::HUD_W as i32, text::HUD_H as i32, 0,
            glow::LUMINANCE, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
        for (p, v) in [(glow::TEXTURE_MIN_FILTER, glow::NEAREST), (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE), (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE)] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
        }
        t
    };
    let mut hud_buf = vec![0u8; text::HUD_W * text::HUD_H];

    let start = Instant::now();
    let mut last = Instant::now();
    let mut prev_bo: Option<gbm::BufferObject<()>> = None;
    let mut prev_fb: Option<framebuffer::Handle> = None;
    let mut first = true;

    // Register for VT release/acquire signals on the active console (tty0). After this,
    // `chvt` away → the kernel sends SIGUSR1 (we drop DRM master); `chvt` back → SIGUSR2
    // (we re-grab master + re-modeset). Lets flowengine share HDMI with the desktop.
    let vt_file = OpenOptions::new().read(true).write(true).open("/dev/tty0").ok();
    let vt_fd = {
        use std::os::unix::io::AsRawFd;
        vt_file.as_ref().map(|f| f.as_raw_fd())
    };
    if let Some(fd) = vt_fd {
        unsafe {
            libc::signal(libc::SIGUSR1, vt_relsig as extern "C" fn(libc::c_int) as usize);
            libc::signal(libc::SIGUSR2, vt_acqsig as extern "C" fn(libc::c_int) as usize);
            let m = VtMode {
                mode: VT_PROCESS,
                waitv: 0,
                relsig: libc::SIGUSR1 as libc::c_short,
                acqsig: libc::SIGUSR2 as libc::c_short,
                frsig: 0,
            };
            libc::ioctl(fd, VT_SETMODE, &m as *const VtMode);
        }
        eprintln!("vt-switch: registered on active console (chvt hands off the screens)");
    }
    let mut have_master = true;

    let mut logged = Instant::now();
    let mut frames = 0u32;
    let mut cur_fps = 0.0f64;
    let mut snapbuf = vec![0u8; (w * h * 4) as usize];
    let mut snapped = Instant::now();

    loop {
        // VT switched AWAY: drop DRM master, ack the release, and idle (another owner —
        // e.g. the desktop — now drives the screens). VT switched BACK: re-grab master,
        // force a full re-modeset on both CRTCs, ack the acquire.
        if VT_RELEASE.swap(false, Ordering::SeqCst) {
            let _ = gbm.release_master_lock();
            have_master = false;
            if let Some(fd) = vt_fd { unsafe { libc::ioctl(fd, VT_RELDISP, 1); } }
            eprintln!("vt-switch: released the screens");
        }
        if VT_ACQUIRE.swap(false, Ordering::SeqCst) {
            let _ = gbm.acquire_master_lock();
            have_master = true;
            first = true; // re-run set_crtc on every connector
            if let Some(fd) = vt_fd { unsafe { libc::ioctl(fd, VT_RELDISP, VT_ACKACQ as libc::c_ulong); } }
            eprintln!("vt-switch: re-acquired the screens (re-modeset)");
        }
        if !have_master {
            std::thread::sleep(Duration::from_millis(60));
            last = Instant::now();
            continue;
        }

        let now = Instant::now();
        let t = start.elapsed().as_secs_f64();
        if duration > 0.0 && t > duration { break; }
        let dt = (now - last).as_secs_f32().clamp(0.001, 0.05);
        last = now;

        if let Some(a) = audio.as_mut() {
            let n = a.drain(&mut drain);
            if n > 0 {
                last_audio = now;
                let g = &drain[..n];
                if g.len() >= 1024 { window.copy_from_slice(&g[g.len() - 1024..]); }
                else { window.copy_within(g.len().., 0); window[1024 - g.len()..].copy_from_slice(g); }
            }
            if now.duration_since(last_audio) > Duration::from_millis(250) { window = [0.0; 1024]; }
        }
        let (level, peak, bass) = analyzer.process(&window, dt);

        frames += 1;
        let since = now.duration_since(logged).as_secs_f64();
        if since > 1.0 { cur_fps = frames as f64 / since; frames = 0; logged = now; }

        let top = format!("FLOWENGINE // NATIVE-RUST-DRM      {}X{}      FPS {:>3.0}      GPU:VIDEOCORE-VII", w, h, cur_fps);
        let sig = if analyzer.speaking { "VOICE" } else if level > 0.04 { "NOISE" } else { "QUIET" };
        let bottom = format!("LVL {:.2}   PEAK {:.2}   PEAK-HZ {:>5.0}   VOICE {:>3.0}%   BASS {:.2}   SIGNAL: {}",
            level, peak, analyzer.peak_hz, analyzer.voice_ratio * 100.0, bass, sig);
        text::render_two(&top, &bottom, &mut hud_buf);

        unsafe {
            // upload spec + hud
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(spec_tex));
            gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, SPEC_W, 1,
                glow::LUMINANCE, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(&analyzer.bytes)));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(hud_tex));
            gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, text::HUD_W as i32, text::HUD_H as i32,
                glow::LUMINANCE, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(&hud_buf)));

            // pass 1: scene -> 720p FBO
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(scene_fbo));
            gl.viewport(0, 0, RENDER_W, RENDER_H);
            gl.use_program(Some(scene_prog));
            gl.uniform_1_f32(u_time.as_ref(), (t % 3600.0) as f32);
            gl.uniform_2_f32(u_res_s.as_ref(), RENDER_W as f32, RENDER_H as f32);
            gl.uniform_1_f32(u_level.as_ref(), level);
            gl.uniform_1_f32(u_peak.as_ref(), peak);
            gl.uniform_1_f32(u_bass.as_ref(), bass);
            gl.uniform_1_f32(u_voice.as_ref(), analyzer.voice_env);
            gl.uniform_1_f32(u_flip.as_ref(), flip_val);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

            // pass 2: upscale + HUD -> default framebuffer (1080p)
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, w as i32, h as i32);
            gl.use_program(Some(comp_prog));
            gl.uniform_2_f32(u_res_c.as_ref(), w as f32, h as f32);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }

        if now.duration_since(snapped) > Duration::from_secs(3) {
            snapped = now;
            unsafe {
                gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut snapbuf)));
            }
            let wu = w as usize;
            let mut out = Vec::with_capacity(32 + wu * h as usize * 3);
            out.extend_from_slice(format!("P6\n{} {}\n255\n", w, h).as_bytes());
            for row in (0..h as usize).rev() {
                let base = row * wu * 4;
                for px in 0..wu { let o = base + px * 4; out.push(snapbuf[o]); out.push(snapbuf[o+1]); out.push(snapbuf[o+2]); }
            }
            let _ = std::fs::write("/tmp/flowsnap.ppm.tmp", &out);
            let _ = std::fs::rename("/tmp/flowsnap.ppm.tmp", "/tmp/flowsnap.ppm");
        }

        egl.swap_buffers(display, egl_surface)?;
        let bo = unsafe { surface.lock_front_buffer()? };
        let fb = gbm.add_framebuffer(&bo, 24, 32)?;
        if first {
            for (ch, cr, m) in &outputs {
                if let Err(e) = gbm.set_crtc(*cr, Some(fb), (0, 0), &[*ch], Some(*m)) {
                    eprintln!("set_crtc failed (crtc {:?}): {e}", cr);
                }
            }
            first = false;
        } else {
            // Mirror the fb onto every screen and wait for ALL their vblank events before
            // reusing buffers. (An earlier "wait for only the first-ready screen" trick
            // recycled a buffer while the other CRTC was still scanning it — which
            // flickered to black on real dual-HDMI hardware. Correct display > extra fps.)
            let mut pending = 0;
            for (_ch, cr, _m) in &outputs {
                if gbm.page_flip(*cr, fb, drm::control::PageFlipFlags::EVENT, None).is_ok() { pending += 1; }
            }
            let mut got = 0;
            while got < pending {
                let mut ev = gbm.receive_events()?;
                for _e in &mut ev { got += 1; }
            }
        }
        if let Some(old) = prev_fb.take() { let _ = gbm.destroy_framebuffer(old); }
        prev_fb = Some(fb);
        prev_bo = Some(bo);
        let _ = &prev_bo;
        let _ = scene_tex;
    }
    Ok(())
}

fn bytemuck_cast(f: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(f.as_ptr() as *const u8, std::mem::size_of_val(f)) }
}
