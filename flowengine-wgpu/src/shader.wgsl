// flowengine — scene shader, WGSL port of FS_SCENE (GLSL-ES 1.0).
// ---------------------------------------------------------------------------
// Target stack (all Rust-side code MUST match these exact versions):
//     wgpu   = "22.1"      (Metal on macOS / Apple Silicon, Vulkan on Linux)
//     winit  = "0.30"
//     bytemuck, image, cpal, realfft on their current releases.
//
// This file is version-independent WGSL (stable since wgpu 0.14), but the
// BIND GROUP LAYOUT it assumes is fixed and the Rust host must create exactly:
//
//   bind group 0:
//     @binding(0)  var<uniform> U       -> uniform buffer, 32 bytes (see Uniforms)
//     @binding(1)  var spec_tex         -> texture_2d<f32>, 256x1 R8Unorm,
//                                          written each frame via queue.write_texture
//     @binding(2)  var spec_samp        -> sampler (filtering, clamp-to-edge)
//
// Pipeline: no vertex buffers. Draw a single 3-vertex fullscreen triangle
// (render_pass.draw(0..3, 0..1)); the vertex stage synthesises clip positions
// from @builtin(vertex_index). The fragment stage reconstructs pixel position
// from @builtin(position) (WGSL's replacement for gl_FragCoord) and does the
// whole radial effect from there. Surface format is assumed non-sRGB-view
// linear write of an already gamma-corrected colour (pow(col,0.85) below), i.e.
// the pipeline's fragment target should be the Bgra8Unorm (NOT _srgb) view so
// the tonemap/gamma matches the original GLES output 1:1. If you configure an
// _srgb target instead, drop the final pow() to avoid double gamma.
// ---------------------------------------------------------------------------

const TAU: f32 = 6.28318530718;

// -- Uniforms -----------------------------------------------------------------
// Matches this #[repr(C)] Rust struct (bytemuck Pod/Zeroable), 32 bytes total:
//
//   #[repr(C)]
//   #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
//   struct Uniforms {
//       time:  f32,        // offset  0   u_time
//       level: f32,        // offset  4   u_level (RMS, 0..1)
//       peak:  f32,        // offset  8   u_peak
//       bass:  f32,        // offset 12   u_bass
//       res:   [f32; 2],   // offset 16   u_res (framebuffer px, w,h)
//       _pad:  [f32; 2],   // offset 24   padding -> 32 bytes (16-byte aligned)
//   }
//
// WGSL: vec2<f32> has align 8; placing `res` at offset 16 satisfies it, and the
// trailing _pad rounds the struct up to a 16-byte multiple as UBOs require.
struct Uniforms {
    time:  f32,
    level: f32,
    peak:  f32,
    bass:  f32,
    res:   vec2<f32>,
    _pad:  vec2<f32>,
};

@group(0) @binding(0) var<uniform> U: Uniforms;
@group(0) @binding(1) var spec_tex: texture_2d<f32>;
@group(0) @binding(2) var spec_samp: sampler;

// -- Vertex: fullscreen triangle ---------------------------------------------
// Three verts, no vertex buffer. The triangle (-1,-1)(3,-1)(-1,3) fully covers
// the [-1,1] clip square; the surplus is clipped. @builtin(position) in the
// fragment stage then carries pixel coords (origin top-left, y down in WGSL).
struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    var out: VsOut;
    // x: -1, 3, -1   y: -1, -1, 3
    let x = f32(i32(vid & 1u) * 4 - 1);   // vid=0->-1, vid=1->3, vid=2->-1
    let y = f32(i32(vid >> 1u) * 4 - 1);  // vid=0->-1, vid=1->-1, vid=2->3
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    return out;
}

fn hash21(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(123.34, 345.45));
    p = p + dot(p, p + 34.345);
    return fract(p.x * p.y);
}

// -- Fragment: the alien-Jarvis radial scene (NO rotation) --------------------
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = U.res;

    // GLES gl_FragCoord has origin at bottom-left (y up); WGSL @builtin(position)
    // is top-left (y down). Flip y so the visual (and the FFT ring's angular
    // ordering via atan2) matches the original 1:1.
    let frag = vec2<f32>(in.pos.x, res.y - in.pos.y);

    // vec2 uv=(gl_FragCoord.xy-0.5*u_res)/u_res.y;
    let uv: vec2<f32> = (frag - 0.5 * res) / res.y;
    let r: f32   = length(uv);
    let ang: f32 = atan2(uv.y, uv.x);   // WGSL two-arg atan2(y, x)
    let t: f32   = U.time;

    let lv: f32    = clamp(U.level, 0.0, 1.0);
    let drive: f32 = max(lv, U.bass);
    let dc: f32    = drive / (0.6 + drive);

    let cyan  = vec3<f32>(0.26, 0.76, 1.0);
    let pale  = vec3<f32>(0.80, 0.94, 1.0);
    let amber = vec3<f32>(1.0,  0.70, 0.28);
    let alien = vec3<f32>(0.30, 1.0,  0.75);
    var col   = vec3<f32>(0.0);

    // dark near-black-blue base, fading out with radius
    col += vec3<f32>(0.015, 0.035, 0.075) * (1.0 - smoothstep(0.0, 1.3, r));

    // concentric energy waves radiating OUTWARD  (sin(r*26 - t*3.5), squared)
    let ws: f32 = max(sin(r * 26.0 - t * 3.5), 0.0);
    col += mix(cyan, alien, 0.25) * ws * ws
         * smoothstep(0.08, 0.35, r) * smoothstep(1.2, 0.5, r)
         * (0.10 + 0.5 * lv);

    // two sonar pulses expanding to the edges, fading as they go
    for (var i: i32 = 0; i < 2; i = i + 1) {
        let ph: f32  = fract(t * 0.20 + f32(i) * 0.5);
        let pr: f32  = ph * 1.45;
        let son: f32 = exp(-pow((r - pr) * 11.0, 2.0)) * (1.0 - ph);
        col += cyan * son * (0.20 + 0.5 * dc);
    }

    // compressed cyan core (never blows to flat white) + hot point + corona ring
    let breath: f32 = 0.5 + 0.5 * sin(t * 0.7);
    let coreK: f32  = 30.0 - 11.0 * dc;
    let core: f32   = exp(-r * r * coreK);
    let coreI: f32  = 0.26 + 0.10 * breath + 0.85 * dc;
    col += mix(cyan, pale, clamp(0.25 + 0.45 * dc, 0.0, 0.85)) * core * coreI;
    col += pale * exp(-r * r * 460.0) * (0.30 + 0.6 * dc);        // small hot point
    let ringR: f32 = 0.145 + 0.02 * breath;
    col += cyan * exp(-pow((r - ringR) * 28.0, 2.0)) * (0.10 + 0.9 * lv); // corona

    // log-frequency FFT ring, sampled from the 256x1 spectrum texture by angle
    let aa: f32  = fract(ang / TAU + 0.75);
    let bin: f32 = pow(aa, 2.0);
    var mag: f32 = textureSample(spec_tex, spec_samp,
                                 vec2<f32>(clamp(bin, 0.003, 0.997), 0.5)).r;
    mag = mag * mag;
    let baseR: f32   = 0.26;
    let barMax: f32  = 0.18;
    let bars: f32    = 108.0;
    let slot: f32    = fract((ang / TAU) * bars);
    let barMask: f32 = smoothstep(0.60, 0.28, abs(slot - 0.5) * 2.0);
    let outer: f32   = baseR + mag * barMax;
    let inbar: f32   = step(baseR, r) * step(r, outer) * barMask;
    col += cyan * inbar * (0.30 + 0.7 * mag);
    col += pale * exp(-pow((r - outer) * 46.0, 2.0)) * barMask * mag * 0.7;
    col += cyan * exp(-pow((r - baseR) * 120.0, 2.0)) * 0.18;

    // static concentric rings + FFT-DRIVEN radial pulse emission (energy=spectrum)
    let o1: f32 = exp(-pow((r - 0.60) * 70.0, 2.0));
    col += cyan * o1 * (0.10 + 0.06 * sin(t * 1.5)) * (0.6 + 0.6 * lv);
    let o2: f32 = exp(-pow((r - 0.80) * 60.0, 2.0));
    col += mix(cyan, alien, 0.5) * o2 * (0.08 + 0.05 * sin(t * 1.1 + 1.0));
    let phM: f32    = fract(r * 1.8 - t * 1.3);
    let pulseM: f32 = smoothstep(0.0, 0.12, phM) * smoothstep(0.55, 0.12, phM);
    let sp: f32     = 0.5 + 0.5 * sin(ang * 80.0);   // sin(ang*80) shaped, cubed below
    col += mix(cyan, pale, 0.3) * pulseM * (sp * sp * sp) * mag
         * smoothstep(0.16, 0.4, r) * smoothstep(1.15, 0.5, r) * 2.4;

    // amber flare on peaks
    col += amber * core  * smoothstep(0.6,  1.0, U.peak) * 1.1;
    col += amber * inbar * smoothstep(0.75, 1.0, U.peak) * 0.5;

    // alien-teal edge shift + vignette + Reinhard tonemap + gamma
    col = mix(col, col * vec3<f32>(0.7, 1.15, 1.0), smoothstep(0.55, 1.15, r) * 0.5);
    col *= 0.4 + 0.6 * smoothstep(1.3, 0.12, r);        // vignette
    col = col / (1.0 + col);                            // Reinhard
    col = pow(col, vec3<f32>(0.85));                    // gamma

    return vec4<f32>(col, 1.0);
}
