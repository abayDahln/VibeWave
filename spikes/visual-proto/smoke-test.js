// Headless smoke test for the VibeWave visual prototype variants.
// Stubs a minimal Canvas2D context, loads the variant code, and calls every
// variant's draw() across a range of band values to catch runtime errors.
const fs = require("fs");
const path = require("path");

const html = fs.readFileSync(
  path.join(__dirname, "index.html"), "utf8");
const script = html.match(/<script>([\s\S]*)<\/script>/)[1];

// --- minimal DOM + Canvas stubs ---
function ctxStub() {
  const grad = { addColorStop() {} };
  return new Proxy({}, {
    get(t, k) {
      if (k === "createLinearGradient" || k === "createRadialGradient") return () => grad;
      if (k === "canvas") return { width: 400, height: 300 };
      return () => {};
    },
    set() { return true; },
  });
}
const elStub = () => {
  const t = {
    style: {}, classList: { add(){}, remove(){}, toggle(){}, contains(){return false;} },
    append(){}, appendChild(){}, addEventListener(){}, querySelectorAll(){return [];},
    getContext: ctxStub, getBoundingClientRect(){ return {width:400,height:300}; },
    onclick: null, oninput: null, textContent: "", innerHTML: "", value: "24", files: [],
    width: 400, height: 300,
  };
  t.parentElement = t;
  return t;
};

global.document = {
  getElementById: elStub,
  createElement: elStub,
};
global.window = { addEventListener(){}, AudioContext: function(){
  this.sampleRate=48000; this.createAnalyser=()=>({fftSize:2048,frequencyBinCount:1024,
    smoothingTimeConstant:0,getByteFrequencyData(){},getByteTimeDomainData(){}});
  this.createBufferSource=()=>({connect(){},start(){},stop(){},disconnect(){}});
  this.destination={}; this.decodeAudioData=async()=>({duration:100});
}};
global.performance = { now: () => Date.now() };
global.requestAnimationFrame = () => 0;
global.Math.random = Math.random;

// Run the script and expose the variants it defined.
const wrapped = script + "\n;globalThis.__V = VARIANTS; globalThis.__CFG = CFG;";
try {
  eval(wrapped);
} catch (e) {
  console.error("LOAD FAILED:", e.message);
  process.exit(1);
}

const V = globalThis.__V;
const CFG = globalThis.__CFG;
if (!V || !V.length) { console.error("no variants found"); process.exit(1); }

let fail = 0;
for (const v of V) {
  const ctx = ctxStub();
  const S = { sparks: [], streaks: [] };
  // sweep bands + time so every branch runs
  for (const b of [0, 0.25, 0.6, 1]) {
    for (const t of [0, 0.5, 3.7, 12.3]) {
      CFG.t = t;
      CFG.thickness = 24;
      try {
        v.draw(ctx, 400, 300, 1/60, { bass:b, mid:b, treble:b }, S);
      } catch (e) {
        console.error(`FAIL ${v.id} @band=${b} t=${t}: ${e.message}`);
        fail++;
        break;
      }
    }
  }
}
console.log(`variants: ${V.length}`);
console.log("cats:", [...new Set(V.map(v => v.cat))].join(", "));

// --- bars-frame contract: geometry, bounds, collapse/fill, time-invariance ---
const rects = [];
const spy = new Proxy({}, {
  get(t, k) {
    if (k === "createLinearGradient" || k === "createRadialGradient") return () => ({ addColorStop() {} });
    if (k === "canvas") return { width: 400, height: 300 };
    if (k === "fillRect") return (x, y, w, h) => rects.push({ x, y, w, h });
    return () => {};
  },
  set() { return true; },
});
const bars = V.find(v => v.id === "bars-frame");
if (!bars) { console.error("FAIL: bars-frame variant missing"); fail++; }
else {
  for (const [t, b, W, H, want] of [
    [50, 0, 400, 300, "collapse"],
    [50, 1, 400, 300, "fill"],
    [4, 0.5, 64, 48, "tiny"],
    [2, 0.7, 4096, 2160, "huge"],
  ]) {
    rects.length = 0;
    CFG.t = t; CFG.thickness = 4; CFG.centerProfile = 0;   // isolate cap/floor math
    // flat spectrum at level b -> deterministic, no center-weighting surprises
    const flat = new Array(64).fill(b);
    bars.draw(spy, W, H, 1 / 60, { bass: b, mid: b, treble: b }, { spectrum: flat });
    const n = rects.length;
    const maxExtent = Math.max(0, ...rects.map(r => Math.max(r.w, r.h)));
    // bar LENGTH (not thickness): top bars are the tall thin ones (w = thickness)
    const maxLenDrawn = Math.max(0, ...rects.filter(r => r.w <= CFG.thickness + 0.5).map(r => r.h));
    const inBounds = rects.every(r =>
      r.x >= -1 && r.y >= -1 && r.x + r.w <= W + 1 && r.y + r.h <= H + 1);
    // count sanity: tiny (>0) for any non-zero level; bounded always
    if (n > 4000) { console.error(`FAIL bars-frame ${want}: rect count ${n}`); fail++; }
    if (n < 1) { console.error(`FAIL bars-frame ${want}: no rects`); fail++; }
    if (!inBounds) { console.error(`FAIL bars-frame ${want}: rect out of bounds`); fail++; }
    // collapse: with a floor (A3) bars shrink to the floor stub, not to zero.
    // The bar's drawn extent must sit AT the floor (within rounding); a bar
    // cannot grow past it. Thickness may exceed the floor on very small canvases,
    // so the bound is max(floor, thickness).
    if (want === "collapse") {
      const floorPx = CFG.floorFrac * Math.min(W, H);
      const bound = Math.max(floorPx, CFG.thickness);
      if (maxExtent > bound + 1.5) { console.error(`FAIL bars-frame collapse: extent ${maxExtent} > bound ${bound}`); fail++; }
      if (maxExtent < Math.min(floorPx, CFG.thickness) - 1.5) { console.error(`FAIL bars-frame collapse: extent ${maxExtent} too small (stub vanished)`); fail++; }
    }
    // fill: at level 1 the tallest bar reaches floor + maxLen, within rounding.
    if (want === "fill") {
      const expected = CFG.floorFrac * Math.min(W, H) + 0.28 * Math.min(W, H);
      if (Math.abs(maxLenDrawn - expected) > 1.5) { console.error(`FAIL bars-frame fill: len ${maxLenDrawn} != floor+maxLen ${expected}`); fail++; }
    }
  }
  // time-invariance: same bands, different CFG.t -> identical rects
  const snap = t => {
    rects.length = 0; CFG.t = t; CFG.thickness = 4;
    const flat = new Array(64).fill(.6);
    bars.draw(spy, 400, 300, 1 / 60, { bass: .6, mid: .6, treble: .6 }, { spectrum: flat });
    return JSON.stringify(rects);
  };
  if (snap(0) !== snap(9.7)) { console.error("FAIL bars-frame: output depends on time"); fail++; }
}

// --- A3: floor + cap, driven through the REAL variant draw() so barOpts's
// maxLen/floorFrac wiring is exercised (not re-derived locally). Bars must
// never cross the far edge; at full level the tallest bar on each edge stops
// short of that edge's cap. Run at non-square aspect ratios both ways. ---
if (V.find(v => v.id === "bars-frame")) {
  const barV = V.find(v => v.id === "bars-frame");
  for (const [W, H] of [[400, 300], [7680, 1080], [1080, 7680]]) {
    for (const level of [0.5, 1]) {
      rects.length = 0;
      CFG.t = 0; CFG.thickness = 4; CFG.centerProfile = 0;
      const flat = new Array(64).fill(level);
      barV.draw(spy, W, H, 1 / 60, { bass: level, mid: level, treble: level }, { spectrum: flat });
      const topBars = rects.filter(r => r.y <= 1 && r.w <= 4.5);              // grow down from y=0
      const bottomBars = rects.filter(r => r.y + r.h >= H - 1 && r.w <= 4.5); // grow up to y=H
      const leftBars = rects.filter(r => r.x <= 1 && r.h <= 4.5);             // grow right from x=0
      const rightBars = rects.filter(r => r.x + r.w >= W - 1 && r.h <= 4.5);  // grow left to x=W
      // floor and maxLen both derive from the SHORTEST screen dimension.
      const capAll = CFG.floorFrac * Math.min(W, H) + 0.28 * Math.min(W, H);
      const maxDown = Math.max(0, ...topBars.map(r => r.y + r.h));
      const minUp = Math.min(H, ...bottomBars.map(r => r.y));
      const maxRight = Math.max(0, ...leftBars.map(r => r.x + r.w));
      const minFromRight = Math.min(W, ...rightBars.map(r => r.x));
      const tag = `${W}x${H}@${level}`;
      if (maxDown > capAll + 1) { console.error(`FAIL floor ${tag}: top bar reached ${maxDown} (cap ${capAll})`); fail++; }
      if (minUp < H - capAll - 1) { console.error(`FAIL floor ${tag}: bottom bar reached ${minUp}`); fail++; }
      if (maxRight > capAll + 1) { console.error(`FAIL floor ${tag}: left bar reached ${maxRight} (cap ${capAll})`); fail++; }
      if (minFromRight < W - capAll - 1) { console.error(`FAIL floor ${tag}: right bar reached ${minFromRight}`); fail++; }
    }
  }
}

// --- negative floor (A): bars overhang OUTWARD past the edge. The bar's inner
// tip still reaches exactly floor + grow from the edge, and its outer end sits
// at -|floor| (outside the canvas). The far-edge cap still holds. ---
if (V.find(v => v.id === "bars-frame")) {
  const barV = V.find(v => v.id === "bars-frame");
  const W = 400, H = 300, level = 1;
  CFG.t = 0; CFG.thickness = 4; CFG.floorFrac = -0.10; CFG.centerProfile = 0;
  rects.length = 0;
  const flat = new Array(64).fill(level);
  barV.draw(spy, W, H, 1 / 60, { bass: level, mid: level, treble: level }, { spectrum: flat });
  const topBars = rects.filter(r => r.w <= 4.5 && r.y < 0);          // overhanging top bars
  if (topBars.length < 1) { console.error("FAIL negative floor: no top bar overhangs the edge"); fail++; }
  else {
    const minY = Math.min(...topBars.map(r => r.y));
    // outer end should be about -|floor| = -(0.10 * min(W,H)) = -30
    if (Math.abs(minY - (-0.10 * Math.min(W, H))) > 1.5) {
      console.error(`FAIL negative floor: outer end ${minY}, expected ${-0.10 * Math.min(W, H)}`); fail++;
    }
    // every top bar must span from the overhang end inward. At a negative floor
    // only bars with grow > |floor| reach into the visible area; the shortest
    // ones legitimately stay wholly outside. So assert each starts at the
    // overhang end, and that most bars poke into the canvas.
    const overhang = -0.10 * Math.min(W, H);
    if (!topBars.every(r => Math.abs(r.y - overhang) < 1.5)) {
      console.error("FAIL negative floor: a top bar does not start at the overhang end"); fail++;
    }
    const visible = topBars.filter(r => r.y + r.h > 0).length;
    if (visible < topBars.length * 0.5) {
      console.error(`FAIL negative floor: only ${visible}/${topBars.length} bars visible`); fail++;
    }
    // inner tip must still equal floor + grow (capped) exactly as before
    const expectedInner = CFG.floorFrac * Math.min(W, H) + 0.28 * Math.min(W, H);
    const maxInner = Math.max(...topBars.map(r => r.y + r.h));
    if (Math.abs(maxInner - expectedInner) > 2) {
      console.error(`FAIL negative floor: inner tip ${maxInner}, expected ${expectedInner}`); fail++;
    }
  }
  CFG.floorFrac = 0.12; // restore for any later checks
}

// --- B2: per-bar randomness. Two bars on the same edge at the same level must
// have different lengths (not a uniform block). ---
if (typeof globalThis.barShape === "function") {
  const lens = new Set();
  for (let i = 0; i < 40; i++) lens.add(globalThis.barShape(i, 11).toFixed(4));
  if (lens.size < 10) { console.error(`FAIL barShape: only ${lens.size} distinct values (not random enough)`); fail++; }
  // still normalized to (0,1]
  for (let i = 0; i < 40; i++) {
    const v = globalThis.barShape(i, 11);
    if (!(v > 0 && v <= 1)) { console.error(`FAIL barShape: out of range ${v}`); fail++; break; }
  }
}

// --- edge-set variants: top/bottom-only and left/right-only must exist ---
for (const id of ["bars-frame", "bars-horizontal", "bars-vertical"]) {
  const v = V.find(x => x.id === id);
  if (!v) { console.error(`FAIL: variant ${id} missing`); fail++; continue; }
  // square-ish plus both extreme aspect ratios
  for (const [W, H] of [[400, 300], [7680, 1080], [1080, 7680]]) {
    rects.length = 0;
    CFG.t = 0; CFG.thickness = 4;
    v.draw(spy, W, H, 1 / 60, { bass: .8, mid: .8, treble: .8 }, { spectrum: new Array(64).fill(0.8) });
    if (rects.length < 1) { console.error(`FAIL ${id} ${W}x${H}: drew no rects`); fail++; }
    const inb = rects.every(r => r.x >= -1 && r.y >= -1 && r.x + r.w <= W + 1 && r.y + r.h <= H + 1);
    if (!inb) { console.error(`FAIL ${id} ${W}x${H}: rect out of bounds`); fail++; }
  }
}
// horizontal-only must touch top/bottom edges and not left/right; and vice-versa
const hOnly = V.find(x => x.id === "bars-horizontal");
const vOnly = V.find(x => x.id === "bars-vertical");
if (hOnly) {
  rects.length = 0; CFG.t = 0; CFG.thickness = 4;
  hOnly.draw(spy, 400, 300, 1 / 60, { bass: 1, mid: 1, treble: 1 }, { spectrum: new Array(64).fill(1) });
  const touchesTB = rects.some(r => r.y <= 1) && rects.some(r => r.y + r.h >= 299);
  const touchesLR = rects.some(r => r.x <= 1 && r.h <= 4.5) || rects.some(r => r.x + r.w >= 399 && r.h <= 4.5);
  if (!touchesTB) { console.error("FAIL bars-horizontal: does not touch top+bottom edges"); fail++; }
  if (touchesLR) { console.error("FAIL bars-horizontal: drew left/right bars"); fail++; }
}
if (vOnly) {
  rects.length = 0; CFG.t = 0; CFG.thickness = 4;
  vOnly.draw(spy, 400, 300, 1 / 60, { bass: 1, mid: 1, treble: 1 }, { spectrum: new Array(64).fill(1) });
  const touchesLR = rects.some(r => r.x <= 1) && rects.some(r => r.x + r.w >= 399);
  const touchesTB = rects.some(r => r.y <= 1 && r.w <= 4.5) || rects.some(r => r.y + r.h >= 299 && r.w <= 4.5);
  if (!touchesLR) { console.error("FAIL bars-vertical: does not touch left+right edges"); fail++; }
  if (touchesTB) { console.error("FAIL bars-vertical: drew top/bottom bars"); fail++; }
}

// --- parseHex contract: invalid hex rejected, valid parsed ---
if (typeof globalThis.floorFromSlider !== "function") {
  console.error("FAIL: floorFromSlider is not defined"); fail++;
} else {
  // slider 0-100 (%) maps to floorFrac -0.10 .. 0.35
  const cases = [[0, -0.10], [100, 0.35], [50, 0.125], [32, 0.044]];
  for (const [v, expected] of cases) {
    const got = globalThis.floorFromSlider(v);
    if (Math.abs(got - expected) > 0.002) {
      console.error(`FAIL floorFromSlider(${v}) = ${got}, expected ${expected}`); fail++;
    }
  }
  // label formatter shows whole percent
  if (typeof globalThis.floorLabel !== "function") {
    console.error("FAIL: floorLabel is not defined"); fail++;
  } else if (globalThis.floorLabel(0) !== "0%" || globalThis.floorLabel(100) !== "100%") {
    console.error(`FAIL floorLabel: got "${globalThis.floorLabel(0)}"/"${globalThis.floorLabel(100)}"`); fail++;
  }
}
if (typeof globalThis.parseHex !== "function") { console.error("FAIL: parseHex is not defined"); fail++; }
else {
  if (globalThis.parseHex("#zzzzzz") !== null) { console.error("FAIL parseHex: invalid hex accepted"); fail++; }
  const ok = globalThis.parseHex("#3a7bd5");
  if (!ok || ok.r !== 0x3a || ok.g !== 0x7b || ok.b !== 0xd5) { console.error("FAIL parseHex: valid hex mis-parsed"); fail++; }
}

// --- setColor contract: invalid hex leaves CFG.color unchanged (no NaN paint) ---
if (typeof globalThis.setColor === "function" && globalThis.__CFG) {
  const before = globalThis.__CFG.color;
  globalThis.setColor("not-a-color", false);
  if (globalThis.__CFG.color !== before) {
    console.error(`FAIL setColor: invalid hex changed color ${before} -> ${globalThis.__CFG.color}`); fail++;
  }
  globalThis.setColor("#3a7bd5", false);
  if (globalThis.__CFG.color !== "#3a7bd5") {
    console.error(`FAIL setColor: valid hex not applied (got ${globalThis.__CFG.color})`); fail++;
  }
}

// --- A1: per-bar spectrum must drive bars INDEPENDENTLY. A spectrum with one
// tall bin and the rest zero must make exactly some bars taller than others,
// not a uniform block. ---
if (typeof globalThis.drawBarFrame === "function") {
  const W = 400, H = 300, n = 32;
  const spec = new Array(n).fill(0);
  spec[5] = 1; spec[6] = 1;                       // one tall region
  rects.length = 0;
  CFG.t = 0; CFG.thickness = 3; CFG.floorFrac = 0.1;
  globalThis.drawBarFrame(spy, W, H, spec, {
    thickness: 3, maxLen: 0.28 * Math.min(W, H), color: "#fff",
    floorFrac: 0.1, edges: ["top"],
  });
  const hs = rects.filter(r => r.w <= 3.5).map(r => r.h);
  const tallest = Math.max(...hs), shortest = Math.min(...hs);
  if (!(tallest > shortest + 5)) {
    console.error(`FAIL spectrum: uniform block (tallest ${tallest}, shortest ${shortest})`); fail++;
  }
}

// --- center weighting: with a FLAT spectrum, bars near the MIDDLE of an edge
// must be taller than bars near the ENDS. ---
if (typeof globalThis.centerWeight !== "function") {
  console.error("FAIL: centerWeight is not defined"); fail++;
} else {
  const wEdge = globalThis.centerWeight(0.0, 0.6);   // at the end
  const wMid  = globalThis.centerWeight(0.5, 0.6);   // at the middle
  if (!(wMid > wEdge + 0.1)) { console.error(`FAIL centerWeight: mid ${wMid} not > end ${wEdge}`); fail++; }
  if (Math.abs(wMid - 1) > 0.01) { console.error(`FAIL centerWeight: mid should be 1, got ${wMid}`); fail++; }
  if (globalThis.centerWeight(0, 0) < 0.99) { console.error("FAIL centerWeight: profile 0 should be flat"); fail++; }
}
if (typeof globalThis.drawBarFrame === "function" && typeof globalThis.centerWeight === "function") {
  const W = 400, H = 300, n = 64;
  const spec = new Array(n).fill(0.8);              // FLAT spectrum
  const drawTop = profile => {
    rects.length = 0;
    CFG.t = 0; CFG.thickness = 3; CFG.floorFrac = 0.1;
    globalThis.drawBarFrame(spy, W, H, spec, {
      thickness: 3, maxLen: 0.28 * Math.min(W, H), color: "#fff",
      floorFrac: 0.1, edges: ["top"], centerProfile: profile,
    });
    return rects.filter(r => r.w <= 3.5).sort((a, b) => a.x - b.x);
  };
  // Average heights over a MIDDLE band vs the two END bands, so the per-bar
  // barShape() randomness averages out and only the weighting trend remains.
  const bandAvg = (bars, lo, hi) => {
    const sel = bars.filter((_, i) => {
      const t = bars.length > 1 ? i / (bars.length - 1) : 0.5;
      return t >= lo && t < hi;
    });
    return sel.reduce((s, r) => s + r.h, 0) / Math.max(1, sel.length);
  };
  const withW = drawTop(0.6);
  const mid = bandAvg(withW, 0.4, 0.6), ends = (bandAvg(withW, 0, 0.1) + bandAvg(withW, 0.9, 1)) / 2;
  if (!(mid > ends + 5)) {
    console.error(`FAIL center weighting drawn: mid ${mid.toFixed(1)} not > ends ${ends.toFixed(1)}`); fail++;
  }
  // and with NO weighting the bands must be ~equal (guards against a tautology)
  const flat = drawTop(0);
  const fMid = bandAvg(flat, 0.4, 0.6), fEnds = (bandAvg(flat, 0, 0.1) + bandAvg(flat, 0.9, 1)) / 2;
  if (Math.abs(fMid - fEnds) > 8) {
    console.error(`FAIL center weighting off: bands differ (mid ${fMid.toFixed(1)}, ends ${fEnds.toFixed(1)})`); fail++;
  }
}

// --- engine: read() must return a per-bar spectrum array in [0,1] ---
if (typeof globalThis.AudioEngine !== "object" || !globalThis.AudioEngine.read) {
  console.error("FAIL: AudioEngine not exposed"); fail++;
} else {
  const out = globalThis.AudioEngine.read(1 / 60, true);   // simulation
  const isArr = out.spectrum && (Array.isArray(out.spectrum) || ArrayBuffer.isView(out.spectrum));
  if (!isArr || out.spectrum.length < 8) {
    console.error("FAIL engine: read() has no per-bar spectrum array"); fail++;
  } else if (!Array.from(out.spectrum).every(v => v >= 0 && v <= 1)) {
    console.error("FAIL engine: spectrum out of [0,1]"); fail++;
  } else {
    const distinct = new Set(Array.from(out.spectrum).map(v => v.toFixed(3))).size;
    if (distinct < 4) { console.error(`FAIL engine: simulated spectrum not varied (${distinct} values)`); fail++; }
  }
}

// --- I2: a loaded-but-PAUSED file must NOT show synthetic bars. The spectrum
// should be (near) silent, matching the band meters. ---
(async () => {
  let asyncFail = 0;
  try {
    const fakeFile = { arrayBuffer: async () => new ArrayBuffer(8) };
    await globalThis.AudioEngine.loadFile(fakeFile);   // -> usingFile = true
    globalThis.AudioEngine.pause();                    // -> playing = false
    // let the envelope decay to ~0
    for (let i = 0; i < 120; i++) globalThis.AudioEngine.read(1 / 60, false);
    const out = globalThis.AudioEngine.read(1 / 60, false);
    const peak = Math.max(...Array.from(out.spectrum));
    if (peak > 0.1) {
      console.error(`FAIL paused file: bars still moving (spectrum peak ${peak.toFixed(3)})`); asyncFail++;
    }
  } catch (e) {
    console.error(`FAIL paused-file test threw: ${e.message}`); asyncFail++;
  }
  fail += asyncFail;
  console.log(fail === 0 ? "ALL VARIANTS OK" : `${fail} FAILING VARIANTS`);
  process.exit(fail === 0 ? 0 : 1);
})();
