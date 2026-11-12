const brl = new Intl.NumberFormat('pt-BR', { style: 'currency', currency: 'BRL' });
export const money = (c: number) => brl.format(c / 100);
export const usd = (c: number) => 'US$ ' + (c / 100).toFixed(2).replace('.', ',');
export const signed = (c: number) => (c > 0 ? '+' : c < 0 ? '−' : '') + money(Math.abs(c));
export const pct = (n: number) => n.toFixed(1).replace('.', ',') + '%';
export const floatTxt = (f: number) => f.toFixed(4).replace('.', ',');

export function ago(ts: number, now: number): string {
  const s = Math.max(0, now - ts);
  if (s < 60) return `há ${s} s`;
  if (s < 3600) return `há ${Math.floor(s / 60)} min`;
  if (s < 86400) return `há ${Math.floor(s / 3600)} h`;
  return `há ${Math.floor(s / 86400)} d`;
}

export function eta(h: number | null): string {
  if (h == null) return '—';
  if (h < 1) return '< 1 h';
  if (h < 48) return `~${Math.round(h)} h`;
  return `~${Math.round(h / 24)} dias`;
}

export function until(ts: number, now: number): string {
  const s = ts - now;
  if (s <= 0) return 'já liberada';
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `em ${d} d ${h} h`;
  if (h > 0) return `em ${h} h ${m} min`;
  return `em ${Math.max(1, m)} min`;
}

export const clock = (ts: number) => new Date(ts * 1000).toLocaleTimeString('pt-BR', { hour: '2-digit', minute: '2-digit' });
export const day = (ts: number) => new Date(ts * 1000).toLocaleDateString('pt-BR', { day: '2-digit', month: '2-digit' });

// "36,08", "R$ 1.234,56" ou "5" → centavos, sem ponto flutuante.
export function parseBRL(text: string): number | null {
  let t = text.replace(/R\$|\s/g, '');
  if (t.includes(',')) t = t.replace(/\./g, '');
  const m = /^(\d+)(?:[.,](\d{1,2}))?$/.exec(t);
  return m ? Number(m[1]) * 100 + Number((m[2] ?? '').padEnd(2, '0') || 0) : null;
}
export const centsToInput = (c: number) => (c / 100).toFixed(2).replace('.', ',');
