import { defineConfig, type PluginOption } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Os dados de exemplo só entram no `npm run dev`: no build o módulo nem é importado.
export default defineConfig(async ({ command }) => {
  const plugins: PluginOption[] = [svelte()];
  if (command === 'serve') plugins.push((await import('./dev/fixture')).fixture());
  return { plugins, base: './', build: { outDir: 'dist', emptyOutDir: false, target: 'es2022' } };
});
