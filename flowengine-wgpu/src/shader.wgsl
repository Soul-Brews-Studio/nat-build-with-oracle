// flowengine — "Starry Night porthole": a swirling Van Gogh sky + FFT voiceprint,
// cropped to a circular window (black outside), with a glowing gold rim.
// ---------------------------------------------------------------------------
// wgpu 25 (Metal/Vulkan/DX12) + winit 0.30. Bind group 0:
//   @binding(0) var<uniform> U   -> 32-byte Uniforms (time,level,peak,bass,res,voice)
//   @binding(1) var spec_tex     -> texture_2d<f32>, 256x1 R8Unorm (FFT, per frame)
//   @binding(2) var spec_samp    -> filtering sampler, clamp-to-edge
// Fullscreen triangle, no vertex buffers. NON-sRGB (Bgra8Unorm) target.
// Metal-safe: squares written x*x; pow() only on non-negative bases.
// ---------------------------------------------------------------------------

const TAU: f32 = 6.28318530718;
const RAD: f32 = 0.40;   // porthole radius (uv units; full circle with margin)

struct Uniforms {
    time:  f32,        // 0
    level: f32,        // 4
    peak:  f32,        // 8
    bass:  f32,        // 12
    res:   vec2<f32>,  // 16
    voice: f32,        // 24
    flip:  f32,        // 28  0 = bars inward, 1 = bars outward
};

@group(0) @binding(0) var<uniform> U: Uniforms;
@group(0) @binding(1) var spec_tex: texture_2d<f32>;
@group(0) @binding(2) var spec_samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    var out: VsOut;
    let x = f32(i32(vid & 1u) * 4 - 1);
    let y = f32(i32(vid >> 1u) * 4 - 1);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = U.res;
    let frag = vec2<f32>(in.pos.x, res.y - in.pos.y);   // flip y
    let uv: vec2<f32> = (frag - 0.5 * res) / res.y;
    let r: f32 = length(uv);
    let t: f32 = U.time;

    let lv: f32    = clamp(U.level, 0.0, 1.0);
    let voice: f32 = U.voice;
    let drive: f32 = max(lv, U.bass);
    let dc: f32    = drive / (0.6 + drive);

    // Van Gogh "Starry Night" palette
    let deep  = vec3<f32>(0.02, 0.05, 0.15);
    let ultra = vec3<f32>(0.08, 0.28, 0.68);
    let cyan  = vec3<f32>(0.32, 0.72, 1.0);
    let gold  = vec3<f32>(1.0,  0.82, 0.26);

    // --- swirling flow field: domain-warp uv into eddies (flows, never strobes) ---
    var p = uv;
    let turb: f32 = 0.26 + 0.50 * voice + 0.12 * lv;
    for (var k: i32 = 0; k < 3; k = k + 1) {
        let fk: f32 = 2.2 + f32(k) * 2.3;
        p += turb * 0.14 * vec2<f32>(
            sin(fk * p.y + t * 0.55 + f32(k) * 1.7),
            cos(fk * p.x - t * 0.48 + f32(k) * 1.1)
        );
    }
    let rw:  f32 = length(p);
    let sang: f32 = atan2(p.y, p.x);

    // --- painterly brushstrokes following the swirl ---
    let stroke: f32 = 0.5 + 0.5 * sin(rw * 30.0 - t * 1.1 + 5.0 * sin(sang * 2.0 + t * 0.2));
    let paint:  f32 = 0.5 + 0.5 * sin(p.x * 20.0 + 9.0 * p.y + t * 0.15);
    let tex:    f32 = mix(stroke, paint, 0.4);

    // --- night-sky body: deep -> ultramarine, cyan crests, gold flecks ---
    var col = deep;
    let body: f32 = tex * 0.9 + 0.14;
    col = mix(col, ultra, clamp(body, 0.0, 1.0));
    let crest: f32 = tex * tex * tex * tex;
    col += cyan * crest * (0.35 + 0.6 * lv);
    col += gold * smoothstep(0.78, 0.98, tex) * (0.22 + 0.7 * voice);

    // --- central golden STAR + slow radiant rays ---
    let breath: f32 = 0.5 + 0.5 * sin(t * 0.6);
    let core: f32   = exp(-r * r * (26.0 - 10.0 * dc));
    col += mix(cyan, gold, 0.35 + 0.5 * dc) * core * (0.30 + 0.9 * dc);
    col += gold * exp(-r * r * 90.0) * (0.40 + 0.8 * dc);
    let rays: f32 = 0.5 + 0.5 * sin(sang * 6.0 + t * 0.3);
    col += gold * rays * exp(-r * r * 12.0) * (0.06 + 0.10 * breath + 0.20 * voice);

    // --- FFT voiceprint ring: golden star-bursts (unwarped angle -> stable bins) ---
    let a0: f32  = atan2(uv.y, uv.x);
    let aa: f32  = fract(a0 / TAU + 0.75);
    let bin: f32 = aa * aa;
    var mag: f32 = textureSample(spec_tex, spec_samp, vec2<f32>(clamp(bin, 0.003, 0.997), 0.5)).r;
    mag = mag * mag;
    let slot: f32    = fract((a0 / TAU) * 96.0);
    let barMask: f32 = smoothstep(0.60, 0.28, abs(slot - 0.5) * 2.0);
    // bars hang from the golden rim; Space toggles inward (flip 0) / outward (flip 1)
    let barLen: f32  = 0.20;
    let inner: f32   = RAD - mag * barLen;
    let outb:  f32   = RAD + mag * barLen;
    let inA:   f32   = step(inner, r) * step(r, RAD);
    let outA:  f32   = step(RAD, r) * step(r, outb);
    let inbar: f32   = mix(inA, outA, U.flip) * barMask;
    let tipR:  f32   = mix(inner, outb, U.flip);
    let barGate: f32 = 0.18 + 1.05 * voice;
    var fbar = vec3<f32>(0.0);
    fbar += mix(gold, cyan, mag) * inbar * (0.40 + 0.8 * mag) * barGate;
    let dtip: f32 = (r - tipR) * 42.0;
    fbar += gold * exp(-dtip * dtip) * barMask * mag * barGate;

    // --- peak-hold bloom (white-gold flush on a strong peak) ---
    let hold: f32 = U.peak * U.peak;
    col += mix(vec3<f32>(1.0, 1.0, 1.0), gold, 0.5) * exp(-r * r * 5.0) * hold * 0.6;

    // soft inner vignette + tonemap + gamma on the sky
    col *= 0.55 + 0.45 * smoothstep(RAD, 0.05, r);
    col = col / (1.0 + col);
    col = pow(col, vec3<f32>(0.85));

    // TWO ZONES: inside vivid; outside dimmed to a faint filtered backdrop (not black)
    let disc: f32 = 1.0 - smoothstep(RAD - 0.02, RAD + 0.08, r);
    col *= mix(0.15, 1.0, disc);

    // FFT bars + gold rim ON TOP (full brightness, so OUTWARD bars aren't dimmed away)
    col += fbar / (1.0 + fbar);

    // LIVING gold rim: it breathes, a shimmer of light courses around it, and a bright
    // glint slowly travels the ring — plus it swells when you speak.
    let breathR: f32 = RAD + 0.006 * sin(t * 1.3);              // gentle radius breathing
    let drim: f32    = (r - breathR) * 55.0;
    let ring: f32    = exp(-drim * drim);
    let flow: f32    = 0.70 + 0.30 * sin(a0 * 5.0 - t * 1.6) * (0.6 + 0.4 * sin(a0 * 2.0 + t * 0.7));
    let rimI: f32    = 0.40 + 0.10 * sin(t * 1.3) + 0.55 * voice;
    col += gold * ring * flow * rimI;
    let glint: f32   = pow(max(0.5 + 0.5 * sin(a0 - t * 0.9), 0.0), 8.0);   // travelling glint
    col += vec3<f32>(1.0, 0.94, 0.65) * ring * glint * (0.5 + 0.6 * voice);

    col = clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(col, 1.0);
}
