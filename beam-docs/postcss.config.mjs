/*
 * Tailwind runs through PostCSS rather than @tailwindcss/vite.
 *
 * The Vite plugin imports `vite` at runtime and resolves it to 7.3.1 (hoisted from beam-web),
 * while Astro drives the build with its own nested 6.4.1. It works, but the two copies' plugin
 * types are incompatible and `astro check` fails on the `vite.plugins` entry.
 * @tailwindcss/postcss does not import vite at all, so the mismatch cannot arise. Astro loads
 * this config automatically from the project root.
 */
export default {
	plugins: {
		"@tailwindcss/postcss": {},
	},
};
