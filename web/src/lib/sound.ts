// Bip curto gerado na hora (sem arquivo). O navegador só libera som depois do primeiro toque na página.
let ctx: AudioContext | null = null;

export function unlockAudio() {
  ctx ??= new AudioContext();
  if (ctx.state === 'suspended') void ctx.resume();
}

export function beep() {
  if (!ctx || ctx.state !== 'running') return;
  [880, 1320].forEach((f, i) => {
    const t = ctx!.currentTime + i * 0.13;
    const o = ctx!.createOscillator();
    const g = ctx!.createGain();
    o.frequency.value = f;
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(0.18, t + 0.02);
    g.gain.exponentialRampToValueAtTime(0.0001, t + 0.12);
    o.connect(g).connect(ctx!.destination);
    o.start(t);
    o.stop(t + 0.14);
  });
}
