export class ApiError extends Error {}

// Toda mutação leva Content-Type: application/json (o servidor recusa sem isso).
export async function api<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const mutates = method !== 'GET';
  let r: Response;
  try {
    r = await fetch(path, {
      method,
      headers: mutates ? { 'Content-Type': 'application/json' } : undefined,
      body: mutates ? JSON.stringify(body ?? {}) : undefined
    });
  } catch {
    throw new ApiError('Sem conexão com o Skin Sniper. Veja se o programa está aberto.');
  }
  if (!r.ok) {
    let msg = '';
    try {
      const j = await r.json();
      msg = j.error ?? j.message ?? '';
    } catch {}
    throw new ApiError(msg || `O Skin Sniper respondeu com erro (${r.status}).`);
  }
  return r.json();
}
